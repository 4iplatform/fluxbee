package sdk

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/google/uuid"
	"gopkg.in/yaml.v3"
)

type NodeUuidMode string

const (
	NodeUuidPersistent NodeUuidMode = "persistent"
	NodeUuidEphemeral  NodeUuidMode = "ephemeral"
)

type NodeConfig struct {
	Name               string
	RouterSocket       string
	UUIDPersistenceDir string
	UUIDMode           NodeUuidMode
	ConfigDir          string
	Version            string
}

type NodeSender struct {
	uuid     string
	fullName string
	tx       chan []byte
	state    *connectionState
}

type NodeReceiver struct {
	uuid     string
	fullName string
	vpnID    uint32
	rx       chan receivedMessage
	state    *connectionState
}

type receivedMessage struct {
	msg Message
	err error
}

type connectionState struct {
	mu        sync.RWMutex
	connected bool
}

type HiveFile struct {
	HiveID string `yaml:"hive_id"`
}

func (s *connectionState) SetConnected(value bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.connected = value
}

func (s *connectionState) IsConnected() bool {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.connected
}

func Connect(cfg NodeConfig) (*NodeSender, *NodeReceiver, error) {
	hiveID, err := loadHiveID(cfg.ConfigDir)
	if err != nil {
		return nil, nil, err
	}
	fullName, baseName := normalizeNodeName(cfg.Name, hiveID)
	nodeID, err := resolveUUID(cfg.UUIDMode, cfg.UUIDPersistenceDir, baseName)
	if err != nil {
		return nil, nil, err
	}
	state := &connectionState{connected: false}
	tx := make(chan []byte, 256)
	rx := make(chan receivedMessage, 256)

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	announce, err := connectAndHandshake(ctx, cfg, baseName, fullName, nodeID.String())
	if err != nil {
		close(tx)
		close(rx)
		return nil, nil, err
	}

	state.SetConnected(true)
	go connectionManager(cfg, baseName, fullName, nodeID.String(), tx, rx, state)

	sender := &NodeSender{
		uuid:     nodeID.String(),
		fullName: announce.Name,
		tx:       tx,
		state:    state,
	}
	receiver := &NodeReceiver{
		uuid:     nodeID.String(),
		fullName: announce.Name,
		vpnID:    announce.VpnID,
		rx:       rx,
		state:    state,
	}
	return sender, receiver, nil
}

func (s *NodeSender) Send(msg Message) error {
	data, err := json.Marshal(msg)
	if err != nil {
		return err
	}
	select {
	case s.tx <- data:
		return nil
	default:
		return fmt.Errorf("sender queue full")
	}
}

func (s *NodeSender) Close() error {
	traceID := uuid.NewString()
	msg, err := BuildWithdraw(s.uuid, ResolveDestination(), traceID, s.uuid)
	if err != nil {
		return err
	}
	return s.Send(msg)
}

func (s *NodeSender) UUID() string {
	return s.uuid
}

func (s *NodeSender) FullName() string {
	return s.fullName
}

func (r *NodeReceiver) Recv(ctx context.Context) (Message, error) {
	select {
	case <-ctx.Done():
		return Message{}, ctx.Err()
	case item, ok := <-r.rx:
		if !ok {
			r.state.SetConnected(false)
			return Message{}, fmt.Errorf("disconnected")
		}
		return item.msg, item.err
	}
}

func (r *NodeReceiver) IsConnected() bool {
	return r.state.IsConnected()
}

func (r *NodeReceiver) UUID() string {
	return r.uuid
}

func (r *NodeReceiver) FullName() string {
	return r.fullName
}

func (r *NodeReceiver) VpnID() uint32 {
	return r.vpnID
}

func connectionManager(cfg NodeConfig, baseName, fullName, uuidValue string, tx <-chan []byte, rx chan<- receivedMessage, state *connectionState) {
	for {
		conn, err := connectRouter(cfg.RouterSocket, baseName)
		if err != nil {
			state.SetConnected(false)
			time.Sleep(500 * time.Millisecond)
			continue
		}
		announce, err := performHandshake(conn, fullName, uuidValue, cfg.Version)
		if err != nil {
			_ = conn.Close()
			state.SetConnected(false)
			time.Sleep(500 * time.Millisecond)
			continue
		}
		state.SetConnected(true)
		_ = announce

		done := make(chan struct{}, 2)
		go func() {
			defer func() { done <- struct{}{} }()
			for {
				frame, err := ReadFrame(conn)
				if err != nil {
					select {
					case rx <- receivedMessage{err: err}:
					default:
					}
					state.SetConnected(false)
					return
				}
				var msg Message
				if err := json.Unmarshal(frame, &msg); err != nil {
					select {
					case rx <- receivedMessage{err: err}:
					default:
					}
					continue
				}
				select {
				case rx <- receivedMessage{msg: msg}:
				default:
					select {
					case rx <- receivedMessage{err: fmt.Errorf("receiver queue full")}:
					default:
					}
				}
			}
		}()
		go func() {
			defer func() { done <- struct{}{} }()
			for frame := range tx {
				if err := WriteFrame(conn, frame); err != nil {
					state.SetConnected(false)
					return
				}
			}
		}()

		<-done
		state.SetConnected(false)
		_ = conn.Close()
		time.Sleep(500 * time.Millisecond)
	}
}

