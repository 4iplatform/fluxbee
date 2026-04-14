package node

import (
	"context"
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"strings"
	"time"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// Config is the runtime configuration for a wf-generic node instance.
type Config struct {
	WorkflowDefinitionPath string `json:"workflow_definition_path"`
	DBPath                 string `json:"db_path"`
	SYTimerL2Name          string `json:"sy_timer_l2_name"`
	TenantID               string `json:"tenant_id,omitempty"`
	GCRetentionDays        int    `json:"gc_retention_days"`
	GCIntervalSeconds      int    `json:"gc_interval_seconds"`
	System                 *ManagedSystemConfig `json:"_system,omitempty"`
}

type ManagedSystemConfig struct {
	ManagedBy      string `json:"managed_by,omitempty"`
	NodeName       string `json:"node_name,omitempty"`
	HiveID         string `json:"hive_id,omitempty"`
	RelaunchOnBoot bool   `json:"relaunch_on_boot,omitempty"`
	ConfigVersion  uint64 `json:"config_version,omitempty"`
	CreatedAtMS    int64  `json:"created_at_ms,omitempty"`
	UpdatedAtMS    int64  `json:"updated_at_ms,omitempty"`
	Runtime        string `json:"runtime,omitempty"`
	RuntimeVersion string `json:"runtime_version,omitempty"`
	RuntimeBase    string `json:"runtime_base,omitempty"`
	PackagePath    string `json:"package_path,omitempty"`
	IlkID          string `json:"ilk_id,omitempty"`
	TenantID       string `json:"tenant_id,omitempty"`
}

// LoadConfig reads and validates a config.json file.
func LoadConfig(path string) (*Config, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read config %q: %w", path, err)
	}
	payload, err := extractConfigPayload(data)
	if err != nil {
		return nil, fmt.Errorf("parse config %q: %w", path, err)
	}
	var cfg Config
	dec := json.NewDecoder(bytesReader(payload))
	dec.DisallowUnknownFields()
	if err := dec.Decode(&cfg); err != nil {
		return nil, fmt.Errorf("parse config %q: %w", path, err)
	}
	if cfg.WorkflowDefinitionPath == "" {
		return nil, fmt.Errorf("config: workflow_definition_path is required")
	}
	if cfg.DBPath == "" {
		return nil, fmt.Errorf("config: db_path is required")
	}
	if cfg.SYTimerL2Name == "" {
		return nil, fmt.Errorf("config: sy_timer_l2_name is required")
	}
	if cfg.GCRetentionDays <= 0 {
		cfg.GCRetentionDays = 7
	}
	if cfg.GCIntervalSeconds <= 0 {
		cfg.GCIntervalSeconds = 3600
	}
	return &cfg, nil
}

func extractConfigPayload(data []byte) ([]byte, error) {
	var raw map[string]json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		return nil, err
	}

	configPayload, hasWrappedConfig := raw["config"]
	if !hasWrappedConfig {
		if _, hasTopLevelConfigVersion := raw["config_version"]; !hasTopLevelConfigVersion {
			return data, nil
		}
	}

	cfgMap := make(map[string]json.RawMessage)
	if hasWrappedConfig {
		if err := json.Unmarshal(configPayload, &cfgMap); err != nil {
			return nil, fmt.Errorf("parse wrapped config payload: %w", err)
		}
	}

	for _, key := range []string{
		"workflow_definition_path",
		"db_path",
		"sy_timer_l2_name",
		"gc_retention_days",
		"gc_interval_seconds",
		"tenant_id",
		"_system",
	} {
		if _, ok := cfgMap[key]; ok {
			continue
		}
		if value, exists := raw[key]; exists {
			cfgMap[key] = value
		}
	}

	if len(cfgMap) == 0 {
		return data, nil
	}
	merged, err := json.Marshal(cfgMap)
	if err != nil {
		return nil, fmt.Errorf("marshal wrapped config payload: %w", err)
	}
	return merged, nil
}

func ResolveManagedNodeName(cfg *Config) (string, error) {
	if cfg != nil && cfg.System != nil && strings.TrimSpace(cfg.System.NodeName) != "" {
		return strings.TrimSpace(cfg.System.NodeName), nil
	}
	value := strings.TrimSpace(os.Getenv("FLUXBEE_NODE_NAME"))
	if value != "" {
		return value, nil
	}
	return "", fmt.Errorf("managed node name unavailable: set _system.node_name in config or FLUXBEE_NODE_NAME in environment")
}

func ManagedConfigPathFromEnv() (string, error) {
	nodeName := strings.TrimSpace(os.Getenv("FLUXBEE_NODE_NAME"))
	if nodeName == "" {
		return "", fmt.Errorf("FLUXBEE_NODE_NAME is not set")
	}
	kind := managedNodeKind(nodeName)
	if kind == "" {
		return "", fmt.Errorf("cannot derive node kind from %q", nodeName)
	}
	return filepath.Join("/var/lib/fluxbee/nodes", kind, nodeName, "config.json"), nil
}

