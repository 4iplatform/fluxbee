//go:build linux

package main

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"os"
	"os/user"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// One global user policy, compiled on the motherbee only. The motherbee hands the compiled wasm to
// every other hive through a Syncthing folder (sendonly there, receiveonly everywhere else); each
// hive's SY.opa.rules installs it when the file arrives and keeps its own copy in stateDir, so it
// runs on its last policy while cut off. Only the wasm travels: the rego stays on the motherbee.
var policyDistDir = "/var/lib/fluxbee/dist/policy/opa"

const (
	policyManifestName = "manifest.json"
	// The folder is Syncthing's: it runs as this user and must read (motherbee) / write (spokes).
	policySyncUser = "fluxbee"
	// How often a hive checks its local copy of the folder for a policy that arrived without a
	// notice (a hive that was down or cut off when the policy changed). Local only: no one is
	// asked anything.
	policySyncInterval = 5 * time.Second
	// A notice not satisfied by then is dropped: the admin stopped waiting long before, and a
	// notice for a policy superseded before it arrived would otherwise be kept forever.
	policyNoticeTTL = 5 * time.Minute
)

// PolicyManifest names the published policy. Hash "" means "no user policy" (cleared).
type PolicyManifest struct {
	SchemaVersion int    `json:"schema_version"`
	Version       uint64 `json:"version"`
	Hash          string `json:"hash"`
	Entrypoint    string `json:"entrypoint,omitempty"`
	WasmFile      string `json:"wasm_file,omitempty"`
	CompiledAt    string `json:"compiled_at,omitempty"`
	PublishedAt   string `json:"published_at"`
}

// A notice the admin is waiting on: answer once this hive runs the announced policy.
type pendingSync struct {
	src      string
	version  uint64
	hash     string
	received time.Time
}

var errPolicyNotArrived = errors.New("published policy not fully synced to this hive yet")

func (s *Service) isPrimary() bool {
	return s.hiveID == fluxbeesdk.PrimaryHiveID
}

// currentPolicy reads what this hive runs now. Hash "" = no user policy.
func currentPolicy() (PolicyMetadata, []byte) {
	meta, _ := readMetadata(filepath.Join(stateDir, "current", "metadata.json"))
	wasm, err := os.ReadFile(filepath.Join(stateDir, "current", "policy.wasm"))
	if err != nil || len(wasm) == 0 {
		return PolicyMetadata{}, nil
	}
	sum := sha256.Sum256(wasm)
	meta.Hash = "sha256:" + hex.EncodeToString(sum[:])
	meta.WasmSize = len(wasm)
	return meta, wasm
}

// publishPolicy (motherbee) writes the current policy — or its absence — to the synced folder:
// the wasm first, then the manifest that names it.
func (s *Service) publishPolicy() error {
	if err := os.MkdirAll(policyDistDir, 0o750); err != nil {
		return fmt.Errorf("publish: %w", err)
	}
	chownForSync(policyDistDir)
	meta, wasm := currentPolicy()
	manifest := PolicyManifest{SchemaVersion: 1, PublishedAt: time.Now().UTC().Format(time.RFC3339)}
	if wasm != nil {
		manifest.Version = meta.Version
		if manifest.Version == 0 {
			manifest.Version = 1
		}
		manifest.Hash = meta.Hash
		manifest.Entrypoint = meta.Entrypoint
		if manifest.Entrypoint == "" {
			manifest.Entrypoint = defaultEntrypoint
		}
		manifest.CompiledAt = meta.CompiledAt
		manifest.WasmFile = "policy-" + strings.TrimPrefix(meta.Hash, "sha256:")[:16] + ".wasm"
		if err := writeSyncedFile(filepath.Join(policyDistDir, manifest.WasmFile), wasm); err != nil {
			return fmt.Errorf("publish wasm: %w", err)
		}
	}
	data, err := json.MarshalIndent(manifest, "", "  ")
	if err != nil {
		return err
	}
	if err := writeSyncedFile(filepath.Join(policyDistDir, policyManifestName), data); err != nil {
		return fmt.Errorf("publish manifest: %w", err)
	}
	// Only the named wasm stays; the sync carries the removals.
	if entries, err := os.ReadDir(policyDistDir); err == nil {
		for _, entry := range entries {
			name := entry.Name()
			if strings.HasPrefix(name, "policy-") && strings.HasSuffix(name, ".wasm") && name != manifest.WasmFile {
				_ = os.Remove(filepath.Join(policyDistDir, name))
			}
		}
	}
	log.Printf("published opa policy for all hives version=%d hash=%s", manifest.Version, manifest.Hash)
	return nil
}

