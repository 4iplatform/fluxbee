package node

import (
	"context"
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"log"
	"os"
	"time"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// Config is the runtime configuration for a wf-generic node instance.
type Config struct {
	WorkflowDefinitionPath string `json:"workflow_definition_path"`
	DBPath                 string `json:"db_path"`
	SYTimerL2Name          string `json:"sy_timer_l2_name"`
	GCRetentionDays        int    `json:"gc_retention_days"`
	GCIntervalSeconds      int    `json:"gc_interval_seconds"`
}

// LoadConfig reads and validates a config.json file.
func LoadConfig(path string) (*Config, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read config %q: %w", path, err)
	}
	var cfg Config
	dec := json.NewDecoder(bytesReader(data))
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
