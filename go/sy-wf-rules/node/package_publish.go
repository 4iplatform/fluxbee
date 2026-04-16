package node

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
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
	runtimeName := "wf." + workflowName
	version := strconv.FormatUint(meta.Version, 10)
	packageDir := filepath.Join(s.cfg.DistRuntimeRoot, runtimeName, version)
	flowDir := filepath.Join(packageDir, "flow")
	if err := os.MkdirAll(flowDir, 0o755); err != nil {
		return nil, err
	}

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
	if err := writeFileAtomic(filepath.Join(packageDir, "package.json"), packageBytes, 0o644); err != nil {
		return nil, err
	}
	if err := writeFileAtomic(filepath.Join(flowDir, "definition.json"), definitionBytes, 0o644); err != nil {
		return nil, err
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
	data, err := json.MarshalIndent(manifest, "", "  ")
	if err != nil {
		return err
	}
	data = append(data, '\n')
	return writeFileAtomic(manifestPath, data, 0o644)
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

func nextManifestVersion(current, now uint64) uint64 {
	if now > current {
		return now
	}
	return current + 1
}
