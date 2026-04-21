package node

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"
)

const workflowRuntimeBase = "wf.engine"

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
	packageFiles, err := buildWorkflowPackageFiles(s.cfg.HiveID, workflowName, meta, definitionBytes)
	if err != nil {
		return nil, err
	}
	if s.admin == nil {
		return nil, fmt.Errorf("admin client unavailable")
	}
	ctx, cancel := context.WithTimeout(context.Background(), adminRPCTimeout)
	defer cancel()
	return s.admin.PublishRuntimePackage(ctx, packageFiles)
}

func buildWorkflowPackageFiles(hiveID, workflowName string, meta WfRulesMetadata, definitionBytes []byte) (map[string]string, error) {
	runtimeName := "wf." + workflowName
	version := strconv.FormatUint(meta.Version, 10)

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

func (s *Service) publishWorkflowPackageLocal(workflowName string, meta WfRulesMetadata, definitionBytes []byte) (*PackagePublishResult, error) {
	runtimeName := workflowRuntimeName(workflowName)
	version := strconv.FormatUint(meta.Version, 10)
	packageFiles, err := buildWorkflowPackageFiles(s.cfg.HiveID, workflowName, meta, definitionBytes)
	if err != nil {
		return nil, err
	}
	packageDir := filepath.Join(s.cfg.DistRuntimeRoot, runtimeName, version)
	for relPath, contents := range packageFiles {
		targetPath := filepath.Join(packageDir, filepath.FromSlash(relPath))
		parentDir := filepath.Dir(targetPath)
		if err := os.MkdirAll(parentDir, 0o755); err != nil {
			return nil, err
		}
		if err := writeFileAtomic(targetPath, []byte(contents), 0o644); err != nil {
			return nil, err
		}
	}
	if err := s.updateRuntimeManifest(runtimeName, version); err != nil {
		return nil, err
	}
	return &PackagePublishResult{
		RuntimeName: runtimeName,
		Version:     version,
		PackagePath: packageDir,
	}, nil
}

func (s *Service) PurgeWorkflowPackages(workflowName string, preserveBoundVersion bool) error {
	runtimeName := workflowRuntimeName(workflowName)
	runtimeDir := filepath.Join(s.cfg.DistRuntimeRoot, runtimeName)
	entries, err := os.ReadDir(runtimeDir)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return err
	}

	keep := map[string]struct{}{}
	if meta, err := s.store.ReadCurrentMetadata(workflowName); err == nil {
		keep[strconv.FormatUint(meta.Version, 10)] = struct{}{}
	}
	if meta, err := s.store.ReadBackupMetadata(workflowName); err == nil {
		keep[strconv.FormatUint(meta.Version, 10)] = struct{}{}
	}
	if preserveBoundVersion {
		boundVersion, err := s.boundRuntimeVersionForWorkflow(workflowName)
		if err != nil {
			return err
		}
		if strings.TrimSpace(boundVersion) != "" {
			keep[strings.TrimSpace(boundVersion)] = struct{}{}
		}
	}

	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}
		version := entry.Name()
		if _, ok := keep[version]; ok {
			continue
		}
		if err := os.RemoveAll(filepath.Join(runtimeDir, version)); err != nil {
			return err
		}
	}

	remaining, err := runtimeVersionsOnDisk(runtimeDir)
	if err != nil {
		return err
	}
	if len(remaining) == 0 {
		_ = os.Remove(runtimeDir)
	}
	return s.updateRuntimeManifestAfterPurge(runtimeName, remaining, preferredCurrentVersion(keep))
}

func (s *Service) updateRuntimeManifest(runtimeName, version string) error {
	manifestPath := filepath.Join(s.cfg.DistRuntimeRoot, "manifest.json")
	manifest, err := loadRuntimeManifest(manifestPath)
	if err != nil {
		return err
	}
	entry := manifest.Runtimes[runtimeName]
	entry.PackageType = "workflow"
	entry.RuntimeBase = workflowRuntimeBase
	entry.Current = version
	entry.Available = appendIfMissing(entry.Available, version)
	sort.Strings(entry.Available)
	manifest.Runtimes[runtimeName] = entry
	manifest.SchemaVersion = 2
	manifest.Version = nextManifestVersion(manifest.Version, uint64(time.Now().UTC().UnixMilli()))
	manifest.UpdatedAt = time.Now().UTC().Format(timeRFC3339)
	manifest.Hash = nil
	return writeRuntimeManifest(manifestPath, manifest)
}

func (s *Service) updateRuntimeManifestAfterPurge(runtimeName string, available []string, preferredCurrent string) error {
	manifestPath := filepath.Join(s.cfg.DistRuntimeRoot, "manifest.json")
	manifest, err := loadRuntimeManifest(manifestPath)
	if err != nil {
		return err
	}
	entry, ok := manifest.Runtimes[runtimeName]
	if !ok && len(available) == 0 {
		return nil
	}
	if len(available) == 0 {
		delete(manifest.Runtimes, runtimeName)
	} else {
		entry.Available = available
		entry.Current = chooseManifestCurrent(available, preferredCurrent, entry.Current)
		entry.PackageType = "workflow"
		entry.RuntimeBase = workflowRuntimeBase
		manifest.Runtimes[runtimeName] = entry
	}
	manifest.SchemaVersion = 2
	manifest.Version = nextManifestVersion(manifest.Version, uint64(time.Now().UTC().UnixMilli()))
	manifest.UpdatedAt = time.Now().UTC().Format(timeRFC3339)
	manifest.Hash = nil
	return writeRuntimeManifest(manifestPath, manifest)
}

func writeRuntimeManifest(path string, manifest *runtimeManifest) error {
	data, err := json.MarshalIndent(manifest, "", "  ")
	if err != nil {
		return err
	}
	data = append(data, '\n')
	return writeFileAtomic(path, data, 0o644)
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

func appendIfMissing(values []string, target string) []string {
	for _, value := range values {
		if value == target {
			return values
		}
	}
	return append(values, target)
}

func runtimeVersionsOnDisk(runtimeDir string) ([]string, error) {
	entries, err := os.ReadDir(runtimeDir)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, err
	}
	versions := make([]string, 0, len(entries))
	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}
		versions = append(versions, entry.Name())
	}
	sort.Strings(versions)
	return versions, nil
}

func preferredCurrentVersion(keep map[string]struct{}) string {
	if len(keep) == 0 {
		return ""
	}
	versions := make([]string, 0, len(keep))
	for version := range keep {
		versions = append(versions, version)
	}
	sort.Strings(versions)
	return versions[len(versions)-1]
}

func chooseManifestCurrent(available []string, preferredCurrent string, fallbackCurrent string) string {
	if preferredCurrent != "" {
		for _, version := range available {
			if version == preferredCurrent {
				return preferredCurrent
			}
		}
	}
	if fallbackCurrent != "" {
		for _, version := range available {
			if version == fallbackCurrent {
				return fallbackCurrent
			}
		}
	}
	if len(available) == 0 {
		return ""
	}
	return available[len(available)-1]
}

func workflowRuntimeName(workflowName string) string {
	return "wf." + workflowName
}

func nextManifestVersion(current, now uint64) uint64 {
	if now > current {
		return now
	}
	return current + 1
}
