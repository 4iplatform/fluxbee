package node

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"slices"
	"sort"
	"strconv"
	"strings"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

const workflowRuntimeBase = "wf.engine"

// workflowRuntimeVersion encodes the monotonic uint64 workflow version as a
// semver-valid string. Runtime packages (including type=workflow) are validated
// against strict MAJOR.MINOR.PATCH by the runtime-package publisher
// (src/runtime_package.rs), so the raw integer ("1") is rejected. The uint64
// counter stays the source of truth; only its string encoding changes. It is the
// SINGLE place the version string is derived, so the package.json version, the
// dist directory name, the manifest entry, the purge keep-set, and the spawned
// node's runtime_version all agree byte-for-byte.
func workflowRuntimeVersion(v uint64) string {
	return fmt.Sprintf("0.0.%d", v)
}

// workflowVersionOf is the counter a version string encodes (workflowRuntimeVersion: 0.0.<n>), and
// whether it is one: any other version of wf.<workflow> is none SY.wf-rules publishes.
func workflowVersionOf(version string) (uint64, bool) {
	counter, ok := strings.CutPrefix(strings.TrimSpace(version), "0.0.")
	if !ok {
		return 0, false
	}
	n, err := strconv.ParseUint(counter, 10, 64)
	return n, err == nil
}

// highestPublishedVersion is the highest counter among the versions of wf.<workflow> this hive's
// dist holds, 0 for none: its version directories (DistRuntimeRoot/wf.<workflow>/0.0.<n>) and the
// versions its runtimes manifest lists for it (available, current). On the motherbee they are the
// motherbee's own; on a worker, its receive-only Syncthing mirror of them. A directory or a
// manifest that cannot be read is an error: the versions it holds are unknown.
func (s *Service) highestPublishedVersion(workflowName string) (uint64, error) {
	runtimeName := workflowRuntimeName(workflowName)
	entries, err := os.ReadDir(filepath.Join(s.cfg.DistRuntimeRoot, runtimeName))
	if err != nil && !os.IsNotExist(err) {
		return 0, fmt.Errorf("cannot list the versions of %s in this hive's dist: %w", runtimeName, err)
	}
	var versions []string
	for _, entry := range entries {
		if entry.IsDir() {
			versions = append(versions, entry.Name())
		}
	}
	manifest, err := loadRuntimeManifest(filepath.Join(s.cfg.DistRuntimeRoot, "manifest.json"))
	if err != nil {
		return 0, fmt.Errorf("cannot read the versions of %s in this hive's runtimes manifest: %w", runtimeName, err)
	}
	entry := manifest.Runtimes[runtimeName]
	versions = append(append(versions, entry.Available...), entry.Current)
	highest := uint64(0)
	for _, version := range versions {
		if n, ok := workflowVersionOf(version); ok && n > highest {
			highest = n
		}
	}
	return highest, nil
}

type PackagePublishResult struct {
	RuntimeName string
	Version     string
	PackagePath string
}

type runtimeManifest struct {
	SchemaVersion uint64                          `json:"schema_version"`
	Version       uint64                          `json:"version"`
	UpdatedAt     string                          `json:"updated_at,omitempty"`
	Runtimes      map[string]runtimeManifestEntry `json:"runtimes"`
	Hash          *string                         `json:"hash,omitempty"`
}

type runtimeManifestEntry struct {
	Available   []string `json:"available"`
	Current     string   `json:"current"`
	PackageType string   `json:"type"`
	RuntimeBase string   `json:"runtime_base"`
}

func (s *Service) PublishWorkflowPackage(workflowName string, meta WfRulesMetadata, definitionBytes []byte) (*PackagePublishResult, error) {
	ctx, cancel := newRequestContext()
	defer cancel()
	return s.publishWorkflowPackage(ctx, workflowName, meta, definitionBytes)
}

func (s *Service) publishWorkflowPackage(ctx context.Context, workflowName string, meta WfRulesMetadata, definitionBytes []byte) (*PackagePublishResult, error) {
	packageFiles, err := buildWorkflowPackageFiles(s.cfg.HiveID, workflowName, meta, definitionBytes)
	if err != nil {
		return nil, err
	}
	if s.admin == nil {
		return nil, fmt.Errorf("admin client unavailable")
	}
	callCtx, cancel := context.WithTimeout(ctx, adminRPCTimeout)
	defer cancel()
	return s.admin.PublishRuntimePackage(callCtx, packageFiles)
}

