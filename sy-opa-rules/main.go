//go:build linux

package main

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"context"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log"
	"net"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"sync/atomic"
	"time"
	"unsafe"

	fluxbeesdk "github.com/4iplatform/json-router/sy-opa-rules/sdk"
	"github.com/google/uuid"
	"github.com/open-policy-agent/opa/compile"
	"golang.org/x/sys/unix"
	"gopkg.in/yaml.v3"
)

const (
	configDir           = "/etc/fluxbee"
	stateDir            = "/var/lib/fluxbee/opa"
	nodesDir            = "/var/lib/fluxbee/state/nodes"
	routerSockDir       = "/var/run/fluxbee/routers"
	lockPath            = "/var/run/fluxbee/sy-opa-rules.lock"
	opaShmPrefix        = "/jsr-opa-"
	routerShmPrefix     = "/jsr-"
	opaMagic            = 0x4A534F50 // "JSOP"
	opaVersion          = 1
	opaMaxWasmSize      = 4 * 1024 * 1024
	opaStatusOK         = 0
	opaStatusError      = 1
	opaStatusLoading    = 2
	opaHeaderSize       = 192
	defaultEntrypoint   = "router/target"
	defaultNodeBaseName = "SY.opa.rules"
)

const (
	routerMagic   = 0x4A535352 // "JSSR"
	routerVersion = 2
)

type HiveConfig struct {
	HiveID string `yaml:"hive_id"`
}

type Meta struct {
	Type             string          `json:"type"`
	Msg              string          `json:"msg,omitempty"`
	SrcIlk           string          `json:"src_ilk,omitempty"`
	DstIlk           string          `json:"dst_ilk,omitempty"`
	Ich              string          `json:"ich,omitempty"`
	ThreadID         string          `json:"thread_id,omitempty"`
	Ctx              string          `json:"ctx,omitempty"`
	Scope            string          `json:"scope,omitempty"`
	Target           string          `json:"target,omitempty"`
	Action           string          `json:"action,omitempty"`
	ActionClass      string          `json:"action_class,omitempty"`
	ActionResult     string          `json:"action_result,omitempty"`
	ResultOrigin     string          `json:"result_origin,omitempty"`
	ResultDetailCode string          `json:"result_detail_code,omitempty"`
	Priority         string          `json:"priority,omitempty"`
	Context          json.RawMessage `json:"context,omitempty"`
}

type Routing struct {
	Src     string `json:"src"`
	Dst     any    `json:"dst"`
	Ttl     uint8  `json:"ttl"`
	TraceID string `json:"trace_id"`
}

type Message struct {
	Routing Routing         `json:"routing"`
	Meta    Meta            `json:"meta"`
	Payload json.RawMessage `json:"payload"`
}

type NodeHelloPayload struct {
	UUID    string `json:"uuid"`
	Name    string `json:"name"`
	Version string `json:"version"`
}

type NodeAnnouncePayload struct {
	UUID       string `json:"uuid"`
	Name       string `json:"name"`
	Status     string `json:"status"`
	VpnID      uint32 `json:"vpn_id"`
	RouterName string `json:"router_name"`
}

type OpaConfigPayload struct {
	Rego       string `json:"rego"`
	Entrypoint string `json:"entrypoint"`
}

type ConfigChangedPayload struct {
	Subsystem string            `json:"subsystem"`
	Action    string            `json:"action"`
	AutoApply *bool             `json:"auto_apply,omitempty"`
	Version   uint64            `json:"version"`
	Config    *OpaConfigPayload `json:"config,omitempty"`
}

type CompileRequest struct {
	Rego       string `json:"rego"`
	Entrypoint string `json:"entrypoint"`
	Version    uint64 `json:"version"`
}

type VersionRequest struct {
	Version uint64 `json:"version"`
}

type PolicyMetadata struct {
	Version     uint64 `json:"version"`
	Hash        string `json:"hash"`
	Entrypoint  string `json:"entrypoint"`
	CompiledAt  string `json:"compiled_at"`
	WasmSize    int    `json:"wasm_size_bytes,omitempty"`
	ErrorDetail string `json:"error_detail,omitempty"`
}

type OpaError struct {
	Code   string
	Detail string
}

func (e OpaError) Error() string {
	return e.Detail
}

func classifyOpaError(err error) (string, string) {
	if err == nil {
		return "SHM_ERROR", "unknown error"
	}
	var oe OpaError
	if errors.As(err, &oe) {
		return oe.Code, oe.Detail
	}
	var oePtr *OpaError
	if errors.As(err, &oePtr) && oePtr != nil {
		return oePtr.Code, oePtr.Detail
	}
	return "SHM_ERROR", err.Error()
}

type OpaRegion struct {
	name     string
	fd       int
	mmap     []byte
	seqPtr   *uint64
	ownerID  uuid.UUID
	ownerPID uint32
	mu       sync.Mutex
}

type RouterHeader struct {
	Magic            uint32
	Version          uint32
	RouterUUID       [16]byte
	OwnerPID         uint32
	Pad0             uint32
	OwnerStartTime   uint64
	Generation       uint64
	Seq              uint64
	NodeCount        uint32
	NodeMax          uint32
	CreatedAt        uint64
	UpdatedAt        uint64
	Heartbeat        uint64
	HiveID           [64]byte
	HiveIDLen        uint16
	RouterName       [64]byte
	RouterNameLen    uint16
	IsGateway        uint8
	FlagsReserved    [3]byte
	OpaPolicyVersion uint64
	OpaLoadStatus    uint8
	OpaPad           [7]byte
	Reserved         [6]byte
}

type Service struct {
	hiveID     string
	nodeUUID   uuid.UUID
	nodeName   string
	routerConn *RouterClient
	opaRegion  *OpaRegion

	lastError string
}

type RouterStatus struct {
	RouterID      string `json:"router_id"`
	PolicyVersion uint64 `json:"policy_version"`
	LoadStatus    uint8  `json:"load_status"`
	Status        string `json:"status"`
	Error         string `json:"error,omitempty"`
}

func main() {
	log.SetFlags(log.LstdFlags | log.Lmicroseconds)

	hiveID, err := loadHiveID()
	if err != nil {
		log.Fatalf("failed to load hive.yaml: %v", err)
	}
	if err := ensureDirs(); err != nil {
		log.Fatalf("failed to create dirs: %v", err)
	}
	lockFile, err := os.OpenFile(lockPath, os.O_CREATE|os.O_RDWR, 0o644)
	if err != nil {
		log.Fatalf("failed to open lock file: %v", err)
	}
	defer lockFile.Close()
	if err := unix.Flock(int(lockFile.Fd()), unix.LOCK_EX|unix.LOCK_NB); err != nil {
		log.Fatalf("sy-opa-rules already running: %v", err)
	}

	nodeUUID, err := loadOrCreateUUID(nodesDir, defaultNodeBaseName)
	if err != nil {
		log.Fatalf("failed to load uuid: %v", err)
	}
	nodeName := fmt.Sprintf("%s@%s", defaultNodeBaseName, hiveID)

	opaRegion, err := openOrCreateOpaRegion(opaShmPrefix+hiveID, nodeUUID)
	if err != nil {
		log.Fatalf("failed to open opa shm: %v", err)
	}

	service := &Service{
		hiveID:    hiveID,
		nodeUUID:  nodeUUID,
		nodeName:  nodeName,
		opaRegion: opaRegion,
	}

	if err := service.loadCurrentPolicy(); err != nil {
		log.Printf("failed to load current policy: %v", err)
	}

	routerClient := NewRouterClient(nodeUUID, nodeName)
	service.routerConn = routerClient
	go routerClient.Run(service)

	go service.heartbeatLoop()

	select {}
}