func managedNodeKind(nodeName string) string {
	base := strings.TrimSpace(nodeName)
	if at := strings.Index(base, "@"); at >= 0 {
		base = base[:at]
	}
	if dot := strings.Index(base, "."); dot >= 0 {
		base = base[:dot]
	}
	return strings.TrimSpace(base)
}

// RunOptions is the full set of options for node.Run().
type RunOptions struct {
	// ConfigPath is the path to config.json (used by the real entrypoint).
	ConfigPath string
	// Config can be supplied directly (used in tests / embedded use).
	Config *Config
	// Clock overrides time.Now for deterministic testing.
	Clock ClockFunc
	// SDKConfig is the SDK connection config. If zero-value, Run skips SDK connect.
	SDKConfig *sdk.NodeConfig
}

// Run is the main lifecycle function. It:
//  1. Loads config + definition
//  2. Opens SQLite
//  3. Connects to the router (if SDKConfig provided)
//  4. Recovers running instances + reconciles timers
//  5. Starts the GC goroutine
//  6. Enters the receive loop
func Run(ctx context.Context, opts RunOptions) error {
	// --- Config ---
	cfg := opts.Config
	if cfg == nil {
		if opts.ConfigPath == "" {
			return fmt.Errorf("either ConfigPath or Config is required")
		}
		var err error
		cfg, err = LoadConfig(opts.ConfigPath)
		if err != nil {
			return err
		}
	}

	clock := opts.Clock
	if clock == nil {
		clock = defaultClock
	}

	// --- Workflow definition ---
	defData, err := os.ReadFile(cfg.WorkflowDefinitionPath)
	if err != nil {
		return fmt.Errorf("read definition: %w", err)
	}
	def, err := LoadDefinitionBytes(defData, cfg.WorkflowDefinitionPath, clock)
	if err != nil {
		return fmt.Errorf("load definition: %w", err)
	}
	defHash := fmt.Sprintf("%x", sha256.Sum256(defData))
	log.Printf("wf: loaded definition workflow_type=%s states=%d", def.WorkflowType, len(def.States))

	// --- SQLite ---
	store, err := OpenStore(cfg.DBPath)
	if err != nil {
		return fmt.Errorf("open store: %w", err)
	}
	defer store.Close()

	// Persist the definition
	if err := store.UpsertDefinition(ctx, def.WorkflowType, string(defData), defHash, clock); err != nil {
		return fmt.Errorf("upsert definition: %w", err)
	}

	// --- SDK connection (optional for tests) ---
	var dispatcher Dispatcher
	var timerSender TimerSender
	var nodeUUID string

	if opts.SDKConfig != nil {
		sender, receiver, err := sdk.Connect(*opts.SDKConfig)
		if err != nil {
			return fmt.Errorf("sdk connect: %w", err)
		}
		defer sender.Close()

		nodeUUID = sender.UUID()
		dispatcher = NewSDKDispatcher(sender)

		timerClient, err := sdk.NewTimerClient(sender, receiver, sdk.TimerClientConfig{
			TimerNode: cfg.SYTimerL2Name,
		})
		if err != nil {
			return fmt.Errorf("create timer client: %w", err)
		}
		timerSender = NewSDKTimerSender(timerClient)

		// Recovery
		reg := NewInstanceRegistry()
		actx := ActionContext{
			Store:      store,
			Dispatcher: dispatcher,
			Timer:      timerSender,
			Clock:      clock,
		}
		if err := Recover(ctx, def, store, reg, actx, RecoverOptions{
			OwnerL2Name: sender.FullName(),
		}); err != nil {
			return fmt.Errorf("recovery failed: %w", err)
		}

		// GC
		gcInterval := time.Duration(cfg.GCIntervalSeconds) * time.Second
		StartGC(ctx, store, cfg.GCRetentionDays, gcInterval, clock)

		// Runtime
		rt := &NodeRuntime{
			Def:        def,
			DefJSON:    string(defData),
			Registry:   reg,
			Store:      store,
			ActCtx:     actx,
			NodeUUID:   nodeUUID,
			VersionStr: "1.0",
		}

		log.Printf("wf: entering receive loop for %s", sender.FullName())
		for {
			msg, err := receiver.Recv(ctx)
			if err != nil {
				if ctx.Err() != nil {
					return nil // clean shutdown
				}
				log.Printf("wf: recv error: %v", err)
				continue
			}
			if err := Dispatch(ctx, msg, rt); err != nil {
				log.Printf("wf: dispatch error: %v", err)
			}
		}
	}

	// No SDK config — just validate definition and return (useful for tests / dry-run)
	log.Printf("wf: no SDK config, exiting after definition load")
	return nil
}

// bytesReader is a thin wrapper to avoid importing bytes in this file.
func bytesReader(data []byte) interface{ Read([]byte) (int, error) } {
	return &byteReader{data: data}
}

type byteReader struct {
	data []byte
	pos  int
}

func (r *byteReader) Read(p []byte) (int, error) {
	if r.pos >= len(r.data) {
		return 0, fmt.Errorf("EOF")
	}
	n := copy(p, r.data[r.pos:])
	r.pos += n
	return n, nil
}