func buildWorkflowPackageFiles(hiveID, workflowName string, meta WfRulesMetadata, definitionBytes []byte) (map[string]string, error) {
	runtimeName := "wf." + workflowName
	version := workflowRuntimeVersion(meta.Version)

	packageJSON := map[string]any{
		"name":         runtimeName,
		"version":      version,
		"type":         "workflow",
		"description":  fmt.Sprintf("Workflow package for %s", workflowName),
		"runtime_base": workflowRuntimeBase,
	}
	packageBytes, err := json.MarshalIndent(packageJSON, "", "  ")
	if err != nil {
		return nil, err
	}
	packageBytes = append(packageBytes, '\n')
	defaultConfig := map[string]any{
		"sy_timer_l2_name":    fmt.Sprintf("SY.timer@%s", hiveID),
		"gc_retention_days":   defaultWFGCRetentionDays,
		"gc_interval_seconds": defaultWFGCIntervalSeconds,
	}
	defaultConfigBytes, err := json.MarshalIndent(defaultConfig, "", "  ")
	if err != nil {
		return nil, err
	}
	defaultConfigBytes = append(defaultConfigBytes, '\n')
	return map[string]string{
		"package.json":               string(packageBytes),
		"flow/definition.json":       string(definitionBytes),
		"config/default-config.json": string(defaultConfigBytes),
	}, nil
}

// PackageLeft is a version of a workflow's package that a purge did not remove from the motherbee's
// dist, and why: SY.admin refused its removal (ErrorCode is SY.admin's: RUNTIME_USAGE_UNKNOWN while
// a hive that may run the version cannot be read, RUNTIME_IN_USE while a node runs it), did not
// answer in time (TIMEOUT: it may still remove it) or could not be reached (TRANSPORT_ERROR); the
// purge stopped at an earlier version and did not ask (PURGE_STOPPED); or the versions could not be
// listed (DIST_UNREADABLE, with no Version).
type PackageLeft struct {
	Runtime   string `json:"runtime"`
	Version   string `json:"version"`
	ErrorCode string `json:"error_code"`
	Detail    string `json:"detail"`
	// Retryable says that remove_runtime_version asked again for the version, nothing else changed
	// first, may remove it (removalFailure): true for a refusal SY.admin says may pass, a TIMEOUT, a
	// TRANSPORT_ERROR and a PURGE_STOPPED; false for one that needs a change first (RUNTIME_IN_USE:
	// stop or move the node; a RUNTIME_USAGE_UNKNOWN SY.admin says is not, with each PURGE_STOPPED
	// after it: purgeStoppedAt) and for DIST_UNREADABLE (this hive's dist first).
	Retryable bool `json:"retryable"`
	// UnansweredHives are the hives whose nodes SY.admin could not read, as its refusal names them
	// (its payload.unanswered_hives, each {hive, reason, detail}), when it names any.
	UnansweredHives []any `json:"unanswered_hives,omitempty"`
}

func (s *Service) PurgeWorkflowPackages(workflowName string, preserveBoundVersion bool) ([]PackageLeft, error) {
	ctx, cancel := newRequestContext()
	defer cancel()
	return s.purgeWorkflowPackages(ctx, workflowName, preserveBoundVersion)
}

// purgeWorkflowPackages removes from the motherbee's dist each version of wf.<workflow> that no slot
// keeps (current, backup and, with preserveBoundVersion, the version the WF node's config names),
// among those this hive's dist holds and those named names. A delete names the versions the
// workflow's slots held, read before its state went: on a worker this hive's dist is a Syncthing
// copy of the motherbee's, which lists a version published there 10-60 s later, so a delete within
// that window found no wf.<workflow> here, removed nothing and answered packages_left [], while the
// version stayed in the motherbee's dist (the review of A-281). A named version the motherbee does
// not hold (a staged one never applied) SY.admin answers RUNTIME_VERSION_NOT_FOUND, before its read
// of the version's use, which counts as removed. It answers the versions it did not remove, none
// when it removed them all (purgeOnPrimary), and an error, with nothing removed, when it cannot
// tell which to remove: the runtime's directory cannot be listed, or the version the WF node is
// bound to cannot be read.
func (s *Service) purgeWorkflowPackages(ctx context.Context, workflowName string, preserveBoundVersion bool, named ...string) ([]PackageLeft, error) {
	runtimeName := workflowRuntimeName(workflowName)
	entries, err := os.ReadDir(filepath.Join(s.cfg.DistRuntimeRoot, runtimeName))
	if err != nil && !os.IsNotExist(err) {
		return nil, fmt.Errorf("cannot list the versions of %s in this hive's dist: %w", runtimeName, err)
	}
	var versions []string
	for _, entry := range entries {
		if entry.IsDir() {
			versions = append(versions, entry.Name())
		}
	}
	for _, version := range named {
		if !slices.Contains(versions, version) {
			versions = append(versions, version)
		}
	}
	if len(versions) == 0 {
		return nil, nil
	}

	keep := map[string]struct{}{}
	if meta, err := s.store.ReadCurrentMetadata(workflowName); err == nil {
		keep[workflowRuntimeVersion(meta.Version)] = struct{}{}
	}
	if meta, err := s.store.ReadBackupMetadata(workflowName); err == nil {
		keep[workflowRuntimeVersion(meta.Version)] = struct{}{}
	}
	if preserveBoundVersion {
		boundVersion, err := s.boundRuntimeVersionForWorkflow(ctx, workflowName)
		if err != nil {
			return nil, err
		}
		if strings.TrimSpace(boundVersion) != "" {
			keep[strings.TrimSpace(boundVersion)] = struct{}{}
		}
	}

	// The dist tree and its manifest are the motherbee's, and only SY.admin writes them: obsolete
	// versions are removed through it on every hive (A-68). On any other hive the tree is a
	// Syncthing receive-only mirror, where a local delete is never restored and a manifest edit
	// only diverges from the source; on the motherbee, a write of its own raced the admin's
	// publishes now that the admin serves the mesh concurrently.
	var obsolete []string
	for _, version := range versions {
		if _, ok := keep[version]; ok {
			continue
		}
		obsolete = append(obsolete, version)
	}
	return s.purgeOnPrimary(ctx, runtimeName, obsolete), nil
}