func ensureDirs() error {
	paths := []string{
		filepath.Join(stateDir, "current"),
		filepath.Join(stateDir, "staged"),
		filepath.Join(stateDir, "backup"),
		nodesDir,
		"/var/run/fluxbee",
		routerSockDir,
	}
	for _, path := range paths {
		if err := os.MkdirAll(path, 0o755); err != nil {
			return err
		}
	}
	return nil
}

func loadHiveID() (string, error) {
	path := filepath.Join(configDir, "hive.yaml")
	data, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	var cfg HiveConfig
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return "", err
	}
	if cfg.HiveID == "" {
		return "", errors.New("hive_id missing")
	}
	return cfg.HiveID, nil
}

func loadOrCreateUUID(dir, base string) (uuid.UUID, error) {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return uuid.Nil, err
	}
	path := filepath.Join(dir, fmt.Sprintf("%s.uuid", base))
	if data, err := os.ReadFile(path); err == nil {
		return uuid.Parse(strings.TrimSpace(string(data)))
	}
	id := uuid.New()
	if err := os.WriteFile(path, []byte(id.String()), 0o644); err != nil {
		return uuid.Nil, err
	}
	return id, nil
}

func openOrCreateOpaRegion(name string, owner uuid.UUID) (*OpaRegion, error) {
	filePath := filepath.Join("/dev/shm", strings.TrimPrefix(name, "/"))
	fd, err := openFileFd(filePath, unix.O_RDWR, 0o666)
	if err == nil {
		_ = unix.Fchmod(fd, 0o666)
		if err := ensureShmSize(fd, opaHeaderSize+opaMaxWasmSize); err == nil {
			mmap, err := unix.Mmap(fd, 0, opaHeaderSize+opaMaxWasmSize, unix.PROT_READ|unix.PROT_WRITE, unix.MAP_SHARED)
			if err == nil {
				if isValidOpaRegion(mmap) {
					return &OpaRegion{
						name:     name,
						fd:       fd,
						mmap:     mmap,
						seqPtr:   (*uint64)(unsafe.Pointer(&mmap[8])),
						ownerID:  owner,
						ownerPID: uint32(os.Getpid()),
					}, nil
				}
				_ = unix.Munmap(mmap)
			}
		}
		_ = unix.Close(fd)
		_ = os.Remove(filePath)
	}

	fd, err = openFileFd(filePath, unix.O_RDWR|unix.O_CREAT|unix.O_EXCL, 0o666)
	if err != nil {
		return nil, err
	}
	_ = unix.Fchmod(fd, 0o666)
	if err := ensureShmSize(fd, opaHeaderSize+opaMaxWasmSize); err != nil {
		_ = unix.Close(fd)
		_ = os.Remove(filePath)
		return nil, err
	}
	mmap, err := unix.Mmap(fd, 0, opaHeaderSize+opaMaxWasmSize, unix.PROT_READ|unix.PROT_WRITE, unix.MAP_SHARED)
	if err != nil {
		_ = unix.Close(fd)
		_ = os.Remove(filePath)
		return nil, err
	}
	for i := range mmap {
		mmap[i] = 0
	}
	region := &OpaRegion{
		name:     name,
		fd:       fd,
		mmap:     mmap,
		seqPtr:   (*uint64)(unsafe.Pointer(&mmap[8])),
		ownerID:  owner,
		ownerPID: uint32(os.Getpid()),
	}
	region.initHeader()
	return region, nil
}

func ensureShmSize(fd int, size int) error {
	var stat unix.Stat_t
	if err := unix.Fstat(fd, &stat); err != nil {
		return err
	}
	if int(stat.Size) >= size {
		return nil
	}
	return unix.Ftruncate(fd, int64(size))
}

func openFileFd(path string, flags int, perm uint32) (int, error) {
	fd, err := unix.Open(path, flags, perm)
	if err != nil {
		return -1, err
	}
	return fd, nil
}

func isValidOpaRegion(mmap []byte) bool {
	if len(mmap) < opaHeaderSize {
		return false
	}
	magic := binary.LittleEndian.Uint32(mmap[0:4])
	version := binary.LittleEndian.Uint32(mmap[4:8])
	return magic == opaMagic && version == opaVersion
}

func (r *OpaRegion) initHeader() {
	r.mu.Lock()
	defer r.mu.Unlock()
	binary.LittleEndian.PutUint32(r.mmap[0:4], opaMagic)
	binary.LittleEndian.PutUint32(r.mmap[4:8], opaVersion)
	atomic.StoreUint64(r.seqPtr, 0)
	r.writeHeader(0, nil, "", opaStatusOK)
}

func (r *OpaRegion) updateHeartbeat() {
	r.mu.Lock()
	defer r.mu.Unlock()
	atomic.AddUint64(r.seqPtr, 1)
	now := uint64(time.Now().UnixMilli())
	binary.LittleEndian.PutUint64(r.mmap[72:80], now)
	atomic.AddUint64(r.seqPtr, 1)
}

func (r *OpaRegion) writeStatus(status uint8) {
	r.mu.Lock()
	defer r.mu.Unlock()
	atomic.AddUint64(r.seqPtr, 1)
	r.mmap[80] = status
	binary.LittleEndian.PutUint64(r.mmap[72:80], uint64(time.Now().UnixMilli()))
	atomic.AddUint64(r.seqPtr, 1)
}

func (r *OpaRegion) writeHeader(version uint64, wasm []byte, entrypoint string, status uint8) {
	atomic.AddUint64(r.seqPtr, 1)

	binary.LittleEndian.PutUint64(r.mmap[16:24], version)
	if wasm == nil {
		binary.LittleEndian.PutUint32(r.mmap[24:28], 0)
	} else {
		binary.LittleEndian.PutUint32(r.mmap[24:28], uint32(len(wasm)))
	}
	binary.LittleEndian.PutUint64(r.mmap[64:72], uint64(time.Now().UnixMilli()))
	binary.LittleEndian.PutUint64(r.mmap[72:80], uint64(time.Now().UnixMilli()))
	r.mmap[80] = status

	for i := 0; i < 32; i++ {
		r.mmap[32+i] = 0
	}
	if wasm != nil {
		hash := sha256.Sum256(wasm)
		copy(r.mmap[32:64], hash[:])
	}

	for i := 0; i < 64; i++ {
		r.mmap[82+i] = 0
	}
	entryBytes := []byte(entrypoint)
	if len(entryBytes) > 64 {
		entryBytes = entryBytes[:64]
	}
	copy(r.mmap[82:82+len(entryBytes)], entryBytes)
	binary.LittleEndian.PutUint16(r.mmap[146:148], uint16(len(entryBytes)))

	copy(r.mmap[148:164], r.ownerID[:])
	binary.LittleEndian.PutUint32(r.mmap[164:168], r.ownerPID)

	if wasm != nil {
		copy(r.mmap[opaHeaderSize:], wasm)
	}

	atomic.AddUint64(r.seqPtr, 1)
}

func (r *OpaRegion) writePolicy(version uint64, wasm []byte, entrypoint string) {
	r.writeStatus(opaStatusLoading)
	r.mu.Lock()
	defer r.mu.Unlock()
	if len(wasm) > opaMaxWasmSize {
		log.Printf("opa wasm too large: %d bytes", len(wasm))
		r.writeHeader(version, nil, entrypoint, opaStatusError)
		return
	}
	r.writeHeader(version, wasm, entrypoint, opaStatusOK)
}

func (r *OpaRegion) policyHash() string {
	hash := r.mmap[32:64]
	return "sha256:" + hex.EncodeToString(hash)
}

