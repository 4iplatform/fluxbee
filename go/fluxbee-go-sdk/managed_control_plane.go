package sdk

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
)

// The control plane of an orchestrator-managed node, for the Go nodes: a port of the parts of the
// Rust SDK's managed_control_plane.rs that nodes use (boot, persist, lifecycle, health).
//
// `<root>/<KIND>/<node@hive>/config.json` is the one source of truth. The orchestrator writes it on
// run_node and set_node_config; the node writes it on a CONFIG_SET. The `_system` block is the
// orchestrator's: a node reads from it only what it needs, never validates it, and copies it as it
// is when it persists, so a key the orchestrator adds can never stop a node.

const (
	// DefaultManagedNodesRoot is where the orchestrator writes managed node instance dirs.
	DefaultManagedNodesRoot = "/var/lib/fluxbee/nodes"
	// ManagedNodeNameEnv carries the node's full name (`KIND.name@hive`), set by the orchestrator.
	ManagedNodeNameEnv = "FLUXBEE_NODE_NAME"

	managedSystemKey = "_system"
)

// ManagedLifecycleState is the state of a node's config plane. It is not the process state: a node
// can be RUNNING while its config plane is FAILED_CONFIG.
type ManagedLifecycleState string

const (
	ManagedUnconfigured ManagedLifecycleState = "UNCONFIGURED"
	ManagedConfigured   ManagedLifecycleState = "CONFIGURED"
	ManagedFailedConfig ManagedLifecycleState = "FAILED_CONFIG"
)

// Codes the control plane itself reports.
const (
	ErrCodeInvalidConfig = "invalid_config"
	ErrCodeInternal      = "internal_error"
)

// ControlPlaneError is a config-plane error as a node reports it: `{code, message}`.
type ControlPlaneError struct {
	Code    string `json:"code"`
	Message string `json:"message"`
}

func (e *ControlPlaneError) Error() string {
	if e == nil {
		return ""
	}
	if e.Message == "" {
		return e.Code
	}
	return e.Code + ": " + e.Message
}

// InvalidConfig is the error a contract returns for a config it rejects.
func InvalidConfig(format string, args ...any) *ControlPlaneError {
	return &ControlPlaneError{Code: ErrCodeInvalidConfig, Message: fmt.Sprintf(format, args...)}
}

// ManagedControlPlaneState is what a node knows about its own config.
type ManagedControlPlaneState struct {
	CurrentState  ManagedLifecycleState
	SchemaVersion uint32
	ConfigVersion uint64
	// EffectiveConfig is what the contract materialized; set only when CONFIGURED, so a rejected
	// config is never used.
	EffectiveConfig any
	LastError       *ControlPlaneError
}

// ManagedNodeConfigContract is what a node plugs into the control plane.
type ManagedNodeConfigContract interface {
	// ValidateAndMaterialize checks the operator's config (`_system` already removed; numbers arrive
	// as json.Number) and returns it ready to use. system is the orchestrator's `_system` block,
	// read-only: take what the node needs and ignore the rest. An error is a rejection
	// (FAILED_CONFIG): a *ControlPlaneError keeps its code, any other error reads invalid_config.
	ValidateAndMaterialize(effective map[string]any, system map[string]any) (any, error)
	// RunsOnSpawnConfig reports whether the node runs on what the orchestrator gives it at spawn.
	// When false (a node an operator has to configure), a config with only the orchestrator's
	// spawn keys is UNCONFIGURED; when true (the WF node, whose workflow comes from its package),
	// it is validated like any other.
	RunsOnSpawnConfig() bool
}

// orchestratorReservedKeys are the top-level keys the orchestrator writes at spawn whatever the
// operator configured; their presence alone is not an operator config.
var orchestratorReservedKeys = map[string]bool{"tenant_id": true}

// ManagedNodeInstanceDir is `<root>/<KIND>/<node@hive>` for a full node name.
func ManagedNodeInstanceDir(nodeName, root string) (string, error) {
	name := strings.TrimSpace(nodeName)
	if name == "" {
		return "", errors.New("managed node name is empty")
	}
	local, hive, ok := strings.Cut(name, "@")
	if !ok || strings.TrimSpace(hive) == "" {
		return "", fmt.Errorf("managed node name %q has no @hive", name)
	}
	kind, _, _ := strings.Cut(local, ".")
	kind = strings.TrimSpace(kind)
	if kind == "" {
		return "", fmt.Errorf("managed node name %q has no kind", name)
	}
	return filepath.Join(root, kind, name), nil
}

