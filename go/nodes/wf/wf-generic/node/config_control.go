package node

import (
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

const wfConfigSchemaVersion uint32 = 1

func cloneConfig(cfg *Config) *Config {
	if cfg == nil {
		return nil
	}
	copy := *cfg
	copy.System = cloneManagedSystemConfig(cfg.System)
	return &copy
}

func cloneManagedSystemConfig(system *ManagedSystemConfig) *ManagedSystemConfig {
	if system == nil {
		return nil
	}
	copy := *system
	return &copy
}

func loadConfigFromPayload(payload []byte, configPath string, system *ManagedSystemConfig) (*Config, error) {
	var cfg Config
	dec := json.NewDecoder(bytes.NewReader(payload))
	dec.DisallowUnknownFields()
	if err := dec.Decode(&cfg); err != nil {
		return nil, err
	}
	if cfg.System == nil {
		cfg.System = cloneManagedSystemConfig(system)
	} else if system != nil {
		merged := *cloneManagedSystemConfig(system)
		if cfg.System.ManagedBy != "" {
			merged.ManagedBy = cfg.System.ManagedBy
		}
		if cfg.System.NodeName != "" {
			merged.NodeName = cfg.System.NodeName
		}
		if cfg.System.HiveID != "" {
			merged.HiveID = cfg.System.HiveID
		}
		if cfg.System.RelaunchOnBoot {
			merged.RelaunchOnBoot = true
		}
		if cfg.System.ConfigVersion != 0 {
			merged.ConfigVersion = cfg.System.ConfigVersion
		}
		if cfg.System.CreatedAtMS != 0 {
			merged.CreatedAtMS = cfg.System.CreatedAtMS
		}
		if cfg.System.UpdatedAtMS != 0 {
			merged.UpdatedAtMS = cfg.System.UpdatedAtMS
		}
		if cfg.System.Runtime != "" {
			merged.Runtime = cfg.System.Runtime
		}
		if cfg.System.RuntimeVersion != "" {
			merged.RuntimeVersion = cfg.System.RuntimeVersion
		}
		if cfg.System.RuntimeBase != "" {
			merged.RuntimeBase = cfg.System.RuntimeBase
		}
		if cfg.System.PackagePath != "" {
			merged.PackagePath = cfg.System.PackagePath
		}
		if cfg.System.IlkID != "" {
			merged.IlkID = cfg.System.IlkID
		}
		if cfg.System.TenantID != "" {
			merged.TenantID = cfg.System.TenantID
		}
		cfg.System = &merged
	}
	if err := finalizeConfig(&cfg, configPath); err != nil {
		return nil, err
	}
	if cfg.GCRetentionDays <= 0 {
		cfg.GCRetentionDays = 7
	}
	if cfg.GCIntervalSeconds <= 0 {
		cfg.GCIntervalSeconds = 3600
	}
	return &cfg, nil
}

func configVersion(cfg *Config) uint64 {
	if cfg == nil || cfg.System == nil {
		return 0
	}
	return cfg.System.ConfigVersion
}

func setConfigVersion(cfg *Config, version uint64) {
	if cfg == nil {
		return
	}
	if cfg.System == nil {
		cfg.System = &ManagedSystemConfig{}
	}
	cfg.System.ConfigVersion = version
}

func buildWFConfigGetPayload(nodeName string, cfg *Config) map[string]any {
	version := configVersion(cfg)
	state := "configured"
	if cfg == nil {
		state = "unconfigured"
	}
	return map[string]any{
		"ok":             true,
		"node_name":      nodeName,
		"state":          state,
		"schema_version": wfConfigSchemaVersion,
		"config_version": version,
		"effective_config": map[string]any{
			"workflow_definition_path": stringOrEmpty(cfg, func(c *Config) string { return c.WorkflowDefinitionPath }),
			"db_path":                  stringOrEmpty(cfg, func(c *Config) string { return c.DBPath }),
			"sy_timer_l2_name":         stringOrEmpty(cfg, func(c *Config) string { return c.SYTimerL2Name }),
			"tenant_id":                stringOrEmpty(cfg, func(c *Config) string { return c.TenantID }),
			"gc_retention_days":        intOrZero(cfg, func(c *Config) int { return c.GCRetentionDays }),
			"gc_interval_seconds":      intOrZero(cfg, func(c *Config) int { return c.GCIntervalSeconds }),
		},
		"contract": map[string]any{
			"supports":         []string{sdk.MSGConfigGet, sdk.MSGConfigSet},
			"target":           sdk.NodeConfigControlTarget,
			"schema_version":   wfConfigSchemaVersion,
			"apply_modes":      []string{sdk.NodeConfigApplyModeReplace},
			"config_schema":    "wf_runtime_config_v1",
			"boot_time_only":   true,
			"restart_required": true,
		},
	}
}

func applyWFConfigSet(configPath, nodeName string, current *Config, request *sdk.NodeConfigSetPayload) (map[string]any, *Config, error) {
	if request == nil {
		return nil, nil, fmt.Errorf("config set request is nil")
	}
	if request.ApplyMode != "" && request.ApplyMode != sdk.NodeConfigApplyModeReplace {
		return wfConfigErrorPayload(nodeName, "UNSUPPORTED_APPLY_MODE", fmt.Sprintf("unsupported apply_mode: %s", request.ApplyMode), request.ConfigVersion), nil, nil
	}
	configMap, ok := request.Config.(map[string]any)
	if !ok {
		return wfConfigErrorPayload(nodeName, "INVALID_CONFIG_SET", "config must be a JSON object", request.ConfigVersion), nil, nil
	}
	if _, exists := configMap["_system"]; exists {
		return wfConfigErrorPayload(nodeName, "INVALID_CONFIG_SET", "_system is managed by orchestrator and cannot be changed via CONFIG_SET", request.ConfigVersion), nil, nil
	}
	if len(configMap) == 0 {
		return wfConfigErrorPayload(nodeName, "INVALID_CONFIG_SET", "config must not be empty", request.ConfigVersion), nil, nil
	}

	payload, err := json.Marshal(configMap)
	if err != nil {
		return nil, nil, fmt.Errorf("marshal config set payload: %w", err)
	}
	nextCfg, err := loadConfigFromPayload(payload, configPath, cloneManagedSystemConfig(current.System))
	if err != nil {
		return wfConfigErrorPayload(nodeName, "INVALID_CONFIG_SET", err.Error(), request.ConfigVersion), nil, nil
	}

	nextVersion := request.ConfigVersion
	if nextVersion == 0 {
		nextVersion = configVersion(current) + 1
		if nextVersion == 0 {
			nextVersion = 1
		}
	}
	setConfigVersion(nextCfg, nextVersion)
	if err := persistConfigFile(configPath, nextCfg); err != nil {
		return nil, nil, fmt.Errorf("persist config set: %w", err)
	}

	return map[string]any{
		"ok":               true,
		"node_name":        nodeName,
		"state":            "restart_required",
		"schema_version":   wfConfigSchemaVersion,
		"config_version":   nextVersion,
		"restart_required": true,
		"applied":          false,
		"effective_config": buildWFConfigGetPayload(nodeName, nextCfg)["effective_config"],
		"contract":         buildWFConfigGetPayload(nodeName, nextCfg)["contract"],
	}, nextCfg, nil
}

func persistConfigFile(path string, cfg *Config) error {
	if strings.TrimSpace(path) == "" {
		return fmt.Errorf("config path is required")
	}
	raw := map[string]any{}
	if data, err := os.ReadFile(path); err == nil && len(data) > 0 {
		_ = json.Unmarshal(data, &raw)
	}

	raw["workflow_definition_path"] = cfg.WorkflowDefinitionPath
	raw["db_path"] = cfg.DBPath
	raw["sy_timer_l2_name"] = cfg.SYTimerL2Name
	raw["gc_retention_days"] = cfg.GCRetentionDays
	raw["gc_interval_seconds"] = cfg.GCIntervalSeconds
	if strings.TrimSpace(cfg.TenantID) != "" {
		raw["tenant_id"] = cfg.TenantID
	} else {
		delete(raw, "tenant_id")
	}
	if cfg.System != nil {
		raw["_system"] = cfg.System
		raw["config_version"] = cfg.System.ConfigVersion
	}

	data, err := json.MarshalIndent(raw, "", "  ")
	if err != nil {
		return fmt.Errorf("marshal config file: %w", err)
	}
	data = append(data, '\n')
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return fmt.Errorf("create config dir: %w", err)
	}
	if err := os.WriteFile(path, data, 0o644); err != nil {
		return fmt.Errorf("write config file: %w", err)
	}
	return nil
}

func wfConfigErrorPayload(nodeName, code, detail string, version uint64) map[string]any {
	return map[string]any{
		"ok":             false,
		"node_name":      nodeName,
		"state":          "error",
		"schema_version": wfConfigSchemaVersion,
		"config_version": version,
		"error": map[string]any{
			"code":   code,
			"detail": detail,
		},
	}
}

func stringOrEmpty(cfg *Config, fn func(*Config) string) string {
	if cfg == nil {
		return ""
	}
	return fn(cfg)
}

func intOrZero(cfg *Config, fn func(*Config) int) int {
	if cfg == nil {
		return 0
	}
	return fn(cfg)
}
