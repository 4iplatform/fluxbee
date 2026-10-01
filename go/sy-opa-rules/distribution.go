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
	"path/filepath"
	"strings"
	"syscall"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// One global user policy, compiled on the motherbee only. The motherbee hands the compiled wasm to
// every other hive through a Syncthing folder (sendonly there, receiveonly everywhere else); each
// hive's SY.opa.rules installs it when the file arrives and keeps its own copy in stateDir, so it
// runs on its last policy while cut off. Only the wasm travels: the rego stays on the motherbee.
var policyDistDir = "/var/lib/fluxbee/dist/policy/opa"

// What the routers' region holds when there is no user policy (its hash field is zeroed).
var noPolicyRegionHash = "sha256:" + strings.Repeat("0", 64)

const (
	policyManifestName = "manifest.json"
	// A hive still waiting for the published wasm after this long says so in its log (once); its
	// status says it from the first failed check.
	policyStuckAfter = time.Minute
	// How often a hive checks its local copy of the folder for a policy that arrived without a
	// notice (a hive that was down or cut off when the policy changed). Local only: no one is
	// asked anything.
	policySyncInterval = 5 * time.Second
	// After a notice, the file is usually a second or two behind it: re-check this often while a
	// notice waits, for at most this long, then back to policySyncInterval.
	policyNoticeRecheck    = 1 * time.Second
	policyNoticeRecheckFor = 30 * time.Second
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
		s.clearWaiting()
		return false, nil
	}
	wasm, err := os.ReadFile(filepath.Join(policyDistDir, filepath.Base(manifest.WasmFile)))
	if err != nil {
		s.noteWaiting(manifest.Version, manifest.Hash, "the wasm the manifest names has not arrived")
		return false, errPolicyNotArrived
	}
	sum := sha256.Sum256(wasm)
	if "sha256:"+hex.EncodeToString(sum[:]) != manifest.Hash {
		s.noteWaiting(manifest.Version, manifest.Hash, "the wasm does not match the manifest's sha256")
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
	s.clearWaiting()
	log.Printf("installed the published opa policy version=%d hash=%s", manifest.Version, manifest.Hash)
	return true, nil
}

// syncOnce runs one sync (replicas) and answers the notices it satisfies.
func (s *Service) syncOnce() {
	s.policyMu.Lock()
	defer s.policyMu.Unlock()
	if s.isPrimary() {
		s.republishIfStale()
	} else if _, err := s.syncFromPublishedPolicy(); err != nil && !errors.Is(err, errPolicyNotArrived) {
		s.setLastError(err.Error())
		log.Printf("opa policy sync failed: %v", err)
	}
	s.ensureRegionMatchesCurrent()
	s.answerPendingSyncs()
}

// installedPolicy is currentPolicy, only if its wasm is one the routers can load; a corrupt one
// (an install cut short) is never published nor handed to the routers.
func installedPolicy() (PolicyMetadata, []byte, error) {
	meta, wasm := currentPolicy()
	if wasm == nil {
		return meta, nil, nil
	}
	if err := validateWasm(wasm); err != nil {
		return meta, nil, fmt.Errorf("installed opa policy is not a valid wasm: %w", err)
	}
	if !wasmHasExport(wasm, "opa_eval") {
		return meta, nil, errors.New("installed opa policy lacks the opa_eval export")
	}
	return meta, wasm, nil
}

// ensureRegionMatchesCurrent (every hive) makes the routers' region carry the installed policy.
// An install that stopped halfway — current/ written, region not (disk full) — left the hive
// reporting a policy its routers did not run, and nothing retried it. Under policyMu.
func (s *Service) ensureRegionMatchesCurrent() {
	if s.opaRegion == nil {
		return
	}
	current, wasm, err := installedPolicy()
	if err != nil {
		s.setLastError(err.Error())
		return
	}
	want := noPolicyRegionHash
	if wasm != nil {
		want = current.Hash
	}
	if s.opaRegion.policyHash() == want {
		return
	}
	if wasm == nil {
		s.opaRegion.writePolicy(0, nil, "")
		s.broadcastOpaReload(0, noPolicyRegionHash)
	} else {
		entrypoint := current.Entrypoint
		if entrypoint == "" {
			entrypoint = defaultEntrypoint
		}
		s.opaRegion.writePolicy(current.Version, wasm, entrypoint)
		s.broadcastOpaReload(current.Version, current.Hash)
	}
	log.Printf("routers' opa region repaired to the installed policy hash=%s", want)
}

// republishIfStale (motherbee) publishes again when the published manifest does not name the
// policy that runs here — a publish that failed after the apply (disk full, permissions), or a
// changed synced copy. Must be called with policyMu held.
func (s *Service) republishIfStale() {
	current, _, err := installedPolicy()
	if err != nil {
		s.setLastError(err.Error())
		return
	}
	if manifest, err := readPolicyManifest(); err == nil && manifest.Hash == current.Hash {
		s.lastPublishErr = ""
		return
	}
	if err := s.publishPolicy(); err != nil {
		if err.Error() != s.lastPublishErr {
			log.Printf("opa policy re-publish failed (retrying every %s): %v", policySyncInterval, err)
		}
		s.lastPublishErr = err.Error()
		s.setLastError(publishFailedDetail(err))
		return
	}
	if s.lastPublishErr != "" {
		log.Printf("opa policy re-published after an earlier failure hash=%s", current.Hash)
		s.setLastError("")
	}
	s.lastPublishErr = ""
}