func listRouterStatuses() []RouterStatus {
	entries, err := os.ReadDir(routerSockDir)
	if err != nil {
		return nil
	}
	var out []RouterStatus
	for _, entry := range entries {
		if entry.IsDir() {
			continue
		}
		name := entry.Name()
		if strings.HasPrefix(name, "irp-") || !strings.HasSuffix(name, ".sock") {
			continue
		}
		routerID := strings.TrimSuffix(name, ".sock")
		shmName := routerShmPrefix + routerID
		status := readRouterStatus(routerID, shmName)
		out = append(out, status)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].RouterID < out[j].RouterID })
	return out
}

func readRouterStatus(routerID, shmName string) RouterStatus {
	path := filepath.Join("/dev/shm", strings.TrimPrefix(shmName, "/"))
	fd, err := unix.Open(path, unix.O_RDONLY, 0)
	if err != nil {
		return RouterStatus{RouterID: routerID, Status: "error", Error: err.Error()}
	}
	defer unix.Close(fd)
	headerSize := int(unsafe.Sizeof(RouterHeader{}))
	mmap, err := unix.Mmap(fd, 0, headerSize, unix.PROT_READ, unix.MAP_SHARED)
	if err != nil {
		return RouterStatus{RouterID: routerID, Status: "error", Error: err.Error()}
	}
	defer unix.Munmap(mmap)

	if len(mmap) < 8 {
		return RouterStatus{RouterID: routerID, Status: "error", Error: "header too small"}
	}
	magic := binary.LittleEndian.Uint32(mmap[0:4])
	version := binary.LittleEndian.Uint32(mmap[4:8])
	if magic != routerMagic || version != routerVersion {
		return RouterStatus{
			RouterID: routerID,
			Status:   "error",
			Error:    fmt.Sprintf("invalid header magic=0x%x version=%d", magic, version),
		}
	}

	seqOffset := int(unsafe.Offsetof(RouterHeader{}.Seq))
	policyOffset := int(unsafe.Offsetof(RouterHeader{}.OpaPolicyVersion))
	statusOffset := int(unsafe.Offsetof(RouterHeader{}.OpaLoadStatus))

	for i := 0; i < 100; i++ {
		seq1 := binary.LittleEndian.Uint64(mmap[seqOffset : seqOffset+8])
		if seq1&1 != 0 {
			time.Sleep(1 * time.Millisecond)
			continue
		}
		policy := binary.LittleEndian.Uint64(mmap[policyOffset : policyOffset+8])
		load := mmap[statusOffset]
		seq2 := binary.LittleEndian.Uint64(mmap[seqOffset : seqOffset+8])
		if seq1 != seq2 {
			continue
		}
		status := "ok"
		if load == opaStatusLoading {
			status = "loading"
		} else if load == opaStatusError {
			status = "error"
		}
		return RouterStatus{
			RouterID:      routerID,
			PolicyVersion: policy,
			LoadStatus:    load,
			Status:        status,
		}
	}
	return RouterStatus{RouterID: routerID, Status: "error", Error: "seqlock timeout"}
}

func (s *Service) loadCurrentPolicy() error {
	metaPath := filepath.Join(stateDir, "current", "metadata.json")
	wasmPath := filepath.Join(stateDir, "current", "policy.wasm")
	meta := PolicyMetadata{}
	if metaBytes, err := os.ReadFile(metaPath); err == nil {
		_ = json.Unmarshal(metaBytes, &meta)
	}
	wasm, err := os.ReadFile(wasmPath)
	if err != nil {
		s.opaRegion.writeStatus(opaStatusOK)
		return nil
	}
	if err := validateWasm(wasm); err != nil {
		s.lastError = err.Error()
		s.opaRegion.writeStatus(opaStatusError)
		log.Printf("opa wasm error: %v", err)
		return err
	}
	if !wasmHasExport(wasm, "opa_eval") {
		s.lastError = "opa wasm missing export: opa_eval"
		s.opaRegion.writeStatus(opaStatusError)
		log.Printf("opa wasm error: %s", s.lastError)
		return errors.New(s.lastError)
	}
	entrypoint := meta.Entrypoint
	if entrypoint == "" {
		entrypoint = defaultEntrypoint
	}
	version := meta.Version
	if version == 0 {
		version = 1
	}
	hash := sha256.Sum256(wasm)
	meta.Hash = "sha256:" + hex.EncodeToString(hash[:])
	meta.WasmSize = len(wasm)
	s.opaRegion.writePolicy(version, wasm, entrypoint)
	_ = writeMetadata(metaPath, meta)
	log.Printf("loaded current policy version=%d", version)
	return nil
}

func (s *Service) handleMessage(msg Message) {
	switch msg.Meta.Type {
	case "system":
		if msg.Meta.Msg == fluxbeesdk.MSGNodeStatusGet {
			s.handleNodeStatusGet(msg)
			return
		}
		if msg.Meta.Msg == fluxbeesdk.MSGConfigGet {
			s.handleNodeConfigGet(msg)
			return
		}
		if msg.Meta.Msg == fluxbeesdk.MSGConfigSet {
			s.handleNodeConfigSet(msg)
			return
		}
		if msg.Meta.Msg == "CONFIG_CHANGED" {
			var payload ConfigChangedPayload
			if err := json.Unmarshal(msg.Payload, &payload); err != nil {
				log.Printf("invalid config payload: %v", err)
				return
			}
			if payload.Subsystem != "opa" {
				return
			}
			autoApply := false
			if payload.AutoApply != nil {
				autoApply = *payload.AutoApply
			}
			s.handleOpaAction(msg.Routing.Src, payload.Action, payload.Version, payload.Config, autoApply, true)
		}
	case "command":
		s.handleCommand(msg)
	case "query":
		s.handleQuery(msg)
	}
}

func (s *Service) handleNodeStatusGet(msg Message) {
	resp, err := fluxbeesdk.BuildDefaultNodeStatusResponse(
		toSDKMessage(msg),
		s.nodeUUID.String(),
		"HEALTHY",
	)
	if err != nil {
		log.Printf("failed to build node status response: %v", err)
		return
	}
	s.routerConn.Send(fromSDKMessage(resp))
}

func (s *Service) handleNodeConfigGet(msg Message) {
	reqMsg, err := fluxbeesdk.ParseNodeConfigRequest(toSDKMessage(msg))
	if err != nil || reqMsg == nil || reqMsg.Get == nil {
		s.sendNodeConfigResponse(msg, map[string]any{
			"ok":        false,
			"node_name": s.nodeName,
			"state":     "error",
			"error": map[string]any{
				"code":   "INVALID_CONFIG_GET",
				"detail": errorString(err, "invalid CONFIG_GET request"),
			},
		})
		return
	}
	req := reqMsg.Get
	currentMeta, _ := readMetadata(filepath.Join(stateDir, "current", "metadata.json"))
	stagedMeta, _ := readMetadata(filepath.Join(stateDir, "staged", "metadata.json"))
	backupMeta, _ := readMetadata(filepath.Join(stateDir, "backup", "metadata.json"))
	state := "empty"
	if currentMeta.Version > 0 || currentMeta.Hash != "" {
		state = "configured"
	}
	response := map[string]any{
		"ok":             true,
		"node_name":      s.nodeName,
		"state":          state,
		"schema_version": 1,
		"config_version": currentMeta.Version,
		"effective_config": map[string]any{
			"current": buildPolicySnapshot(currentMeta),
			"staged":  buildPolicySnapshot(stagedMeta),
			"backup":  buildPolicySnapshot(backupMeta),
			"router": map[string]any{
				"routers": listRouterStatuses(),
			},
		},
		"contract": map[string]any{
			"supports":              []string{fluxbeesdk.MSGConfigGet, fluxbeesdk.MSGConfigSet},
			"target":                fluxbeesdk.NodeConfigControlTarget,
			"schema_version":        1,
			"apply_modes":           []string{fluxbeesdk.NodeConfigApplyModeReplace},
			"config_schema":         "opa_control_v1",
			"supported_operations":  []string{"check", "compile", "compile_apply", "apply", "rollback"},
			"requested_node_name":   req.NodeName,
			"preserves_opa_actions": true,
			"preserves_opa_queries": true,
		},
	}
	s.sendNodeConfigResponse(msg, response)
}

