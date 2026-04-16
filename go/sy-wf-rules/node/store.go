package node

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

type Store struct {
	root string
}

func NewStore(root string) *Store {
	return &Store{root: filepath.Clean(root)}
}

func (s *Store) Root() string {
	return s.root
}

func (s *Store) workflowDir(workflowName string) string {
	return filepath.Join(s.root, workflowName)
}

func (s *Store) slotDir(workflowName, slot string) string {
	return filepath.Join(s.workflowDir(workflowName), slot)
}

func (s *Store) definitionPath(workflowName, slot string) string {
	return filepath.Join(s.slotDir(workflowName, slot), "definition.json")
}

func (s *Store) metadataPath(workflowName, slot string) string {
	return filepath.Join(s.slotDir(workflowName, slot), "metadata.json")
}

func (s *Store) EnsureWorkflowDir(workflowName string) error {
	return os.MkdirAll(s.workflowDir(workflowName), 0o755)
}

func (s *Store) WriteSlot(workflowName, slot string, definitionBytes []byte, meta WfRulesMetadata) error {
	if err := s.EnsureWorkflowDir(workflowName); err != nil {
		return err
	}
	slotDir := s.slotDir(workflowName, slot)
	if err := os.MkdirAll(slotDir, 0o755); err != nil {
		return err
	}
	if err := writeFileAtomic(s.definitionPath(workflowName, slot), definitionBytes, 0o644); err != nil {
		return err
	}
	metaBytes, err := json.MarshalIndent(meta, "", "  ")
	if err != nil {
		return err
	}
	metaBytes = append(metaBytes, '\n')
	return writeFileAtomic(s.metadataPath(workflowName, slot), metaBytes, 0o644)
}

func (s *Store) WriteStaged(workflowName string, definitionBytes []byte, meta WfRulesMetadata) error {
	return s.WriteSlot(workflowName, "staged", definitionBytes, meta)
}

func (s *Store) ReadCurrentMetadata(workflowName string) (*WfRulesMetadata, error) {
	return s.ReadMetadata(workflowName, "current")
}

func (s *Store) ReadStagedMetadata(workflowName string) (*WfRulesMetadata, error) {
	return s.ReadMetadata(workflowName, "staged")
}

func (s *Store) ReadBackupMetadata(workflowName string) (*WfRulesMetadata, error) {
	return s.ReadMetadata(workflowName, "backup")
}

func (s *Store) ReadCurrentDefinition(workflowName string) ([]byte, error) {
	return s.ReadDefinitionBytes(workflowName, "current")
}

func (s *Store) ReadStagedDefinition(workflowName string) ([]byte, error) {
	return s.ReadDefinitionBytes(workflowName, "staged")
}

func (s *Store) ReadBackupDefinition(workflowName string) ([]byte, error) {
	return s.ReadDefinitionBytes(workflowName, "backup")
}

func (s *Store) ReadMetadata(workflowName, slot string) (*WfRulesMetadata, error) {
	data, err := os.ReadFile(s.metadataPath(workflowName, slot))
	if err != nil {
		return nil, err
	}
	var meta WfRulesMetadata
	if err := json.Unmarshal(data, &meta); err != nil {
		return nil, err
	}
	return &meta, nil
}

func (s *Store) ReadDefinitionBytes(workflowName, slot string) ([]byte, error) {
	return os.ReadFile(s.definitionPath(workflowName, slot))
}

func (s *Store) NextVersion(workflowName string) uint64 {
	maxVersion := uint64(0)
	for _, slot := range []string{"staged", "current", "backup"} {
		meta, err := s.ReadMetadata(workflowName, slot)
		if err == nil && meta.Version > maxVersion {
			maxVersion = meta.Version
		}
	}
	return maxVersion + 1
}

func (s *Store) WorkflowExists(workflowName string) bool {
	for _, slot := range []string{"current", "staged", "backup"} {
		if _, err := os.Stat(s.metadataPath(workflowName, slot)); err == nil {
			return true
		}
	}
	return false
}

func (s *Store) ListWorkflows() ([]string, error) {
	entries, err := os.ReadDir(s.root)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, err
	}
	out := make([]string, 0, len(entries))
	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}
		name := strings.TrimSpace(entry.Name())
		if name == "" {
			continue
		}
		out = append(out, name)
	}
	sort.Strings(out)
	return out, nil
}

func (s *Store) RotateToApply(workflowName string) error {
	workflowDir := s.workflowDir(workflowName)
	if err := os.MkdirAll(workflowDir, 0o755); err != nil {
		return err
	}
	backupDir := s.slotDir(workflowName, "backup")
	currentDir := s.slotDir(workflowName, "current")
	stagedDir := s.slotDir(workflowName, "staged")

	if _, err := os.Stat(stagedDir); err != nil {
		return err
	}
	_ = os.RemoveAll(backupDir)
	if _, err := os.Stat(currentDir); err == nil {
		if err := os.Rename(currentDir, backupDir); err != nil {
			return err
		}
	}
	return os.Rename(stagedDir, currentDir)
}

func (s *Store) RotateToRollback(workflowName string) error {
	workflowDir := s.workflowDir(workflowName)
	if err := os.MkdirAll(workflowDir, 0o755); err != nil {
		return err
	}
	backupDir := s.slotDir(workflowName, "backup")
	currentDir := s.slotDir(workflowName, "current")
	stagedDir := s.slotDir(workflowName, "staged")

	if _, err := os.Stat(backupDir); err != nil {
		return err
	}
	_ = os.RemoveAll(stagedDir)
	if _, err := os.Stat(currentDir); err == nil {
		if err := os.Rename(currentDir, stagedDir); err != nil {
			return err
		}
	}
	return os.Rename(backupDir, currentDir)
}

func (s *Store) DeleteWorkflowState(workflowName string) error {
	return os.RemoveAll(s.workflowDir(workflowName))
}

func HashDefinition(definitionBytes []byte) string {
	sum := sha256.Sum256(definitionBytes)
	return "sha256:" + hex.EncodeToString(sum[:])
}

func writeFileAtomic(path string, data []byte, perm os.FileMode) error {
	tmp := path + ".tmp"
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	if err := os.WriteFile(tmp, data, perm); err != nil {
		return err
	}
	if err := os.Rename(tmp, path); err != nil {
		return err
	}
	return nil
}

func normalizeDefinitionBytes(definition any) ([]byte, error) {
	data, err := json.MarshalIndent(definition, "", "  ")
	if err != nil {
		return nil, fmt.Errorf("marshal definition: %w", err)
	}
	data = append(data, '\n')
	return data, nil
}