// publishFailedDetail says what a failed publish leaves behind: the motherbee already runs the
// policy; the other hives get it once a retry publishes it.
func publishFailedDetail(err error) string {
	return fmt.Sprintf("applied on this hive but not published to the others: %v; "+
		"publishing is retried every %s", err, policySyncInterval)
}

// policySyncLoop keeps a replica on the published policy, and the motherbee's publication on what
// it runs: at start, on every notice and every policySyncInterval (local check of the synced
// folder) — every policyNoticeRecheck while a fresh notice waits for its policy.
func (s *Service) policySyncLoop() {
	timer := time.NewTimer(0)
	defer timer.Stop()
	for {
		select {
		case <-timer.C:
		case <-s.syncKick:
			if !timer.Stop() {
				select {
				case <-timer.C:
				default:
				}
			}
		}
		s.syncOnce()
		timer.Reset(s.nextSyncCheck(time.Now()))
	}
}

// nextSyncCheck: soon while a notice younger than policyNoticeRecheckFor waits for its policy,
// otherwise the regular interval.
func (s *Service) nextSyncCheck(now time.Time) time.Duration {
	s.policyMu.Lock()
	defer s.policyMu.Unlock()
	for _, notice := range s.pendingSyncs {
		if now.Sub(notice.received) < policyNoticeRecheckFor {
			return policyNoticeRecheck
		}
	}
	return policySyncInterval
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

// chownForSync hands a published file to whoever owns the synced folder (dist/policy): the
// orchestrator gives it to the Syncthing service user configured in hive.yaml.
func chownForSync(path string) {
	info, err := os.Stat(filepath.Dir(policyDistDir))
	if err != nil {
		return
	}
	st, ok := info.Sys().(*syscall.Stat_t)
	if !ok {
		return
	}
	_ = os.Chown(path, int(st.Uid), int(st.Gid))
}

// syncStatus is what get_status reports beyond the files: the last error, and — when the hive is
// behind — the published policy it waits for, since when, and why.
type syncStatus struct {
	lastError  string
	waitHash   string
	waitVer    uint64
	waitReason string
	waitSince  time.Time
	waitWarned bool
}

func (s *Service) setLastError(detail string) {
	s.statusMu.Lock()
	s.syncStatus.lastError = detail
	s.statusMu.Unlock()
}

func (s *Service) getLastError() string {
	s.statusMu.Lock()
	defer s.statusMu.Unlock()
	return s.syncStatus.lastError
}

// noteWaiting records that the published policy (version, hash) cannot be installed yet. The log
// says so once, when it has been waiting longer than policyStuckAfter.
func (s *Service) noteWaiting(version uint64, hash, reason string) {
	s.statusMu.Lock()
	defer s.statusMu.Unlock()
	st := &s.syncStatus
	if st.waitHash != hash {
		st.waitHash, st.waitVer, st.waitSince, st.waitWarned = hash, version, time.Now(), false
	}
	st.waitReason = reason
	if !st.waitWarned && time.Since(st.waitSince) >= policyStuckAfter {
		st.waitWarned = true
		log.Printf("opa policy version=%d hash=%s still not installable after %s: %s",
			version, hash, time.Since(st.waitSince).Round(time.Second), reason)
	}
}

func (s *Service) clearWaiting() {
	s.statusMu.Lock()
	defer s.statusMu.Unlock()
	st := &s.syncStatus
	if st.waitWarned {
		log.Printf("opa policy hash=%s installed after waiting %s", st.waitHash, time.Since(st.waitSince).Round(time.Second))
	}
	st.waitHash, st.waitVer, st.waitReason, st.waitSince, st.waitWarned = "", 0, "", time.Time{}, false
}

// statusView is the get_status answer: what this hive runs, what was published to it, what its
// routers load, and — when it is behind — what it waits for and why.
func (s *Service) statusView() map[string]any {
	current, _ := readMetadata(filepath.Join(stateDir, "current", "metadata.json"))
	staged, _ := readMetadata(filepath.Join(stateDir, "staged", "metadata.json"))
	installed, installedWasm := currentPolicy()
	resp := map[string]any{
		"hive":            s.hiveID,
		"current_version": current.Version,
		"current_hash":    current.Hash,
		"staged_version":  staged.Version,
		"status":          "ok",
		"wasm_size_bytes": current.WasmSize,
		"routers":         listRouterStatuses(),
	}
	inSync := true
	if manifest, err := readPolicyManifest(); err == nil {
		resp["published_version"] = manifest.Version
		resp["published_hash"] = manifest.Hash
		inSync = manifest.Hash == installed.Hash
	}
	if s.opaRegion != nil {
		region := s.opaRegion.policyHash()
		resp["region_hash"] = region
		want := noPolicyRegionHash
		if installedWasm != nil {
			want = installed.Hash
		}
		inSync = inSync && region == want
	}
	resp["in_sync"] = inSync

	s.statusMu.Lock()
	st := s.syncStatus
	s.statusMu.Unlock()
	resp["last_error"] = st.lastError
	if st.waitHash != "" {
		resp["waiting"] = map[string]any{
			"version": st.waitVer,
			"hash":    st.waitHash,
			"since":   st.waitSince.UTC().Format(time.RFC3339),
			"reason":  st.waitReason,
		}
	}
	return resp
}