func (s *Service) handleNodeConfigSet(msg Message) {
	reqMsg, err := fluxbeesdk.ParseNodeConfigRequest(toSDKMessage(msg))
	if err != nil || reqMsg == nil || reqMsg.Set == nil {
		s.sendNodeConfigResponse(msg, map[string]any{
			"ok":        false,
			"node_name": s.nodeName,
			"state":     "error",
			"error": map[string]any{
				"code":   "INVALID_CONFIG_SET",
				"detail": errorString(err, "invalid CONFIG_SET request"),
			},
		})
		return
	}
	req := reqMsg.Set
	if req.ApplyMode != "" && req.ApplyMode != fluxbeesdk.NodeConfigApplyModeReplace {
		s.sendNodeConfigResponse(msg, map[string]any{
			"ok":        false,
			"node_name": s.nodeName,
			"state":     "error",
			"error": map[string]any{
				"code":   "UNSUPPORTED_APPLY_MODE",
				"detail": fmt.Sprintf("unsupported apply_mode: %s", req.ApplyMode),
			},
		})
		return
	}

	configMap, ok := req.Config.(map[string]any)
	if !ok {
		s.sendNodeConfigResponse(msg, map[string]any{
			"ok":        false,
			"node_name": s.nodeName,
			"state":     "error",
			"error": map[string]any{
				"code":   "INVALID_CONFIG_SET",
				"detail": "config must be a JSON object",
			},
		})
		return
	}

	operation, _ := configMap["operation"].(string)
	operation = strings.TrimSpace(strings.ToLower(operation))
	if operation == "" {
		operation = "check"
	}
	version := req.ConfigVersion
	if rawVersion, ok := configMap["version"].(float64); ok && uint64(rawVersion) != 0 {
		version = uint64(rawVersion)
	}
	rego, _ := configMap["rego"].(string)
	entrypoint, _ := configMap["entrypoint"].(string)
	autoApply, _ := configMap["auto_apply"].(bool)

	var opaCfg *OpaConfigPayload
	if rego != "" || entrypoint != "" {
		opaCfg = &OpaConfigPayload{
			Rego:       rego,
			Entrypoint: entrypoint,
		}
	}

	handled, err := s.handleOpaAction(msg.Routing.Src, operation, version, opaCfg, autoApply, false)
	if err != nil {
		s.sendNodeConfigResponse(msg, map[string]any{
			"ok":             false,
			"node_name":      s.nodeName,
			"state":          "error",
			"schema_version": req.SchemaVersion,
			"config_version": version,
			"error": map[string]any{
				"code":   inferOpaErrorCode(err),
				"detail": err.Error(),
			},
		})
		return
	}
	if !handled {
		s.sendNodeConfigResponse(msg, map[string]any{
			"ok":             false,
			"node_name":      s.nodeName,
			"state":          "error",
			"schema_version": req.SchemaVersion,
			"config_version": version,
			"error": map[string]any{
				"code":   "UNSUPPORTED_OPERATION",
				"detail": fmt.Sprintf("unsupported OPA config operation: %s", operation),
			},
		})
		return
	}

	currentMeta, _ := readMetadata(filepath.Join(stateDir, "current", "metadata.json"))
	if operation == "check" || operation == "compile" || operation == "compile_apply" {
		stagedMeta, _ := readMetadata(filepath.Join(stateDir, "staged", "metadata.json"))
		s.sendNodeConfigResponse(msg, map[string]any{
			"ok":             true,
			"node_name":      s.nodeName,
			"state":          "configured",
			"schema_version": req.SchemaVersion,
			"config_version": stagedMeta.Version,
			"effective_config": map[string]any{
				"staged": buildPolicySnapshot(stagedMeta),
			},
		})
		return
	}

	s.sendNodeConfigResponse(msg, map[string]any{
		"ok":             true,
		"node_name":      s.nodeName,
		"state":          "configured",
		"schema_version": req.SchemaVersion,
		"config_version": currentMeta.Version,
		"effective_config": map[string]any{
			"current": buildPolicySnapshot(currentMeta),
		},
	})
}

func (s *Service) sendNodeConfigResponse(request Message, payload map[string]any) {
	resp, err := fluxbeesdk.BuildNodeConfigResponseMessage(toSDKMessage(request), s.nodeUUID.String(), payload)
	if err != nil {
		log.Printf("failed to build CONFIG_RESPONSE: %v", err)
		return
	}
	s.routerConn.Send(fromSDKMessage(resp))
}

func buildPolicySnapshot(meta PolicyMetadata) map[string]any {
	return map[string]any{
		"version":         meta.Version,
		"hash":            meta.Hash,
		"entrypoint":      meta.Entrypoint,
		"compiled_at":     meta.CompiledAt,
		"wasm_size_bytes": meta.WasmSize,
		"error_detail":    meta.ErrorDetail,
	}
}

func inferOpaErrorCode(err error) string {
	if err == nil {
		return ""
	}
	var oe OpaError
	if errors.As(err, &oe) {
		return oe.Code
	}
	var oePtr *OpaError
	if errors.As(err, &oePtr) && oePtr != nil {
		return oePtr.Code
	}
	return "OPA_CONFIG_ERROR"
}

func errorString(err error, fallback string) string {
	if err == nil {
		return fallback
	}
	return err.Error()
}

func toSDKMessage(msg Message) *fluxbeesdk.Message {
	return &fluxbeesdk.Message{
		Routing: fluxbeesdk.Routing{
			Src:     msg.Routing.Src,
			Dst:     toSDKDestination(msg.Routing.Dst),
			TTL:     msg.Routing.Ttl,
			TraceID: msg.Routing.TraceID,
		},
		Meta: fluxbeesdk.Meta{
			MsgType:          msg.Meta.Type,
			Msg:              stringPtr(msg.Meta.Msg),
			SrcIlk:           stringPtr(msg.Meta.SrcIlk),
			DstIlk:           stringPtr(msg.Meta.DstIlk),
			Ich:              stringPtr(msg.Meta.Ich),
			ThreadID:         stringPtr(msg.Meta.ThreadID),
			Ctx:              stringPtr(msg.Meta.Ctx),
			Scope:            stringPtr(msg.Meta.Scope),
			Target:           stringPtr(msg.Meta.Target),
			Action:           stringPtr(msg.Meta.Action),
			ActionClass:      stringPtr(msg.Meta.ActionClass),
			ActionResult:     stringPtr(msg.Meta.ActionResult),
			ResultOrigin:     stringPtr(msg.Meta.ResultOrigin),
			ResultDetailCode: stringPtr(msg.Meta.ResultDetailCode),
			Priority:         stringPtr(msg.Meta.Priority),
			Context:          cloneRawMessage(msg.Meta.Context),
		},
		Payload: cloneRawMessage(msg.Payload),
	}
}

func fromSDKMessage(msg fluxbeesdk.Message) Message {
	return Message{
		Routing: Routing{
			Src:     msg.Routing.Src,
			Dst:     fromSDKDestination(msg.Routing.Dst),
			Ttl:     msg.Routing.TTL,
			TraceID: msg.Routing.TraceID,
		},
		Meta: Meta{
			Type:             msg.Meta.MsgType,
			Msg:              derefString(msg.Meta.Msg),
			SrcIlk:           derefString(msg.Meta.SrcIlk),
			DstIlk:           derefString(msg.Meta.DstIlk),
			Ich:              derefString(msg.Meta.Ich),
			ThreadID:         derefString(msg.Meta.ThreadID),
			Ctx:              derefString(msg.Meta.Ctx),
			Scope:            derefString(msg.Meta.Scope),
			Target:           derefString(msg.Meta.Target),
			Action:           derefString(msg.Meta.Action),
			ActionClass:      derefString(msg.Meta.ActionClass),
			ActionResult:     derefString(msg.Meta.ActionResult),
			ResultOrigin:     derefString(msg.Meta.ResultOrigin),
			ResultDetailCode: derefString(msg.Meta.ResultDetailCode),
			Priority:         derefString(msg.Meta.Priority),
			Context:          cloneRawMessage(msg.Meta.Context),
		},
		Payload: cloneRawMessage(msg.Payload),
	}
}