// purgeAfterPublish is an apply's or a rollback's purge (purgeWorkflowPackages, keeping the version
// the WF node is bound to), within deployPurgeBudget so the node's step after it keeps its time.
// The deploy goes on whatever it leaves, and the next one tries that again: what it left is
// logged, not answered.
func (s *Service) purgeAfterPublish(ctx context.Context, workflowName string) {
	purgeCtx, cancel := context.WithTimeout(ctx, deployPurgeBudget)
	defer cancel()
	left, err := s.purgeWorkflowPackages(purgeCtx, workflowName, true)
	if err != nil {
		log.Printf("purge of %s skipped: %v", workflowRuntimeName(workflowName), err)
	}
	for _, pkg := range left {
		log.Printf("purge left %s %s: %s: %s", pkg.Runtime, pkg.Version, pkg.ErrorCode, pkg.Detail)
	}
}

// purgeOnPrimary removes versions from the motherbee's dist, one at a time, and answers those it
// did not remove. It stops at the first removal that fails, SY.admin's refusal or no answer in
// time: each removal reads the version's use on every hive first (FINDINGS A-281), and what refused
// one (a hive that cannot be read) holds for the next, which would wait as long to be refused too.
// The version it stopped at is answered with why, and each one it had still to remove as
// PURGE_STOPPED.
//
// The admin refuses to remove a runtime's current version while other versions remain
// (RUNTIME_CURRENT_CONFLICT), so the current one goes last: first by this hive's copy of the
// manifest, and a version refused as current is tried again once the others are gone, since a
// worker's copy can lag behind the motherbee's (a rollback moves the current version back; 0.1.70
// gate, A-68).
func (s *Service) purgeOnPrimary(ctx context.Context, runtimeName string, versions []string) []PackageLeft {
	current := ""
	if manifest, err := loadRuntimeManifest(filepath.Join(s.cfg.DistRuntimeRoot, "manifest.json")); err == nil {
		current = manifest.Runtimes[runtimeName].Current
	}
	sort.SliceStable(versions, func(i, j int) bool {
		return versions[i] != current && versions[j] == current
	})
	var refusedAsCurrent []string
	for i, version := range versions {
		err := s.removeRuntimeVersionOnPrimary(ctx, runtimeName, version)
		var actionErr *adminActionError
		if errors.As(err, &actionErr) && actionErr.Code == "RUNTIME_CURRENT_CONFLICT" {
			refusedAsCurrent = append(refusedAsCurrent, version)
			continue
		}
		if err != nil {
			notRemoved := append(append([]string{}, versions[i+1:]...), refusedAsCurrent...)
			return purgeStoppedAt(runtimeName, version, err, notRemoved)
		}
	}
	for i, version := range refusedAsCurrent {
		if err := s.removeRuntimeVersionOnPrimary(ctx, runtimeName, version); err != nil {
			return purgeStoppedAt(runtimeName, version, err, refusedAsCurrent[i+1:])
		}
	}
	return nil
}

// purgeStoppedAt is what a purge that stopped at version leaves: that version, with why its
// removal failed (removalFailure), and each version it had still to remove, PURGE_STOPPED (a
// version refused as current earlier in the pass, waiting to be asked again, among them), which
// was not asked, so asking may remove it; but not after a RUNTIME_USAGE_UNKNOWN SY.admin says
// cannot pass before something changes (a hive not read is a join that ended, or the motherbee
// could not read its own nodes): what it could not read holds for every version, whose removal
// reads the same (FINDINGS A-281, the integration's review).
func purgeStoppedAt(runtimeName, version string, err error, notRemoved []string) []PackageLeft {
	stopped := removalFailure(err)
	stopped.Runtime, stopped.Version = runtimeName, version
	retryable := stopped.Retryable || stopped.ErrorCode != "RUNTIME_USAGE_UNKNOWN"
	left := []PackageLeft{stopped}
	for _, next := range notRemoved {
		left = append(left, PackageLeft{
			Runtime:   runtimeName,
			Version:   next,
			ErrorCode: "PURGE_STOPPED",
			Detail:    fmt.Sprintf("the purge stopped at %s %s (%s) before removing it", runtimeName, version, stopped.ErrorCode),
			Retryable: retryable,
		})
	}
	return left
}