func readPolicyManifest() (PolicyManifest, error) {
	var manifest PolicyManifest
	data, err := os.ReadFile(filepath.Join(policyDistDir, policyManifestName))
	if err != nil {
		return manifest, err
	}
	err = json.Unmarshal(data, &manifest)
	return manifest, err
}

// syncFromPublishedPolicy (every hive but the motherbee) installs the published policy if it is
// not the one running here. errPolicyNotArrived: the manifest is here but its wasm is missing or
// incomplete — the sync is still carrying it; try again later.
func (s *Service) syncFromPublishedPolicy() (bool, error) {
	manifest, err := readPolicyManifest()
	if err != nil {
		if os.IsNotExist(err) {
			return false, nil
		}
		return false, err
	}
	current, currentWasm := currentPolicy()
	if manifest.Hash == "" {
		if currentWasm == nil {
			return false, nil
		}
		if err := s.clearPolicy(); err != nil {
			return false, err
		}
		return true, nil
	}
	if manifest.Hash == current.Hash {
		return false, nil
	}
	wasm, err := os.ReadFile(filepath.Join(policyDistDir, filepath.Base(manifest.WasmFile)))
	if err != nil {
		return false, errPolicyNotArrived
	}
	sum := sha256.Sum256(wasm)
	if "sha256:"+hex.EncodeToString(sum[:]) != manifest.Hash {
		return false, errPolicyNotArrived
	}
	staged := PolicyMetadata{
		Version:    manifest.Version,
		Hash:       manifest.Hash,
		Entrypoint: manifest.Entrypoint,
		CompiledAt: manifest.CompiledAt,
		WasmSize:   len(wasm),
	}
	if err := writePolicyFiles(filepath.Join(stateDir, "staged"), wasm, staged, ""); err != nil {
		return false, err
	}
	if err := s.applyPolicy(manifest.Version); err != nil {
		return false, err
	}
	log.Printf("installed the published opa policy version=%d hash=%s", manifest.Version, manifest.Hash)
	return true, nil
}

// syncOnce runs one sync (replicas) and answers the notices it satisfies.
func (s *Service) syncOnce() {
	s.policyMu.Lock()
	defer s.policyMu.Unlock()
	if !s.isPrimary() {
		if _, err := s.syncFromPublishedPolicy(); err != nil && !errors.Is(err, errPolicyNotArrived) {
			s.lastError = err.Error()
			log.Printf("opa policy sync failed: %v", err)
		}
	}
	s.answerPendingSyncs()
}

// policySyncLoop keeps a replica on the published policy: at start, on every notice and every
// policySyncInterval (local check of the synced folder).
func (s *Service) policySyncLoop() {
	ticker := time.NewTicker(policySyncInterval)
	defer ticker.Stop()
	s.syncOnce()
	for {
		select {
		case <-ticker.C:
		case <-s.syncKick:
		}
		s.syncOnce()
	}
}

// handleSyncNotice: the admin announced a newly published policy. Answer at once if this hive
// already runs it; otherwise when the file arrives.
func (s *Service) handleSyncNotice(src string, version uint64, hash string) {
	s.policyMu.Lock()
	s.pendingSyncs = append(s.pendingSyncs, pendingSync{src: src, version: version, hash: hash, received: time.Now()})
	s.policyMu.Unlock()
	select {
	case s.syncKick <- struct{}{}:
	default:
	}
}

// answerPendingSyncs must be called with policyMu held.
func (s *Service) answerPendingSyncs() {
	if len(s.pendingSyncs) == 0 {
		return
	}
	current, _ := currentPolicy()
	kept := s.pendingSyncs[:0]
	for _, notice := range s.pendingSyncs {
		if notice.hash != current.Hash {
			if time.Since(notice.received) < policyNoticeTTL {
				kept = append(kept, notice)
			}
			continue
		}
		s.sendConfigResponse(notice.src, "sync", notice.version, "ok", current, 0)
	}
	s.pendingSyncs = kept
}

// writeSyncedFile writes atomically (temp + rename) and hands the file to Syncthing's user.
func writeSyncedFile(path string, data []byte) error {
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, data, 0o640); err != nil {
		return err
	}
	chownForSync(tmp)
	return os.Rename(tmp, path)
}

func chownForSync(path string) {
	account, err := user.Lookup(policySyncUser)
	if err != nil {
		return
	}
	uid, errUID := strconv.Atoi(account.Uid)
	gid, errGID := strconv.Atoi(account.Gid)
	if errUID != nil || errGID != nil {
		return
	}
	_ = os.Chown(path, uid, gid)
}