// ManagedNodeConfigPath is the config.json of a managed node.
func ManagedNodeConfigPath(nodeName, root string) (string, error) {
	dir, err := ManagedNodeInstanceDir(nodeName, root)
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "config.json"), nil
}

// ExtractEffectiveConfig is the config a contract sees: the `config` object when the document wraps
// one, the document itself otherwise, without `_system` either way. The result may not be an
// object; the caller decides what that means.
func ExtractEffectiveConfig(root any) any {
	candidate := root
	if object, ok := root.(map[string]any); ok {
		if wrapped, present := object["config"]; present {
			candidate = wrapped
		}
	}
	object, ok := candidate.(map[string]any)
	if !ok {
		return candidate
	}
	effective := make(map[string]any, len(object))
	for key, value := range object {
		if key != managedSystemKey {
			effective[key] = value
		}
	}
	return effective
}

// ManagedSystemBlock is a copy of the document's `_system` object, empty when there is none.
func ManagedSystemBlock(root any) map[string]any {
	system := map[string]any{}
	object, ok := root.(map[string]any)
	if !ok {
		return system
	}
	if block, ok := object[managedSystemKey].(map[string]any); ok {
		for key, value := range block {
			system[key] = value
		}
	}
	return system
}

// ManagedSystemString reads a string from `_system`, trimmed; "" when absent or not a string.
func ManagedSystemString(system map[string]any, key string) string {
	value, _ := system[key].(string)
	return strings.TrimSpace(value)
}

// managedConfigVersion is `_system.config_version`, 1 when absent (as the orchestrator starts).
func managedConfigVersion(system map[string]any) uint64 {
	switch value := system["config_version"].(type) {
	case json.Number:
		if parsed, err := value.Int64(); err == nil && parsed > 0 {
			return uint64(parsed)
		}
	case float64:
		if value > 0 {
			return uint64(value)
		}
	}
	return 1
}

func hasOperatorContent(effective map[string]any) bool {
	for key := range effective {
		if !orchestratorReservedKeys[key] {
			return true
		}
	}
	return false
}

// decodeManagedDocument parses a config document keeping numbers exact (json.Number), so `_system`
// goes back to disk as it came.
func decodeManagedDocument(data []byte) (any, error) {
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	var document any
	if err := decoder.Decode(&document); err != nil {
		return nil, err
	}
	if decoder.More() {
		return nil, errors.New("trailing data after the JSON document")
	}
	return document, nil
}

func failedConfig(version uint64, code, message string) ManagedControlPlaneState {
	return ManagedControlPlaneState{
		CurrentState:  ManagedFailedConfig,
		SchemaVersion: 1,
		ConfigVersion: version,
		LastError:     &ControlPlaneError{Code: code, Message: message},
	}
}