func toSDKDestination(dst any) fluxbeesdk.Destination {
	switch value := dst.(type) {
	case nil:
		return fluxbeesdk.ResolveDestination()
	case string:
		if value == "broadcast" {
			return fluxbeesdk.BroadcastDestination()
		}
		return fluxbeesdk.UnicastDestination(value)
	default:
		return fluxbeesdk.ResolveDestination()
	}
}

func fromSDKDestination(dst fluxbeesdk.Destination) any {
	switch {
	case dst.IsBroadcast():
		return "broadcast"
	case dst.IsResolve():
		return nil
	default:
		return dst.Value()
	}
}

func stringPtr(value string) *string {
	if strings.TrimSpace(value) == "" {
		return nil
	}
	copy := value
	return &copy
}

func derefString(value *string) string {
	if value == nil {
		return ""
	}
	return *value
}

func cloneRawMessage(value json.RawMessage) json.RawMessage {
	if len(value) == 0 {
		return nil
	}
	out := make([]byte, len(value))
	copy(out, value)
	return out
}

func (s *Service) handleCommand(msg Message) {
	action := strings.ToLower(msg.Meta.Action)
	switch action {
	case "compile_policy":
		var req CompileRequest
		if err := json.Unmarshal(msg.Payload, &req); err != nil {
			s.sendCommandResponse(msg, action, map[string]any{
				"status":       "error",
				"error_code":   "COMPILE_ERROR",
				"error_detail": err.Error(),
			})
			return
		}
		_, err := s.handleOpaAction(msg.Routing.Src, "compile", req.Version, &OpaConfigPayload{
			Rego:       req.Rego,
			Entrypoint: req.Entrypoint,
		}, false, false)
		if err != nil {
			return
		}
		s.sendCommandResponse(msg, action, map[string]any{
			"status":  "ok",
			"version": req.Version,
			"staged":  true,
		})
	case "apply_policy":
		var req VersionRequest
		_ = json.Unmarshal(msg.Payload, &req)
		_, err := s.handleOpaAction(msg.Routing.Src, "apply", req.Version, nil, false, false)
		if err != nil {
			return
		}
		s.sendCommandResponse(msg, action, map[string]any{
			"status":  "ok",
			"version": req.Version,
		})
	case "rollback_policy":
		_, err := s.handleOpaAction(msg.Routing.Src, "rollback", 0, nil, false, false)
		if err != nil {
			return
		}
		s.sendCommandResponse(msg, action, map[string]any{
			"status": "ok",
		})
	}
}

func (s *Service) handleQuery(msg Message) {
	action := strings.ToLower(msg.Meta.Action)
	log.Printf("query received action=%s src=%s", action, msg.Routing.Src)
	switch action {
	case "get_policy":
		meta, rego := readCurrentPolicy()
		resp := map[string]any{
			"status":      "ok",
			"hive":        s.hiveID,
			"version":     meta.Version,
			"rego":        rego,
			"hash":        meta.Hash,
			"compiled_at": meta.CompiledAt,
			"entrypoint":  meta.Entrypoint,
		}
		s.sendQueryResponse(msg, action, resp)
	case "get_status":
		current, _ := readMetadata(filepath.Join(stateDir, "current", "metadata.json"))
		staged, _ := readMetadata(filepath.Join(stateDir, "staged", "metadata.json"))
		resp := map[string]any{
			"hive":            s.hiveID,
			"current_version": current.Version,
			"current_hash":    current.Hash,
			"staged_version":  staged.Version,
			"status":          "ok",
			"last_error":      s.lastError,
			"wasm_size_bytes": current.WasmSize,
			"routers":         listRouterStatuses(),
		}
		s.sendQueryResponse(msg, action, resp)
	}
}

func (s *Service) handleOpaAction(src string, action string, version uint64, cfg *OpaConfigPayload, autoApply bool, broadcast bool) (bool, error) {
	action = strings.ToLower(action)
	switch action {
	case "compile", "compile_apply":
		if cfg == nil || cfg.Rego == "" {
			return s.respondConfigError(src, action, version, "COMPILE_ERROR", "rego missing", broadcast)
		}
		entrypoint := cfg.Entrypoint
		if entrypoint == "" {
			entrypoint = defaultEntrypoint
		}
		wasm, hash, compileMs, err := compileRego(cfg.Rego, entrypoint)
		if err != nil {
			return s.respondConfigError(src, action, version, "COMPILE_ERROR", err.Error(), broadcast)
		}
		meta := PolicyMetadata{
			Version:    version,
			Hash:       hash,
			Entrypoint: entrypoint,
			CompiledAt: time.Now().UTC().Format(time.RFC3339),
			WasmSize:   len(wasm),
		}
		if err := writePolicyFiles(filepath.Join(stateDir, "staged"), wasm, meta, cfg.Rego); err != nil {
			return s.respondConfigError(src, action, version, "SHM_ERROR", err.Error(), broadcast)
		}
		if action == "compile_apply" || autoApply {
			if err := s.applyPolicy(version); err != nil {
				code, detail := classifyOpaError(err)
				return s.respondConfigError(src, "apply", version, code, detail, broadcast)
			}
		}
		if broadcast {
			s.sendConfigResponse(src, action, version, "ok", meta, compileMs)
		}
		return true, nil
	case "apply":
		if err := s.applyPolicy(version); err != nil {
			code, detail := classifyOpaError(err)
			return s.respondConfigError(src, action, version, code, detail, broadcast)
		}
		if broadcast {
			s.sendConfigResponse(src, action, version, "ok", PolicyMetadata{Version: version}, 0)
		}
		return true, nil
	case "rollback":
		if err := s.rollbackPolicy(); err != nil {
			code, detail := classifyOpaError(err)
			return s.respondConfigError(src, action, version, code, detail, broadcast)
		}
		if broadcast {
			s.sendConfigResponse(src, action, version, "ok", PolicyMetadata{Version: version}, 0)
		}
		return true, nil
	case "check":
		if cfg == nil || cfg.Rego == "" {
			return s.respondConfigError(src, action, version, "COMPILE_ERROR", "rego missing", broadcast)
		}
		entrypoint := cfg.Entrypoint
		if entrypoint == "" {
			entrypoint = defaultEntrypoint
		}
		wasm, hash, compileMs, err := compileRego(cfg.Rego, entrypoint)
		if err != nil {
			return s.respondConfigError(src, action, version, "COMPILE_ERROR", err.Error(), broadcast)
		}
		meta := PolicyMetadata{
			Version:    version,
			Hash:       hash,
			Entrypoint: entrypoint,
			CompiledAt: time.Now().UTC().Format(time.RFC3339),
			WasmSize:   len(wasm),
		}
		if err := writePolicyFiles(filepath.Join(stateDir, "staged"), wasm, meta, cfg.Rego); err != nil {
			return s.respondConfigError(src, action, version, "SHM_ERROR", err.Error(), broadcast)
		}
		if broadcast {
			s.sendConfigResponse(src, action, version, "ok", meta, compileMs)
		}
		return true, nil
	}
	return false, nil
}