// removalFailure is why a removal of a runtime version failed, as packages_left names it, without
// the runtime and the version: SY.admin's refusal, with its code, its detail, the hives it could
// not read (its payload.unanswered_hives) and whether asking again may pass (its payload.retryable,
// or passingRemovalRefusal when it says nothing); TIMEOUT when no answer came in time, which does
// not mean the version stayed: SY.admin may still remove it; TRANSPORT_ERROR when SY.admin was not
// reached. Both of these are retryable.
func removalFailure(err error) PackageLeft {
	var actionErr *adminActionError
	switch {
	case errors.As(err, &actionErr):
		left := PackageLeft{ErrorCode: actionErr.Code, Detail: actionErr.Message}
		if left.ErrorCode == "" {
			left.ErrorCode = "SERVICE_FAILED"
		}
		if left.Detail == "" {
			left.Detail = err.Error()
		}
		payload, _ := actionErr.Payload["payload"].(map[string]any)
		if hives, _ := payload["unanswered_hives"].([]any); len(hives) > 0 {
			left.UnansweredHives = hives
		}
		retryable, said := payload["retryable"].(bool)
		left.Retryable = retryable || !said && passingRemovalRefusal(left.ErrorCode)
		return left
	case isRPCTimeout(nil, err):
		return PackageLeft{
			ErrorCode: "TIMEOUT",
			Detail:    fmt.Sprintf("no answer in time (%v); SY.admin may still remove it, and remove_runtime_version answers RUNTIME_VERSION_NOT_FOUND once it is gone", err),
			Retryable: true,
		}
	default:
		return PackageLeft{ErrorCode: "TRANSPORT_ERROR", Detail: err.Error(), Retryable: true}
	}
}

// passingRemovalRefusal is whether a refusal of remove_runtime_version whose payload does not say
// may pass when asked again, nothing changed first: SY.admin could not read some hive's nodes
// (RUNTIME_USAGE_UNKNOWN, which says retryable itself since 0.1.133) or the nodes connected now,
// which it reads from shared memory for A-62 (INVENTORY_UNAVAILABLE, 503). Any other refusal needs
// a change first, as RUNTIME_IN_USE does (a node runs the version), or is SY.admin's to say.
func passingRemovalRefusal(code string) bool {
	return code == "RUNTIME_USAGE_UNKNOWN" || code == "INVENTORY_UNAVAILABLE"
}

// removeRuntimeVersionOnPrimary asks SY.admin to remove one version from the motherbee's dist, and
// waits runtimeRemovalTimeout at most for its answer, within the request's deadline.
func (s *Service) removeRuntimeVersionOnPrimary(ctx context.Context, runtimeName, version string) error {
	if s.admin == nil {
		return fmt.Errorf("admin client unavailable")
	}
	callCtx, cancel := context.WithTimeout(ctx, runtimeRemovalTimeout)
	defer cancel()
	if err := s.admin.RemoveRuntimeVersion(callCtx, runtimeName, version); err != nil {
		var actionErr *adminActionError
		if errors.As(err, &actionErr) &&
			(actionErr.Code == "RUNTIME_NOT_FOUND" || actionErr.Code == "RUNTIME_VERSION_NOT_FOUND") {
			// Already gone from the motherbee (e.g. a directory the manifest no longer lists):
			// exactly the state the purge wants.
			return nil
		}
		return fmt.Errorf("remove %s %s on %s: %w", runtimeName, version, fluxbeesdk.PrimaryAdminNode, err)
	}
	return nil
}

func loadRuntimeManifest(path string) (*runtimeManifest, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return &runtimeManifest{
				SchemaVersion: 2,
				Version:       1,
				Runtimes:      map[string]runtimeManifestEntry{},
			}, nil
		}
		return nil, err
	}
	var manifest runtimeManifest
	if err := json.Unmarshal(data, &manifest); err != nil {
		return nil, err
	}
	if manifest.Runtimes == nil {
		manifest.Runtimes = map[string]runtimeManifestEntry{}
	}
	if manifest.SchemaVersion == 0 {
		manifest.SchemaVersion = 1
	}
	return &manifest, nil
}

func workflowRuntimeName(workflowName string) string {
	return "wf." + workflowName
}