// BootstrapManagedControlPlane reads a managed node's config.json and decides its config state:
//   - no file: UNCONFIGURED;
//   - not JSON, or not an object: FAILED_CONFIG invalid_config;
//   - only the orchestrator's spawn keys: UNCONFIGURED, unless the contract runs on them;
//   - otherwise the contract decides: CONFIGURED, or FAILED_CONFIG with its error.
//
// It never fails on a bad config, so the node can stay up and say what is wrong; an error means
// the file exists but cannot be read.
func BootstrapManagedControlPlane(configPath string, contract ManagedNodeConfigContract) (ManagedControlPlaneState, error) {
	data, err := os.ReadFile(configPath)
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return ManagedControlPlaneState{CurrentState: ManagedUnconfigured}, nil
		}
		return ManagedControlPlaneState{}, fmt.Errorf("read %s: %w", configPath, err)
	}
	document, err := decodeManagedDocument(data)
	if err != nil {
		return failedConfig(0, ErrCodeInvalidConfig, fmt.Sprintf("config.json is not valid JSON: %v", err)), nil
	}
	system := ManagedSystemBlock(document)
	version := managedConfigVersion(system)
	effective, ok := ExtractEffectiveConfig(document).(map[string]any)
	if !ok {
		return failedConfig(version, ErrCodeInvalidConfig, fmt.Sprintf("config is not an object: %s", configPath)), nil
	}
	if !contract.RunsOnSpawnConfig() && !hasOperatorContent(effective) {
		return ManagedControlPlaneState{
			CurrentState:  ManagedUnconfigured,
			SchemaVersion: 1,
			ConfigVersion: version,
		}, nil
	}
	materialized, err := contract.ValidateAndMaterialize(effective, system)
	if err != nil {
		code, message := ErrCodeInvalidConfig, err.Error()
		var planeErr *ControlPlaneError
		if errors.As(err, &planeErr) {
			code, message = planeErr.Code, planeErr.Message
		}
		return failedConfig(version, code, message), nil
	}
	return ManagedControlPlaneState{
		CurrentState:    ManagedConfigured,
		SchemaVersion:   1,
		ConfigVersion:   version,
		EffectiveConfig: materialized,
	}, nil
}

// PersistManagedConfig writes the node's config after a CONFIG_SET. `_system` is copied from the
// file as it is, with only `_system.config_version` set to configVersion; every other top-level
// key is the effective config (an object is written flat, anything else under `config`). The write
// is atomic (a temporary file renamed over the old one) and keeps the file's mode (0600 when new,
// as the orchestrator writes it). A file that exists but cannot be read or parsed is refused, not
// overwritten: its `_system` would be lost, and the orchestrator could no longer start the node.
func PersistManagedConfig(configPath string, configVersion uint64, effective any) error {
	system := map[string]any{}
	mode := fs.FileMode(0o600)
	if info, err := os.Stat(configPath); err == nil {
		mode = info.Mode().Perm()
		data, err := os.ReadFile(configPath)
		if err != nil {
			return fmt.Errorf("read %s: %w", configPath, err)
		}
		document, err := decodeManagedDocument(data)
		if err != nil {
			return fmt.Errorf("%s is not valid JSON, refusing to write over its _system: %w", configPath, err)
		}
		system = ManagedSystemBlock(document)
	} else if !errors.Is(err, fs.ErrNotExist) {
		return fmt.Errorf("stat %s: %w", configPath, err)
	}
	system["config_version"] = configVersion

	var document map[string]any
	if object, ok := effective.(map[string]any); ok {
		document = make(map[string]any, len(object)+1)
		for key, value := range object {
			document[key] = value
		}
	} else {
		document = map[string]any{"config": effective}
	}
	document[managedSystemKey] = system

	data, err := json.MarshalIndent(document, "", "  ")
	if err != nil {
		return fmt.Errorf("encode config: %w", err)
	}
	data = append(data, '\n')
	dir := filepath.Dir(configPath)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return fmt.Errorf("create %s: %w", dir, err)
	}
	tmp, err := os.CreateTemp(dir, ".config.json.tmp.*")
	if err != nil {
		return fmt.Errorf("create temporary config: %w", err)
	}
	tmpPath := tmp.Name()
	defer os.Remove(tmpPath)
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		return fmt.Errorf("write temporary config: %w", err)
	}
	if err := tmp.Chmod(mode); err != nil {
		tmp.Close()
		return fmt.Errorf("chmod temporary config: %w", err)
	}
	if err := tmp.Sync(); err != nil {
		tmp.Close()
		return fmt.Errorf("sync temporary config: %w", err)
	}
	if err := tmp.Close(); err != nil {
		return fmt.Errorf("close temporary config: %w", err)
	}
	if err := os.Rename(tmpPath, configPath); err != nil {
		return fmt.Errorf("replace %s: %w", configPath, err)
	}
	return nil
}

// ManagedHealthState is the health a managed node reports for its config state: a node that is not
// configured, or whose config was rejected, refuses work and is not healthy.
func ManagedHealthState(state ManagedControlPlaneState) string {
	if state.CurrentState == ManagedConfigured {
		return "HEALTHY"
	}
	return "DEGRADED"
}
