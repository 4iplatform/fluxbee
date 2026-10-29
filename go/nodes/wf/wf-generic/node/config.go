package node

import (
	"bytes"
	"encoding/json"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"strings"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// The WF node runs on the SDK's managed control plane, like the AI and IO nodes: config.json is the
// one source of truth, the orchestrator's `_system` block is read only for what the node needs and
// copied untouched when the node persists, and a config or workflow definition the node cannot use
// leaves it up in FAILED_CONFIG, saying why, instead of exiting (docs/wf-standard-path-v1.md).

const (
	// ErrCodeInvalidDefinition: the workflow definition the node would run is missing or invalid.
	ErrCodeInvalidDefinition = "invalid_definition"
	// ErrCodeManagedPackagePathLocked: a CONFIG_SET tried to point a package-bound node elsewhere.
	ErrCodeManagedPackagePathLocked = "managed_package_path_locked"

	defaultGCRetentionDays   = 7
	defaultGCIntervalSeconds = 3600
)

// OperatorConfig is the part of config.json an operator configures; the orchestrator's `_system`
// is not part of it. An unknown key is refused, so a typo is never silently ignored.
type OperatorConfig struct {
	WorkflowDefinitionPath string `json:"workflow_definition_path,omitempty"`
	DBPath                 string `json:"db_path,omitempty"`
	SYTimerL2Name          string `json:"sy_timer_l2_name,omitempty"`
	TenantID               string `json:"tenant_id,omitempty"`
	GCRetentionDays        int    `json:"gc_retention_days,omitempty"`
	GCIntervalSeconds      int    `json:"gc_interval_seconds,omitempty"`
}

// Config is what a WF node runs with: the operator's config made concrete with what `_system` says
// about the node.
type Config struct {
	WorkflowDefinitionPath string
	DBPath                 string
	SYTimerL2Name          string
	TenantID               string
	GCRetentionDays        int
	GCIntervalSeconds      int
	// PackagePath is `_system.package_path`. When set the node runs a workflow package and the
	// package's flow/definition.json is its definition (managed package mode).
	PackagePath string
	// NodeName and HiveID: `_system.node_name` and `_system.hive_id`, or the managed node name.
	NodeName string
	HiveID   string
}

// IsManagedPackageMode reports whether the node runs a workflow package: the orchestrator stamped
// `_system.package_path`, and the definition is the package's, not operator-controlled.
func (c *Config) IsManagedPackageMode() bool {
	return c != nil && c.PackagePath != ""
}

// PackageDefinitionPath is the package's definition in managed package mode, "" otherwise.
func (c *Config) PackageDefinitionPath() string {
	if !c.IsManagedPackageMode() {
		return ""
	}
	return packageDefinitionPath(c.PackagePath)
}

func packageDefinitionPath(packagePath string) string {
	return filepath.Join(packagePath, "flow", "definition.json")
}

// Materialized is a config the node accepted: the operator's part, what it runs with, and the
// workflow definition, already loaded and validated.
type Materialized struct {
	Operator        OperatorConfig
	Config          Config
	Definition      *WorkflowDefinition
	DefinitionBytes []byte
}

// wfContract is the WF node's config contract for the SDK control plane.
type wfContract struct {
	configPath string
	// nodeName is the managed node name the orchestrator launched it as ("" in local dev).
	nodeName string
	clock    ClockFunc
}

// RunsOnSpawnConfig: a WF node needs no operator config; its workflow comes from its package.
func (c wfContract) RunsOnSpawnConfig() bool { return true }

func (c wfContract) ValidateAndMaterialize(effective map[string]any, system map[string]any) (any, error) {
	operator, err := decodeOperatorConfig(effective)
	if err != nil {
		return nil, sdk.InvalidConfig("%v", err)
	}
	cfg, err := c.materialize(operator, system)
	if err != nil {
		return nil, err
	}
	definitionBytes, err := os.ReadFile(cfg.WorkflowDefinitionPath)
	if err != nil {
		return nil, &sdk.ControlPlaneError{Code: ErrCodeInvalidDefinition, Message: fmt.Sprintf("read workflow definition: %v", err)}
	}
	definition, err := LoadDefinitionBytes(definitionBytes, cfg.WorkflowDefinitionPath, c.clock)
	if err != nil {
		return nil, &sdk.ControlPlaneError{Code: ErrCodeInvalidDefinition, Message: err.Error()}
	}
	return &Materialized{Operator: operator, Config: cfg, Definition: definition, DefinitionBytes: definitionBytes}, nil
}

// decodeOperatorConfig reads the operator's config strictly: an unknown key is an error.
func decodeOperatorConfig(effective map[string]any) (OperatorConfig, error) {
	var operator OperatorConfig
	raw, err := json.Marshal(effective)
	if err != nil {
		return operator, err
	}
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&operator); err != nil {
		return operator, err
	}
	operator.WorkflowDefinitionPath = strings.TrimSpace(operator.WorkflowDefinitionPath)
	operator.DBPath = strings.TrimSpace(operator.DBPath)
	operator.SYTimerL2Name = strings.TrimSpace(operator.SYTimerL2Name)
	operator.TenantID = strings.TrimSpace(operator.TenantID)
	return operator, nil
}

