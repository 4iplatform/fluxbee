//go:build linux

package main

import (
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

	"github.com/google/uuid"
	"github.com/open-policy-agent/opa/compile"
	"golang.org/x/sys/unix"
	"gopkg.in/yaml.v3"
)

const (
	configDir           = "/etc/json-router"
	stateDir            = "/var/lib/json-router/opa"
	nodesDir            = "/var/lib/json-router/nodes"
	routerSockDir       = "/var/run/json-router/routers"
	lockPath            = "/var/run/json-router/sy-opa-rules.lock"
	opaShmPrefix        = "/jsr-opa-"
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

type IslandConfig struct {
	IslandID string `yaml:"island_id"`
}

type Meta struct {
	Type   string `json:"type"`
	Msg    string `json:"msg,omitempty"`
	Action string `json:"action,omitempty"`
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

type Service struct {
	islandID   string
	nodeUUID   uuid.UUID
	nodeName   string
	routerConn *RouterClient
	opaRegion  *OpaRegion

	lastError string
}

func main() {
	log.SetFlags(log.LstdFlags | log.Lmicroseconds)

	islandID, err := loadIslandID()
	if err != nil {
		log.Fatalf("failed to load island.yaml: %v", err)
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
	nodeName := fmt.Sprintf("%s@%s", defaultNodeBaseName, islandID)

	opaRegion, err := openOrCreateOpaRegion(opaShmPrefix+islandID, nodeUUID)
	if err != nil {
		log.Fatalf("failed to open opa shm: %v", err)
	}

	service := &Service{
		islandID:  islandID,
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
		"/var/run/json-router",
		routerSockDir,
	}
	for _, path := range paths {
		if err := os.MkdirAll(path, 0o755); err != nil {
			return err
		}
	}
	return nil
}

func loadIslandID() (string, error) {
	path := filepath.Join(configDir, "island.yaml")
	data, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	var cfg IslandConfig
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return "", err
	}
	if cfg.IslandID == "" {
		return "", errors.New("island_id missing")
	}
	return cfg.IslandID, nil
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

func (s *Service) loadCurrentPolicy() error {
	currentRego := filepath.Join(stateDir, "current", "policy.rego")
	metaPath := filepath.Join(stateDir, "current", "metadata.json")
	data, err := os.ReadFile(currentRego)
	if err != nil {
		s.opaRegion.writeStatus(opaStatusOK)
		return nil
	}
	meta := PolicyMetadata{}
	if metaBytes, err := os.ReadFile(metaPath); err == nil {
		_ = json.Unmarshal(metaBytes, &meta)
	}
	entrypoint := meta.Entrypoint
	if entrypoint == "" {
		entrypoint = defaultEntrypoint
	}
	version := meta.Version
	if version == 0 {
		version = 1
	}
	wasm, hash, compileMs, err := compileRego(string(data), entrypoint)
	if err != nil {
		s.lastError = err.Error()
		s.opaRegion.writeStatus(opaStatusError)
		log.Printf("opa compile error: %v", err)
		return err
	}
	s.opaRegion.writePolicy(version, wasm, entrypoint)
	meta = PolicyMetadata{
		Version:    version,
		Hash:       hash,
		Entrypoint: entrypoint,
		CompiledAt: time.Now().UTC().Format(time.RFC3339),
		WasmSize:   len(wasm),
	}
	_ = writeMetadata(metaPath, meta)
	log.Printf("loaded current policy version=%d compile_time_ms=%d", version, compileMs)
	return nil
}

func (s *Service) handleMessage(msg Message) {
	switch msg.Meta.Type {
	case "system":
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

func (s *Service) handleCommand(msg Message) {
	action := strings.ToLower(msg.Meta.Action)
	switch action {
	case "compile_policy":
		var req CompileRequest
		if err := json.Unmarshal(msg.Payload, &req); err != nil {
			s.sendCommandResponse(msg, action, map[string]any{
				"status":       "error",
				"error_code":   "INVALID_PAYLOAD",
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
	switch action {
	case "get_policy":
		meta, rego := readCurrentPolicy()
		resp := map[string]any{
			"status":      "ok",
			"island":      s.islandID,
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
			"island":          s.islandID,
			"current_version": current.Version,
			"current_hash":    current.Hash,
			"staged_version":  staged.Version,
			"status":          "ok",
			"last_error":      s.lastError,
			"wasm_size_bytes": current.WasmSize,
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
		if err := writePolicyFiles(filepath.Join(stateDir, "staged"), cfg.Rego, meta); err != nil {
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
		if err := writePolicyFiles(filepath.Join(stateDir, "staged"), cfg.Rego, meta); err != nil {
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
	stagedRego := filepath.Join(stagedDir, "policy.rego")
	stagedMeta := filepath.Join(stagedDir, "metadata.json")
	rego, err := os.ReadFile(stagedRego)
	if err != nil {
		return OpaError{Code: "NOTHING_STAGED", Detail: "staged policy missing"}
	}
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
	wasm, hash, _, err := compileRego(string(rego), entrypoint)
	if err != nil {
		return OpaError{Code: "COMPILE_ERROR", Detail: err.Error()}
	}
	meta.Version = version
	meta.Hash = hash
	meta.Entrypoint = entrypoint
	meta.CompiledAt = time.Now().UTC().Format(time.RFC3339)
	meta.WasmSize = len(wasm)

	backupDir := filepath.Join(stateDir, "backup")
	currentDir := filepath.Join(stateDir, "current")
	_ = copyDir(currentDir, backupDir)
	if err := writePolicyFiles(currentDir, string(rego), meta); err != nil {
		return OpaError{Code: "SHM_ERROR", Detail: err.Error()}
	}
	s.opaRegion.writePolicy(version, wasm, entrypoint)
	s.broadcastOpaReload(version, hash)
	log.Printf("applied opa policy version=%d", version)
	return nil
}

func (s *Service) rollbackPolicy() error {
	backupDir := filepath.Join(stateDir, "backup")
	backupRego := filepath.Join(backupDir, "policy.rego")
	backupMeta := filepath.Join(backupDir, "metadata.json")
	rego, err := os.ReadFile(backupRego)
	if err != nil {
		return OpaError{Code: "NO_BACKUP", Detail: "backup policy missing"}
	}
	meta, _ := readMetadata(backupMeta)
	entrypoint := meta.Entrypoint
	if entrypoint == "" {
		entrypoint = defaultEntrypoint
	}
	wasm, hash, _, err := compileRego(string(rego), entrypoint)
	if err != nil {
		return OpaError{Code: "COMPILE_ERROR", Detail: err.Error()}
	}
	currentDir := filepath.Join(stateDir, "current")
	meta.Hash = hash
	meta.CompiledAt = time.Now().UTC().Format(time.RFC3339)
	meta.WasmSize = len(wasm)
	if err := writePolicyFiles(currentDir, string(rego), meta); err != nil {
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
			"island":       s.islandID,
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
		"island":          s.islandID,
		"compile_time_ms": compileMs,
		"wasm_size_bytes": meta.WasmSize,
		"hash":            meta.Hash,
	}
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
	mu       sync.Mutex
	conn     net.Conn
	lastDst  string
}

func NewRouterClient(nodeUUID uuid.UUID, nodeName string) *RouterClient {
	base := nodeName
	if parts := strings.Split(nodeName, "@"); len(parts) > 1 {
		base = parts[0]
	}
	return &RouterClient{nodeUUID: nodeUUID, nodeName: nodeName, nodeBase: base}
}

func (c *RouterClient) LastPeer() string {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.lastDst
}

func (c *RouterClient) Run(service *Service) {
	for {
		conn, err := c.connect()
		if err != nil {
			log.Printf("connect failed: %v", err)
			time.Sleep(1 * time.Second)
			continue
		}
		c.mu.Lock()
		c.conn = conn
		c.mu.Unlock()
		if err := c.handshake(conn); err != nil {
			log.Printf("handshake failed: %v", err)
			conn.Close()
			time.Sleep(1 * time.Second)
			continue
		}
		if err := c.readLoop(conn, service); err != nil {
			log.Printf("router read error: %v", err)
		}
		conn.Close()
		time.Sleep(1 * time.Second)
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
	hello := Message{
		Routing: Routing{
			Src:     c.nodeUUID.String(),
			Dst:     nil,
			Ttl:     1,
			TraceID: uuid.New().String(),
		},
		Meta: Meta{
			Type: "system",
			Msg:  "HELLO",
		},
		Payload: mustJSON(NodeHelloPayload{
			UUID:    c.nodeUUID.String(),
			Name:    c.nodeName,
			Version: "1.0",
		}),
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
	if msg.Meta.Type != "system" || msg.Meta.Msg != "ANNOUNCE" {
		return fmt.Errorf("unexpected announce")
	}
	var payload NodeAnnouncePayload
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

func (c *RouterClient) Send(msg Message) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.conn == nil {
		return
	}
	data := mustJSON(msg)
	if err := writeFrame(c.conn, data); err != nil {
		log.Printf("send error: %v", err)
	}
}

func readFrame(conn net.Conn) ([]byte, error) {
	var lenBuf [4]byte
	if _, err := io.ReadFull(conn, lenBuf[:]); err != nil {
		return nil, err
	}
	size := binary.BigEndian.Uint32(lenBuf[:])
	if size == 0 {
		return nil, fmt.Errorf("empty frame")
	}
	buf := make([]byte, size)
	if _, err := io.ReadFull(conn, buf); err != nil {
		return nil, err
	}
	return buf, nil
}

func writeFrame(conn net.Conn, payload []byte) error {
	if len(payload) > 64*1024 {
		return fmt.Errorf("frame too large")
	}
	var lenBuf [4]byte
	binary.BigEndian.PutUint32(lenBuf[:], uint32(len(payload)))
	if _, err := conn.Write(lenBuf[:]); err != nil {
		return err
	}
	_, err := conn.Write(payload)
	return err
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
	var err error
	wasm, err = normalizeWasmBytes(wasm)
	if err != nil {
		return nil, "", 0, err
	}
	if err := validateWasm(wasm); err != nil {
		return nil, "", 0, err
	}
	if len(wasm) > opaMaxWasmSize {
		return nil, "", 0, fmt.Errorf("wasm too large (%d bytes)", len(wasm))
	}
	hash := sha256.Sum256(wasm)
	return wasm, "sha256:" + hex.EncodeToString(hash[:]), time.Since(start).Milliseconds(), nil
}

func normalizeWasmBytes(data []byte) ([]byte, error) {
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
		return decoded, nil
	}
	return data, nil
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

func writePolicyFiles(dir string, rego string, meta PolicyMetadata) error {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(dir, "policy.rego"), []byte(rego), 0o644); err != nil {
		return err
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