func (s *Service) applyPolicy(version uint64) error {
	stagedDir := filepath.Join(stateDir, "staged")
	stagedMeta := filepath.Join(stagedDir, "metadata.json")
	stagedWasm := filepath.Join(stagedDir, "policy.wasm")
	stagedRego := filepath.Join(stagedDir, "policy.rego")
	wasm, err := os.ReadFile(stagedWasm)
	if err != nil {
		return OpaError{Code: "NOTHING_STAGED", Detail: "staged policy missing"}
	}
	regoBytes, _ := os.ReadFile(stagedRego)
	rego := string(regoBytes)
	meta, _ := readMetadata(stagedMeta)
	entrypoint := meta.Entrypoint
	if entrypoint == "" {
		entrypoint = defaultEntrypoint
	}
	if version != 0 && meta.Version != 0 && version != meta.Version {
		return OpaError{Code: "VERSION_MISMATCH", Detail: fmt.Sprintf("version mismatch: staged=%d requested=%d", meta.Version, version)}
	}
	if version == 0 {
		version = meta.Version
	}
	if err := validateWasm(wasm); err != nil {
		s.opaRegion.writeStatus(opaStatusError)
		return OpaError{Code: "COMPILE_ERROR", Detail: err.Error()}
	}
	if !wasmHasExport(wasm, "opa_eval") {
		s.opaRegion.writeStatus(opaStatusError)
		return OpaError{Code: "COMPILE_ERROR", Detail: "opa wasm missing export: opa_eval"}
	}
	hash := sha256.Sum256(wasm)
	meta.Version = version
	meta.Hash = "sha256:" + hex.EncodeToString(hash[:])
	meta.Entrypoint = entrypoint
	meta.CompiledAt = time.Now().UTC().Format(time.RFC3339)
	meta.WasmSize = len(wasm)

	backupDir := filepath.Join(stateDir, "backup")
	currentDir := filepath.Join(stateDir, "current")
	_ = copyDir(currentDir, backupDir)
	if err := writePolicyFiles(currentDir, wasm, meta, rego); err != nil {
		return OpaError{Code: "SHM_ERROR", Detail: err.Error()}
	}
	s.opaRegion.writePolicy(version, wasm, entrypoint)
	s.broadcastOpaReload(version, "sha256:"+hex.EncodeToString(hash[:]))
	log.Printf("applied opa policy version=%d", version)
	return nil
}

func (s *Service) rollbackPolicy() error {
	backupDir := filepath.Join(stateDir, "backup")
	backupMeta := filepath.Join(backupDir, "metadata.json")
	backupWasm := filepath.Join(backupDir, "policy.wasm")
	backupRego := filepath.Join(backupDir, "policy.rego")
	wasm, err := os.ReadFile(backupWasm)
	if err != nil {
		return OpaError{Code: "NO_BACKUP", Detail: "backup policy missing"}
	}
	regoBytes, _ := os.ReadFile(backupRego)
	rego := string(regoBytes)
	meta, _ := readMetadata(backupMeta)
	entrypoint := meta.Entrypoint
	if entrypoint == "" {
		entrypoint = defaultEntrypoint
	}
	if err := validateWasm(wasm); err != nil {
		s.opaRegion.writeStatus(opaStatusError)
		return OpaError{Code: "COMPILE_ERROR", Detail: err.Error()}
	}
	if !wasmHasExport(wasm, "opa_eval") {
		s.opaRegion.writeStatus(opaStatusError)
		return OpaError{Code: "COMPILE_ERROR", Detail: "opa wasm missing export: opa_eval"}
	}
	currentDir := filepath.Join(stateDir, "current")
	hash := sha256.Sum256(wasm)
	meta.Hash = "sha256:" + hex.EncodeToString(hash[:])
	meta.CompiledAt = time.Now().UTC().Format(time.RFC3339)
	meta.WasmSize = len(wasm)
	if err := writePolicyFiles(currentDir, wasm, meta, rego); err != nil {
		return OpaError{Code: "SHM_ERROR", Detail: err.Error()}
	}
	s.opaRegion.writePolicy(meta.Version, wasm, entrypoint)
	s.broadcastOpaReload(meta.Version, meta.Hash)
	log.Printf("rollback applied version=%d", meta.Version)
	return nil
}

func (s *Service) respondConfigError(src, action string, version uint64, code, detail string, broadcast bool) (bool, error) {
	s.lastError = detail
	if broadcast {
		payload := map[string]any{
			"subsystem":    "opa",
			"action":       action,
			"version":      version,
			"status":       "error",
			"hive":         s.hiveID,
			"error_code":   code,
			"error_detail": detail,
		}
		s.sendSystemMessage(src, "CONFIG_RESPONSE", payload)
	} else {
		s.sendCommandError(action, version, code, detail)
	}
	return false, errors.New(detail)
}

func (s *Service) sendConfigResponse(dst, action string, version uint64, status string, meta PolicyMetadata, compileMs int64) {
	payload := map[string]any{
		"subsystem":       "opa",
		"action":          action,
		"version":         version,
		"status":          status,
		"hive":            s.hiveID,
		"compile_time_ms": compileMs,
		"wasm_size_bytes": meta.WasmSize,
		"hash":            meta.Hash,
	}
	log.Printf("config response action=%s version=%d hash=%s", action, version, meta.Hash)
	s.sendSystemMessage(dst, "CONFIG_RESPONSE", payload)
}

func (s *Service) sendCommandResponse(msg Message, action string, payload map[string]any) {
	resp := Message{
		Routing: Routing{
			Src:     s.nodeUUID.String(),
			Dst:     msg.Routing.Src,
			Ttl:     16,
			TraceID: uuid.New().String(),
		},
		Meta: Meta{
			Type:   "command_response",
			Action: action,
		},
		Payload: mustJSON(payload),
	}
	s.routerConn.Send(resp)
}

func (s *Service) sendCommandError(action string, version uint64, code, detail string) {
	payload := map[string]any{
		"status":       "error",
		"error_code":   code,
		"error_detail": detail,
		"version":      version,
	}
	resp := Message{
		Routing: Routing{
			Src:     s.nodeUUID.String(),
			Dst:     nil,
			Ttl:     16,
			TraceID: uuid.New().String(),
		},
		Meta: Meta{
			Type:   "command_response",
			Action: action,
		},
		Payload: mustJSON(payload),
	}
	if s.routerConn != nil {
		if dst := s.routerConn.LastPeer(); dst != "" {
			resp.Routing.Dst = dst
		}
	}
	if resp.Routing.Dst == nil {
		return
	}
	s.routerConn.Send(resp)
}

func (s *Service) sendQueryResponse(msg Message, action string, payload map[string]any) {
	resp := Message{
		Routing: Routing{
			Src:     s.nodeUUID.String(),
			Dst:     msg.Routing.Src,
			Ttl:     16,
			TraceID: uuid.New().String(),
		},
		Meta: Meta{
			Type:   "query_response",
			Action: action,
		},
		Payload: mustJSON(payload),
	}
	if version, ok := payload["version"]; ok {
		log.Printf("query response action=%s version=%v", action, version)
	} else {
		log.Printf("query response action=%s", action)
	}
	s.routerConn.Send(resp)
}

func (s *Service) sendSystemMessage(dst, msg string, payload map[string]any) {
	resp := Message{
		Routing: Routing{
			Src:     s.nodeUUID.String(),
			Dst:     dst,
			Ttl:     16,
			TraceID: uuid.New().String(),
		},
		Meta: Meta{
			Type: "system",
			Msg:  msg,
		},
		Payload: mustJSON(payload),
	}
	s.routerConn.Send(resp)
}