// materialize makes the operator's config concrete: the package's definition in managed package
// mode, the instance store next to config.json, SY.timer of the node's hive, and the GC defaults.
func (c wfContract) materialize(operator OperatorConfig, system map[string]any) (Config, error) {
	cfg := Config{
		DBPath:            operator.DBPath,
		SYTimerL2Name:     operator.SYTimerL2Name,
		TenantID:          operator.TenantID,
		GCRetentionDays:   operator.GCRetentionDays,
		GCIntervalSeconds: operator.GCIntervalSeconds,
		PackagePath:       sdk.ManagedSystemString(system, "package_path"),
		NodeName:          sdk.ManagedSystemString(system, "node_name"),
		HiveID:            sdk.ManagedSystemString(system, "hive_id"),
	}
	if cfg.NodeName == "" {
		cfg.NodeName = strings.TrimSpace(c.nodeName)
	}
	if cfg.HiveID == "" {
		if _, hive, ok := strings.Cut(cfg.NodeName, "@"); ok {
			cfg.HiveID = strings.TrimSpace(hive)
		}
	}
	if cfg.TenantID == "" {
		cfg.TenantID = sdk.ManagedSystemString(system, "tenant_id")
	}
	if cfg.IsManagedPackageMode() {
		cfg.WorkflowDefinitionPath = cfg.PackageDefinitionPath()
		if operator.WorkflowDefinitionPath != "" && operator.WorkflowDefinitionPath != cfg.WorkflowDefinitionPath {
			log.Printf("wf-generic: managed package mode: ignoring workflow_definition_path=%q, the definition is the package's %q", operator.WorkflowDefinitionPath, cfg.WorkflowDefinitionPath)
		}
	} else {
		cfg.WorkflowDefinitionPath = operator.WorkflowDefinitionPath
	}
	if cfg.DBPath == "" && c.configPath != "" {
		cfg.DBPath = filepath.Join(filepath.Dir(c.configPath), "wf_instances.db")
	}
	if cfg.SYTimerL2Name == "" && cfg.HiveID != "" {
		cfg.SYTimerL2Name = "SY.timer@" + cfg.HiveID
	}
	if cfg.GCRetentionDays <= 0 {
		cfg.GCRetentionDays = defaultGCRetentionDays
	}
	if cfg.GCIntervalSeconds <= 0 {
		cfg.GCIntervalSeconds = defaultGCIntervalSeconds
	}
	switch {
	case cfg.WorkflowDefinitionPath == "":
		return cfg, sdk.InvalidConfig("workflow_definition_path is required (or _system.package_path for a workflow package)")
	case cfg.DBPath == "":
		return cfg, sdk.InvalidConfig("db_path is required")
	case cfg.SYTimerL2Name == "":
		return cfg, sdk.InvalidConfig("sy_timer_l2_name is required (or the node's hive, to use its SY.timer)")
	case cfg.HiveID != "" && cfg.SYTimerL2Name != "SY.timer@"+cfg.HiveID:
		// The node takes a TIMER_FIRED only from SY.timer of its own hive (FINDINGS A-119): a timer
		// scheduled on any other would never fire.
		return cfg, sdk.InvalidConfig("sy_timer_l2_name must be SY.timer@%s, the SY.timer of the node's hive, not %q", cfg.HiveID, cfg.SYTimerL2Name)
	}
	return cfg, nil
}

