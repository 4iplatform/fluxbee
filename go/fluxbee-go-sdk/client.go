package sdk

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"log"
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

// receivedMessage is what the connection's reader hands the receiver: a message and its frame's
// size in bytes, or an error.
type receivedMessage struct {
	msg  Message
	size int
	err  error
}

type connectionState struct {
	mu         sync.RWMutex
	connected  bool
	routerName string
	fullName   string
	vpnID      uint32
}

type HiveFile struct {
	HiveID string `yaml:"hive_id"`
}

func (s *connectionState) SetConnected(value bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.connected = value
}

func (s *connectionState) SetRouterName(value string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.routerName = strings.TrimSpace(value)
}

func (s *connectionState) SetAnnounce(payload *NodeAnnouncePayload) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if payload == nil {
		return
	}
	s.connected = true
	s.routerName = strings.TrimSpace(payload.RouterName)
	s.fullName = strings.TrimSpace(payload.Name)
	s.vpnID = payload.VpnID
}

func (s *connectionState) IsConnected() bool {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.connected
}

func (s *connectionState) RouterName() string {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.routerName
}

func (s *connectionState) FullName() string {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.fullName
}

func (s *connectionState) VpnID() uint32 {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.vpnID
}

func connect(cfg NodeConfig) (*NodeSender, *NodeReceiver, error) {
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

	// The connection of the first handshake is the one the manager serves: a node says HELLO once
	// at start (FINDINGS A-183).
	conn, err := connectRouter(cfg.RouterSocket, baseName)
	if err != nil {
		close(tx)
		close(rx)
		return nil, nil, err
	}
	announce, err := handshakeWithin(conn, fullName, nodeID.String(), cfg.Version, handshakeTimeout)
	if err != nil {
		_ = conn.Close()
		close(tx)
		close(rx)
		return nil, nil, err
	}

	state.SetAnnounce(announce)
	go connectionManager(cfg, baseName, fullName, nodeID.String(), conn, tx, rx, state)

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

// MessageTooLargeError is what Send returns for a message whose frame is
// over the router's limit (maxFrameSize, 128 KiB). The message is not
// queued and the connection stays up. Mirror of Rust
// `NodeError::MessageTooLarge`.
type MessageTooLargeError struct {
	Size int
	Max  int
}

func (e *MessageTooLargeError) Error() string {
	if e == nil {
		return ""
	}
	return fmt.Sprintf("message too large: %d bytes (max %d)", e.Size, e.Max)
}

// MarshalMessage is msg as a frame carries it: its JSON without HTML
// escaping. json.Marshal writes <, > and & as <, > and &,
// inside a payload passed as raw JSON too, so a payload a node forwards as
// it received it went out changed (and longer).
func MarshalMessage(msg Message) ([]byte, error) {
	var buf bytes.Buffer
	encoder := json.NewEncoder(&buf)
	encoder.SetEscapeHTML(false)
	if err := encoder.Encode(msg); err != nil {
		return nil, err
	}
	return bytes.TrimSuffix(buf.Bytes(), []byte("\n")), nil
}

// Send queues msg for the router, with this node's entry in its cycle's trace (SendTraced).
func (s *NodeSender) Send(msg Message) error {
	return s.SendTraced(msg, TraceNote{})
}

// SendTraced queues msg for the router with this node's entry in its cycle's trace
// (docs/message-trace-v1.md): when it sends it, its own name, and what note adds, the destination
// by name and the actions it took. A message that is not the solution's carries no trace. A writer
// it names by ILK alone gets its name from the identity memory of this node's hive
// (docs/cognition-hands-v1.md §5). A message whose frame is over the limit goes with the text of its
// text/v1 payload as a blob, its content_ref in its place, when that makes it fit; a message that
// fits goes as it is, its text inline, however long (text_offload.go, FINDINGS A-264: only past the
// frame, where the Rust SDK offloads a text over about 62 KiB). Any other message over the limit is
// refused with *MessageTooLargeError before it is queued: queued, it would make the writer drop the
// connection. A full queue fails with "sender queue full".
func (s *NodeSender) SendTraced(msg Message, note TraceNote) error {
	WriteTraceEntry(&msg, s.FullName(), note)
	if hive := hiveOf(s.FullName()); hive != "" {
		fillWriterName(&msg, func(ilk string) (IlkFacts, bool) { return IlkFactsOf(hive, ilk) })
	}
	data, err := MarshalMessage(msg)
	if err != nil {
		return err
	}
	if len(data) > maxFrameSize {
		data = offloadTextOverTheFrame(textOffloadBlobRoot, &msg, data)
	}
	if len(data) > maxFrameSize {
		log.Printf(
			"fluxbee-go-sdk: FRAME_TOO_LARGE, message refused before sending; fix the sending node: frame_len=%d max=%d msg_type=%q msg=%q trace_id=%s",
			len(data), maxFrameSize, msg.Meta.MsgType, stringValue(msg.Meta.Msg), msg.Routing.TraceID,
		)
		return &MessageTooLargeError{Size: len(data), Max: maxFrameSize}
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
	if value := s.state.FullName(); value != "" {
		return value
	}
	return s.fullName
}

func (s *NodeSender) RouterName() string {
	return s.state.RouterName()
}

func (r *NodeReceiver) Recv(ctx context.Context) (Message, error) {
	item, err := r.recvItem(ctx)
	return item.msg, err
}

// recvItem is Recv with the frame's size, which the dispatcher's command queues count.
func (r *NodeReceiver) recvItem(ctx context.Context) (receivedMessage, error) {
	select {
	case <-ctx.Done():
		return receivedMessage{}, ctx.Err()
	case item, ok := <-r.rx:
		if !ok {
			r.state.SetConnected(false)
			return receivedMessage{}, fmt.Errorf("disconnected")
		}
		return item, item.err
	}
}

func (r *NodeReceiver) IsConnected() bool {
	return r.state.IsConnected()
}

func (r *NodeReceiver) UUID() string {
	return r.uuid
}

func (r *NodeReceiver) FullName() string {
	if value := r.state.FullName(); value != "" {
		return value
	}
	return r.fullName
}

func (r *NodeReceiver) VpnID() uint32 {
	if value := r.state.VpnID(); value != 0 {
		return value
	}
	return r.vpnID
}

func (r *NodeReceiver) RouterName() string {
	return r.state.RouterName()
}

// connectionManager serves first, the connection connect's handshake opened, and after it every
// connection it opens again, each with its own handshake.
func connectionManager(cfg NodeConfig, baseName, fullName, uuidValue string, first net.Conn, tx <-chan []byte, rx chan<- receivedMessage, state *connectionState) {
	conn := first
	for {
		if conn == nil {
			next, err := connectRouter(cfg.RouterSocket, baseName)
			if err != nil {
				state.SetConnected(false)
				time.Sleep(500 * time.Millisecond)
				continue
			}
			announce, err := handshakeWithin(next, fullName, uuidValue, cfg.Version, handshakeTimeout)
			if err != nil {
				_ = next.Close()
				state.SetConnected(false)
				time.Sleep(500 * time.Millisecond)
				continue
			}
			state.SetAnnounce(announce)
			conn = next
		}

		stop := make(chan struct{})
		done := make(chan struct{}, 2)
		go func() {
			defer func() { done <- struct{}{} }()
			for {
				frame, err := ReadFrame(conn)
				if err != nil {
					select {
					case <-stop:
					default:
						select {
						case rx <- receivedMessage{err: err}:
						default:
						}
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
				// The reader waits for room: the dispatcher reads the queue without ever waiting
				// itself (a full command channel refuses at once, FINDINGS A-329), so the wait is
				// short, and while it lasts the frames stay in the router's queue for this link,
				// which answers LINK_BUSY past its bound (FINDINGS A-222). Before 0.1.141 a frame
				// that found the queue full was dropped, and the dispatcher failed every pending
				// request as disconnected.
				select {
				case rx <- receivedMessage{msg: msg, size: len(frame)}:
				case <-stop:
					return
				}
			}
		}()
		go func() {
			defer func() { done <- struct{}{} }()
			for {
				select {
				case <-stop:
					return
				case frame := <-tx:
					if err := WriteFrame(conn, frame); err != nil {
						state.SetConnected(false)
						return
					}
				}
			}
		}()

		<-done
		state.SetConnected(false)
		close(stop)
		_ = conn.Close()
		<-done
		conn = nil
		time.Sleep(500 * time.Millisecond)
	}
}

// handshakeTimeout bounds a node's handshake with its router (FINDINGS A-183). The router answers a
// HELLO at once; one that took the connection and never answers would otherwise hold a reconnect
// forever.
const handshakeTimeout = 5 * time.Second

// handshakeWithin is PerformHandshake under a deadline on conn, cleared once the router answered.
func handshakeWithin(conn net.Conn, fullName, uuidValue, version string, limit time.Duration) (*NodeAnnouncePayload, error) {
	if err := conn.SetDeadline(time.Now().Add(limit)); err != nil {
		return nil, err
	}
	_, announce, err := PerformHandshake(conn, fullName, uuidValue, version)
	if err != nil {
		return nil, err
	}
	if err := conn.SetDeadline(time.Time{}); err != nil {
		return nil, err
	}
	return announce, nil
}

func PerformHandshake(conn net.Conn, fullName, uuidValue, version string) (*Message, *NodeAnnouncePayload, error) {
	hello, err := BuildHello(uuidValue, uuid.NewString(), NodeHelloPayload{
		UUID:    uuidValue,
		Name:    fullName,
		Version: version,
	})
	if err != nil {
		return nil, nil, err
	}
	data, err := MarshalMessage(hello)
	if err != nil {
		return nil, nil, err
	}
	if err := WriteFrame(conn, data); err != nil {
		return nil, nil, err
	}
	frame, err := ReadFrame(conn)
	if err != nil {
		return nil, nil, err
	}
	var msg Message
	if err := json.Unmarshal(frame, &msg); err != nil {
		return nil, nil, err
	}
	if msg.Meta.MsgType != SYSTEMKind || stringValue(msg.Meta.Msg) != MSGAnnounce {
		return nil, nil, fmt.Errorf("expected ANNOUNCE")
	}
	var announce NodeAnnouncePayload
	if err := json.Unmarshal(msg.Payload, &announce); err != nil {
		return nil, nil, err
	}
	return &msg, &announce, nil
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

// LoadHiveID exposes the hive_id parsed from hive.yaml for callers that need it
// outside Connect (e.g. to call WaitForSelfSystemIlkID before sender wiring).
func LoadHiveID(configDir string) (string, error) {
	return loadHiveID(configDir)
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
	candidates, err := RouterSocketCandidates(routerSocket, nodeName)
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

func RouterSocketCandidates(path, nodeName string) ([]string, error) {
	return routerSocketCandidates(path, nodeName)
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