func (s *Service) broadcastOpaReload(version uint64, hash string) {
	payload := map[string]any{
		"version": version,
		"hash":    hash,
	}
	msg := Message{
		Routing: Routing{
			Src:     s.nodeUUID.String(),
			Dst:     "broadcast",
			Ttl:     2,
			TraceID: uuid.New().String(),
		},
		Meta: Meta{
			Type: "system",
			Msg:  "OPA_RELOAD",
		},
		Payload: mustJSON(payload),
	}
	s.routerConn.Send(msg)
}

func (s *Service) heartbeatLoop() {
	ticker := time.NewTicker(5 * time.Second)
	defer ticker.Stop()
	for range ticker.C {
		s.opaRegion.updateHeartbeat()
	}
}

type RouterClient struct {
	nodeUUID uuid.UUID
	nodeName string
	nodeBase string
	tx       chan []byte
	mu       sync.Mutex
	lastDst  string
}

func NewRouterClient(nodeUUID uuid.UUID, nodeName string) *RouterClient {
	base := nodeName
	if parts := strings.Split(nodeName, "@"); len(parts) > 1 {
		base = parts[0]
	}
	return &RouterClient{
		nodeUUID: nodeUUID,
		nodeName: nodeName,
		nodeBase: base,
		tx:       make(chan []byte, 256),
	}
}

func (c *RouterClient) LastPeer() string {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.lastDst
}

func (c *RouterClient) Run(service *Service) {
	backoff := 100 * time.Millisecond
	for {
		c.drainTxQueue()
		conn, err := c.connect()
		if err != nil {
			log.Printf("connect failed: %v", err)
			time.Sleep(backoff)
			backoff = nextBackoff(backoff)
			continue
		}
		if err := c.handshake(conn); err != nil {
			log.Printf("handshake failed: %v", err)
			conn.Close()
			time.Sleep(backoff)
			backoff = nextBackoff(backoff)
			continue
		}
		backoff = 100 * time.Millisecond
		if err := c.runConn(conn, service); err != nil {
			log.Printf("router connection error: %v", err)
		}
		conn.Close()
		time.Sleep(backoff)
		backoff = nextBackoff(backoff)
	}
}

func (c *RouterClient) connect() (net.Conn, error) {
	candidates, err := routerSocketCandidates(routerSockDir, c.nodeBase)
	if err != nil {
		return nil, err
	}
	var lastErr error
	for _, sock := range candidates {
		conn, err := net.Dial("unix", sock)
		if err == nil {
			log.Printf("connected to router socket=%s name=%s", sock, c.nodeName)
			return conn, nil
		}
		lastErr = err
	}
	if lastErr != nil {
		return nil, lastErr
	}
	return nil, fmt.Errorf("no router sockets found")
}

func (c *RouterClient) handshake(conn net.Conn) error {
	hello, err := fluxbeesdk.BuildHello(c.nodeUUID.String(), uuid.New().String(), fluxbeesdk.NodeHelloPayload{
		UUID:    c.nodeUUID.String(),
		Name:    c.nodeName,
		Version: "1.0",
	})
	if err != nil {
		return err
	}
	if err := writeFrame(conn, mustJSON(hello)); err != nil {
		return err
	}
	log.Printf("waiting for ANNOUNCE")
	frame, err := readFrame(conn)
	if err != nil {
		return err
	}
	var msg Message
	if err := json.Unmarshal(frame, &msg); err != nil {
		return err
	}
	if msg.Meta.Type != fluxbeesdk.SYSTEMKind || msg.Meta.Msg != fluxbeesdk.MSGAnnounce {
		return fmt.Errorf("unexpected announce")
	}
	var payload fluxbeesdk.NodeAnnouncePayload
	_ = json.Unmarshal(msg.Payload, &payload)
	log.Printf("connected as %s (vpn=%d router=%s)", payload.Name, payload.VpnID, payload.RouterName)
	c.mu.Lock()
	c.lastDst = msg.Routing.Src
	c.mu.Unlock()
	return nil
}

func (c *RouterClient) readLoop(conn net.Conn, service *Service) error {
	for {
		frame, err := readFrame(conn)
		if err != nil {
			return err
		}
		var msg Message
		if err := json.Unmarshal(frame, &msg); err != nil {
			log.Printf("invalid message: %v", err)
			continue
		}
		if msg.Routing.Src != "" {
			c.mu.Lock()
			c.lastDst = msg.Routing.Src
			c.mu.Unlock()
		}
		service.handleMessage(msg)
	}
}

func (c *RouterClient) runConn(conn net.Conn, service *Service) error {
	errCh := make(chan error, 2)
	done := make(chan struct{})
	go func() {
		errCh <- c.readLoop(conn, service)
	}()
	go func() {
		errCh <- c.writeLoop(conn, done)
	}()
	err := <-errCh
	close(done)
	return err
}

func (c *RouterClient) writeLoop(conn net.Conn, done <-chan struct{}) error {
	for {
		select {
		case <-done:
			return nil
		case frame, ok := <-c.tx:
			if !ok {
				return io.EOF
			}
			if err := writeFrame(conn, frame); err != nil {
				return err
			}
		}
	}
}

func (c *RouterClient) Send(msg Message) {
	data := mustJSON(msg)
	c.tx <- data
}

func readFrame(conn net.Conn) ([]byte, error) {
	return fluxbeesdk.ReadFrame(conn)
}

func writeFrame(conn net.Conn, payload []byte) error {
	return fluxbeesdk.WriteFrame(conn, payload)
}

func routerSocketCandidates(dir string, nodeName string) ([]string, error) {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, err
	}
	var sockets []string
	for _, entry := range entries {
		if entry.IsDir() {
			continue
		}
		name := entry.Name()
		if strings.HasPrefix(name, "irp-") {
			continue
		}
		if !strings.HasSuffix(name, ".sock") {
			continue
		}
		sockets = append(sockets, filepath.Join(dir, name))
	}
	if len(sockets) == 0 {
		return nil, fmt.Errorf("no router sockets found")
	}
	sort.Strings(sockets)
	idx := int(fnv1a64([]byte(nodeName)) % uint64(len(sockets)))
	ordered := make([]string, 0, len(sockets))
	ordered = append(ordered, sockets[idx:]...)
	ordered = append(ordered, sockets[:idx]...)
	return ordered, nil
}

func (c *RouterClient) drainTxQueue() {
	for {
		select {
		case <-c.tx:
		default:
			return
		}
	}
}

func nextBackoff(current time.Duration) time.Duration {
	next := current * 2
	if next > 30*time.Second {
		return 30 * time.Second
	}
	return next
}

func fnv1a64(data []byte) uint64 {
	const (
		offset64 = 14695981039346656037
		prime64  = 1099511628211
	)
	hash := uint64(offset64)
	for _, b := range data {
		hash ^= uint64(b)
		hash *= prime64
	}
	return hash
}

func mustJSON(v any) []byte {
	data, _ := json.Marshal(v)
	return data
}

func compileRego(rego string, entrypoint string) ([]byte, string, int64, error) {
	tmpDir, err := os.MkdirTemp("", "opa-compile")
	if err != nil {
		return nil, "", 0, err
	}
	defer os.RemoveAll(tmpDir)
	regoPath := filepath.Join(tmpDir, "policy.rego")
	if err := os.WriteFile(regoPath, []byte(rego), 0o644); err != nil {
		return nil, "", 0, err
	}
	start := time.Now()
	var buf bytes.Buffer
	compiler := compile.New().
		WithTarget(compile.TargetWasm).
		WithEntrypoints(entrypoint).
		WithPaths(regoPath).
		WithOutput(&buf)
	if err := compiler.Build(context.Background()); err != nil {
		return nil, "", 0, err
	}
	wasm := buf.Bytes()
	wasm, normErr := normalizeWasmBytes(wasm, entrypoint)
	if normErr != nil {
		return nil, "", 0, normErr
	}
	if err := validateWasm(wasm); err != nil {
		return nil, "", 0, err
	}
	if !wasmHasExport(wasm, "opa_eval") {
		return nil, "", 0, fmt.Errorf("opa wasm missing export: opa_eval")
	}
	if len(wasm) > opaMaxWasmSize {
		return nil, "", 0, fmt.Errorf("wasm too large (%d bytes)", len(wasm))
	}
	hash := sha256.Sum256(wasm)
	return wasm, "sha256:" + hex.EncodeToString(hash[:]), time.Since(start).Milliseconds(), nil
}