// ControlPlane is the WF node's config plane: the SDK state and what the node needs to answer
// CONFIG_GET and CONFIG_SET.
type ControlPlane struct {
	ConfigPath string
	State      sdk.ManagedControlPlaneState
	// NodeName is the managed node name, or `_system.node_name` in local dev; "" when neither.
	NodeName string
	// PackagePath is `_system.package_path` as read at boot, so CONFIG_GET can describe a node
	// whose config was rejected.
	PackagePath string
	// pendingVersion is the version of a CONFIG_SET persisted but not running yet: WF applies a
	// config at boot, so a restart applies it.
	pendingVersion uint64
	contract       wfContract
}

// BootstrapControlPlane reads the node's config.json. A config the node cannot use is a state
// (FAILED_CONFIG, with the reason), never an error; an error means the file cannot be read.
func BootstrapControlPlane(configPath, nodeName string, clock ClockFunc) (*ControlPlane, error) {
	if clock == nil {
		clock = defaultClock
	}
	contract := wfContract{configPath: configPath, nodeName: nodeName, clock: clock}
	state, err := sdk.BootstrapManagedControlPlane(configPath, contract)
	if err != nil {
		return nil, err
	}
	plane := &ControlPlane{ConfigPath: configPath, State: state, NodeName: strings.TrimSpace(nodeName), contract: contract}
	system := readSystemBlock(configPath)
	if plane.NodeName == "" {
		plane.NodeName = sdk.ManagedSystemString(system, "node_name")
	}
	plane.PackagePath = sdk.ManagedSystemString(system, "package_path")
	return plane, nil
}

// Configured is what the node runs with, or nil when its config is not in use.
func (p *ControlPlane) Configured() *Materialized {
	if p == nil || p.State.CurrentState != sdk.ManagedConfigured {
		return nil
	}
	materialized, _ := p.State.EffectiveConfig.(*Materialized)
	return materialized
}

// latestVersion is the newest config version the node knows of: running, persisted by a CONFIG_SET,
// or bumped on disk by the orchestrator's set_node_config, which does not tell the node.
func (p *ControlPlane) latestVersion() uint64 {
	latest := p.State.ConfigVersion
	if p.pendingVersion > latest {
		latest = p.pendingVersion
	}
	if onDisk := systemConfigVersion(readSystemBlock(p.ConfigPath)); onDisk > latest {
		latest = onDisk
	}
	return latest
}

// readSystemBlock is config.json's `_system`, empty when the file cannot be read or parsed.
func readSystemBlock(configPath string) map[string]any {
	data, err := os.ReadFile(configPath)
	if err != nil {
		return map[string]any{}
	}
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	var document any
	if err := decoder.Decode(&document); err != nil {
		return map[string]any{}
	}
	return sdk.ManagedSystemBlock(document)
}

func systemConfigVersion(system map[string]any) uint64 {
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
	return 0
}

// persistableOperatorConfig is what a CONFIG_SET writes: the operator's config, without the
// definition path of a package-bound node (the package decides it, and changes with each deploy).
func persistableOperatorConfig(operator OperatorConfig, packageBound bool) (map[string]any, error) {
	if packageBound {
		operator.WorkflowDefinitionPath = ""
	}
	raw, err := json.Marshal(operator)
	if err != nil {
		return nil, err
	}
	out := map[string]any{}
	if err := json.Unmarshal(raw, &out); err != nil {
		return nil, err
	}
	return out, nil
}

// notConfiguredReason is what a request the node cannot serve without a config is told.
func (p *ControlPlane) notConfiguredReason() string {
	if p == nil || p.State.LastError == nil {
		return "the WF node has no usable config"
	}
	return fmt.Sprintf("the WF node's config is not in use (%s)", p.State.LastError.Error())
}