func connectAndHandshake(ctx context.Context, cfg NodeConfig, baseName, fullName, uuidValue string) (*NodeAnnouncePayload, error) {
	conn, err := connectRouter(cfg.RouterSocket, baseName)
	if err != nil {
		return nil, err
	}
	defer conn.Close()
	type result struct {
		payload *NodeAnnouncePayload
		err     error
	}
	done := make(chan result, 1)
	go func() {
		payload, err := performHandshake(conn, fullName, uuidValue, cfg.Version)
		done <- result{payload: payload, err: err}
	}()
	select {
	case <-ctx.Done():
		return nil, ctx.Err()
	case out := <-done:
		return out.payload, out.err
	}
}

func performHandshake(conn net.Conn, fullName, uuidValue, version string) (*NodeAnnouncePayload, error) {
	hello, err := BuildHello(uuidValue, uuid.NewString(), NodeHelloPayload{
		UUID:    uuidValue,
		Name:    fullName,
		Version: version,
	})
	if err != nil {
		return nil, err
	}
	data, err := json.Marshal(hello)
	if err != nil {
		return nil, err
	}
	if err := WriteFrame(conn, data); err != nil {
		return nil, err
	}
	frame, err := ReadFrame(conn)
	if err != nil {
		return nil, err
	}
	var msg Message
	if err := json.Unmarshal(frame, &msg); err != nil {
		return nil, err
	}
	if msg.Meta.MsgType != SYSTEMKind || stringValue(msg.Meta.Msg) != MSGAnnounce {
		return nil, fmt.Errorf("expected ANNOUNCE")
	}
	var announce NodeAnnouncePayload
	if err := json.Unmarshal(msg.Payload, &announce); err != nil {
		return nil, err
	}
	return &announce, nil
}

func loadHiveID(configDir string) (string, error) {
	data, err := os.ReadFile(filepath.Join(configDir, "hive.yaml"))
	if err != nil {
		return "", err
	}
	var hive HiveFile
	if err := yaml.Unmarshal(data, &hive); err != nil {
		return "", err
	}
	if hive.HiveID == "" {
		return "", fmt.Errorf("hive_id missing")
	}
	return hive.HiveID, nil
}

func normalizeNodeName(name, hiveID string) (string, string) {
	if strings.Contains(name, "@") {
		parts := strings.SplitN(name, "@", 2)
		return name, parts[0]
	}
	return fmt.Sprintf("%s@%s", name, hiveID), name
}

func resolveUUID(mode NodeUuidMode, dir, baseName string) (uuid.UUID, error) {
	if mode == NodeUuidEphemeral {
		return uuid.New(), nil
	}
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return uuid.Nil, err
	}
	path := filepath.Join(dir, fmt.Sprintf("%s.uuid", baseName))
	if data, err := os.ReadFile(path); err == nil {
		return uuid.Parse(strings.TrimSpace(string(data)))
	}
	id := uuid.New()
	if err := os.WriteFile(path, []byte(id.String()), 0o644); err != nil {
		return uuid.Nil, err
	}
	return id, nil
}

func connectRouter(routerSocket, nodeName string) (net.Conn, error) {
	candidates, err := routerSocketCandidates(routerSocket, nodeName)
	if err != nil {
		return nil, err
	}
	var lastErr error
	for _, candidate := range candidates {
		conn, err := net.Dial("unix", candidate)
		if err == nil {
			return conn, nil
		}
		lastErr = err
	}
	if lastErr != nil {
		return nil, lastErr
	}
	return nil, fmt.Errorf("no router sockets found")
}

func routerSocketCandidates(path, nodeName string) ([]string, error) {
	info, err := os.Stat(path)
	if err == nil && info.IsDir() {
		entries, err := os.ReadDir(path)
		if err != nil {
			return nil, err
		}
		var sockets []string
		for _, entry := range entries {
			if entry.IsDir() {
				continue
			}
			name := entry.Name()
			if !strings.HasSuffix(name, ".sock") || strings.HasPrefix(name, "irp-") {
				continue
			}
			sockets = append(sockets, filepath.Join(path, name))
		}
		sort.Strings(sockets)
		if len(sockets) == 0 {
			return nil, fmt.Errorf("no router sockets found")
		}
		start := int(fnv1a64([]byte(nodeName)) % uint64(len(sockets)))
		ordered := make([]string, 0, len(sockets))
		for offset := 0; offset < len(sockets); offset++ {
			idx := (start + offset) % len(sockets)
			ordered = append(ordered, sockets[idx])
		}
		return ordered, nil
	}
	if err != nil && !os.IsNotExist(err) {
		return nil, err
	}
	return []string{path}, nil
}

func fnv1a64(bytes []byte) uint64 {
	var hash uint64 = 0xcbf29ce484222325
	for _, b := range bytes {
		hash ^= uint64(b)
		hash *= 0x100000001b3
	}
	return hash
}