func normalizeWasmBytes(data []byte, entrypoint string) ([]byte, error) {
	if len(data) >= 4 && bytes.Equal(data[:4], []byte{0x1f, 0x8b, 0x08, 0x00}) {
		reader, err := gzip.NewReader(bytes.NewReader(data))
		if err != nil {
			return nil, fmt.Errorf("invalid gzip wasm: %w", err)
		}
		defer reader.Close()
		decoded, err := io.ReadAll(reader)
		if err != nil {
			return nil, fmt.Errorf("failed to decode gzip wasm: %w", err)
		}
		data = decoded
	}
	if len(data) >= 4 && bytes.Equal(data[:4], []byte{0x00, 0x61, 0x73, 0x6d}) {
		return data, nil
	}
	if wasm, err := extractWasmFromBundle(data, entrypoint); err == nil {
		return wasm, nil
	}
	return data, nil
}

func extractWasmFromBundle(data []byte, entrypoint string) ([]byte, error) {
	tr := tar.NewReader(bytes.NewReader(data))
	var fallback []byte
	var entrypointMatch []byte
	var policyMatch []byte
	var opaEvalMatch []byte
	debug := os.Getenv("OPA_DEBUG_BUNDLE") == "1"
	entrypoint = strings.TrimPrefix(entrypoint, "/")
	entrypoint = strings.TrimSuffix(entrypoint, "/")
	entrypointFile := ""
	if entrypoint != "" {
		parts := strings.Split(entrypoint, "/")
		entrypointFile = parts[len(parts)-1] + ".wasm"
	}
	for {
		hdr, err := tr.Next()
		if err != nil {
			if errors.Is(err, io.EOF) {
				break
			}
			return nil, err
		}
		if hdr.Typeflag != tar.TypeReg {
			continue
		}
		if strings.HasSuffix(hdr.Name, ".wasm") {
			wasm, err := io.ReadAll(tr)
			if err != nil {
				return nil, err
			}
			hasEval := wasmHasExport(wasm, "opa_eval")
			if debug {
				log.Printf("bundle wasm entry=%s size=%d opa_eval=%v", hdr.Name, len(wasm), hasEval)
			}
			if hasEval {
				opaEvalMatch = wasm
			}
			if entrypoint != "" && strings.HasSuffix(hdr.Name, entrypoint+".wasm") {
				entrypointMatch = wasm
			} else if entrypointFile != "" && strings.HasSuffix(hdr.Name, entrypointFile) {
				entrypointMatch = wasm
			} else if strings.HasSuffix(hdr.Name, "policy.wasm") {
				policyMatch = wasm
			} else if fallback == nil {
				fallback = wasm
			}
		}
	}
	if opaEvalMatch != nil {
		if debug {
			log.Printf("bundle selected wasm by export opa_eval")
		}
		return opaEvalMatch, nil
	}
	if entrypointMatch != nil {
		if debug {
			log.Printf("bundle selected wasm by entrypoint")
		}
		return entrypointMatch, nil
	}
	if policyMatch != nil {
		if debug {
			log.Printf("bundle selected wasm policy.wasm")
		}
		return policyMatch, nil
	}
	if fallback != nil {
		if debug {
			log.Printf("bundle selected wasm fallback")
		}
		return fallback, nil
	}
	return nil, fmt.Errorf("no wasm entry found in bundle")
}

func wasmHasExport(wasm []byte, name string) bool {
	if len(wasm) < 8 {
		return false
	}
	if !bytes.Equal(wasm[:4], []byte{0x00, 0x61, 0x73, 0x6d}) {
		return false
	}
	off := 8
	for off < len(wasm) {
		sectionID := wasm[off]
		off++
		secLen, n := readU32LEB(wasm[off:])
		if n == 0 {
			return false
		}
		off += n
		if off+int(secLen) > len(wasm) {
			return false
		}
		if sectionID == 7 {
			sec := wasm[off : off+int(secLen)]
			count, n := readU32LEB(sec)
			if n == 0 {
				return false
			}
			cursor := n
			for i := 0; i < int(count); i++ {
				nameLen, n := readU32LEB(sec[cursor:])
				if n == 0 {
					return false
				}
				cursor += n
				if cursor+int(nameLen) > len(sec) {
					return false
				}
				expName := string(sec[cursor : cursor+int(nameLen)])
				cursor += int(nameLen)
				if cursor >= len(sec) {
					return false
				}
				cursor++ // kind
				_, n = readU32LEB(sec[cursor:])
				if n == 0 {
					return false
				}
				cursor += n
				if expName == name {
					return true
				}
			}
			return false
		}
		off += int(secLen)
	}
	return false
}

func readU32LEB(data []byte) (uint32, int) {
	var result uint32
	var shift uint
	for i := 0; i < len(data); i++ {
		b := data[i]
		result |= uint32(b&0x7f) << shift
		if b&0x80 == 0 {
			return result, i + 1
		}
		shift += 7
		if shift >= 32 {
			break
		}
	}
	return 0, 0
}

func validateWasm(wasm []byte) error {
	if len(wasm) < 8 {
		return fmt.Errorf("invalid wasm output: too short (%d bytes)", len(wasm))
	}
	if !bytes.Equal(wasm[:4], []byte{0x00, 0x61, 0x73, 0x6d}) {
		return fmt.Errorf("invalid wasm magic: %x", wasm[:4])
	}
	version := binary.LittleEndian.Uint32(wasm[4:8])
	if version != 1 {
		return fmt.Errorf("unsupported wasm version: %d", version)
	}
	return nil
}

func writePolicyFiles(dir string, wasm []byte, meta PolicyMetadata, rego string) error {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(dir, "policy.wasm"), wasm, 0o644); err != nil {
		return err
	}
	if rego != "" {
		if err := os.WriteFile(filepath.Join(dir, "policy.rego"), []byte(rego), 0o644); err != nil {
			return err
		}
	}
	return writeMetadata(filepath.Join(dir, "metadata.json"), meta)
}

func writeMetadata(path string, meta PolicyMetadata) error {
	data, err := json.MarshalIndent(meta, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(path, data, 0o644)
}

func readMetadata(path string) (PolicyMetadata, error) {
	var meta PolicyMetadata
	data, err := os.ReadFile(path)
	if err != nil {
		return meta, err
	}
	err = json.Unmarshal(data, &meta)
	return meta, err
}

func readCurrentPolicy() (PolicyMetadata, string) {
	meta, _ := readMetadata(filepath.Join(stateDir, "current", "metadata.json"))
	regoBytes, _ := os.ReadFile(filepath.Join(stateDir, "current", "policy.rego"))
	return meta, string(regoBytes)
}

func copyDir(src, dst string) error {
	if _, err := os.Stat(src); err != nil {
		return nil
	}
	if err := os.MkdirAll(dst, 0o755); err != nil {
		return err
	}
	entries, err := os.ReadDir(src)
	if err != nil {
		return err
	}
	for _, entry := range entries {
		if entry.IsDir() {
			continue
		}
		from := filepath.Join(src, entry.Name())
		to := filepath.Join(dst, entry.Name())
		data, err := os.ReadFile(from)
		if err != nil {
			return err
		}
		if err := os.WriteFile(to, data, 0o644); err != nil {
			return err
		}
	}
	return nil
}
