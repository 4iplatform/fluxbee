package node

import (
	"fmt"
	"strings"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

const wfConfigSchemaVersion uint32 = 1

// Codes of the CONFIG_GET / CONFIG_SET replies, the IO family's (lowercase, in {code, message}).
const (
	errCodeUnsupportedApplyMode = "unsupported_apply_mode"
	errCodeStaleConfigVersion   = "stale_config_version"
	errCodeConfigPersistError   = "config_persist_error"
	errCodeNodeNotConfigured    = "node_not_configured"
)

// buildWFConfigGetPayload answers CONFIG_GET: the node's config state and what it runs with.
func buildWFConfigGetPayload(nodeName string, plane *ControlPlane) map[string]any {
	materialized := plane.Configured()
	var effective any
	if materialized != nil {
		effective = effectiveConfigView(&materialized.Config)
	}
	var lastError any
	if plane.State.LastError != nil {
		lastError = plane.State.LastError
	}
	payload := map[string]any{
		"ok":               materialized != nil,
		"node_name":        nodeName,
		"state":            plane.State.CurrentState,
		"schema_version":   wfConfigSchemaVersion,
		"config_version":   plane.State.ConfigVersion,
		"effective_config": effective,
		"contract":         wfConfigContract(plane.PackagePath),
		"error":            lastError,
	}
	if plane.pendingVersion > plane.State.ConfigVersion {
		payload["pending_restart"] = map[string]any{"config_version": plane.pendingVersion}
	}
	return payload
}

func effectiveConfigView(cfg *Config) map[string]any {
	source := "config"
	if cfg.IsManagedPackageMode() {
		source = "managed_package"
	}
	return map[string]any{
		"workflow_definition_path":   cfg.WorkflowDefinitionPath,
		"workflow_definition_source": source,
		"db_path":                    cfg.DBPath,
		"sy_timer_l2_name":           cfg.SYTimerL2Name,
		"tenant_id":                  cfg.TenantID,
		"gc_retention_days":          cfg.GCRetentionDays,
		"gc_interval_seconds":        cfg.GCIntervalSeconds,
		"managed_package_mode":       cfg.IsManagedPackageMode(),
		"package_path":               cfg.PackagePath,
		"package_definition_path":    cfg.PackageDefinitionPath(),
	}
}

func wfConfigContract(packagePath string) map[string]any {
	managedPackage := packagePath != ""
	contract := map[string]any{
		"supports":               []string{sdk.MSGConfigGet, sdk.MSGConfigSet},
		"target":                 sdk.NodeConfigControlTarget,
		"schema_version":         wfConfigSchemaVersion,
		"apply_modes":            []string{sdk.NodeConfigApplyModeReplace},
		"config_schema":          "wf_runtime_config_v1",
		"boot_time_only":         true,
		"restart_required":       true,
		"package_native_binding": managedPackage,
	}
	if managedPackage {
		contract["workflow_definition_path_locked"] = true
		contract["notes"] = []string{
			"workflow_definition_path is package-native in managed mode: derived from _system.package_path/flow/definition.json.",
			"A CONFIG_SET that points workflow_definition_path elsewhere is refused with " + ErrCodeManagedPackagePathLocked + ".",
			"Operator-facing source of truth in managed mode is the workflow package, not the local config field.",
		}
	}
	return contract
}

// applyWFConfigSet validates a CONFIG_SET and persists it, and returns the reply. WF applies its
// config at boot, so the running config does not change: the reply says a restart applies it. A
// rejected CONFIG_SET changes nothing.
func applyWFConfigSet(nodeName string, plane *ControlPlane, request *sdk.NodeConfigSetPayload) map[string]any {
	if request.ApplyMode != "" && request.ApplyMode != sdk.NodeConfigApplyModeReplace {
		return wfConfigErrorPayload(nodeName, plane, errCodeUnsupportedApplyMode, fmt.Sprintf("unsupported apply_mode %q; this node supports replace", request.ApplyMode))
	}
	candidate, ok := request.Config.(map[string]any)
	if !ok {
		return wfConfigErrorPayload(nodeName, plane, sdk.ErrCodeInvalidConfig, "config must be a JSON object")
	}
	if _, present := candidate["_system"]; present {
		return wfConfigErrorPayload(nodeName, plane, sdk.ErrCodeInvalidConfig, "_system is the orchestrator's and cannot be set via CONFIG_SET")
	}
	if len(candidate) == 0 {
		return wfConfigErrorPayload(nodeName, plane, sdk.ErrCodeInvalidConfig, "config must not be empty")
	}
	latest := plane.latestVersion()
	next := request.ConfigVersion
	if next == 0 {
		next = latest + 1
	} else if next <= latest {
		return wfConfigErrorPayload(nodeName, plane, errCodeStaleConfigVersion, fmt.Sprintf("config_version %d is not newer than %d", next, latest))
	}

	system := readSystemBlock(plane.ConfigPath)
	packagePath := sdk.ManagedSystemString(system, "package_path")
	if packagePath != "" {
		if supplied, _ := candidate["workflow_definition_path"].(string); strings.TrimSpace(supplied) != "" && strings.TrimSpace(supplied) != packageDefinitionPath(packagePath) {
			return wfConfigErrorPayload(nodeName, plane, ErrCodeManagedPackagePathLocked, fmt.Sprintf(
				"workflow_definition_path is the workflow package's in managed mode (%q, got %q); republish the workflow package to change the definition",
				packageDefinitionPath(packagePath), strings.TrimSpace(supplied)))
		}
	}
	accepted, err := plane.contract.ValidateAndMaterialize(candidate, system)
	if err != nil {
		code, message := sdk.ErrCodeInvalidConfig, err.Error()
		if planeErr, ok := err.(*sdk.ControlPlaneError); ok {
			code, message = planeErr.Code, planeErr.Message
		}
		return wfConfigErrorPayload(nodeName, plane, code, message)
	}
	materialized := accepted.(*Materialized)
	persisted, err := persistableOperatorConfig(materialized.Operator, materialized.Config.IsManagedPackageMode())
	if err == nil {
		err = sdk.PersistManagedConfig(plane.ConfigPath, next, persisted)
	}
	if err != nil {
		return wfConfigErrorPayload(nodeName, plane, errCodeConfigPersistError, err.Error())
	}
	plane.pendingVersion = next

	// The node's own state does not change until a restart applies the config: a node in
	// FAILED_CONFIG stays there (and DEGRADED), it is not healthy yet.
	return map[string]any{
		"ok":               true,
		"node_name":        nodeName,
		"state":            plane.State.CurrentState,
		"schema_version":   wfConfigSchemaVersion,
		"config_version":   next,
		"effective_config": effectiveConfigView(&materialized.Config),
		"contract":         wfConfigContract(materialized.Config.PackagePath),
		"error":            nil,
		"restart_required": true,
		"applied":          false,
		"pending_restart":  map[string]any{"config_version": next},
	}
}

// wfConfigErrorPayload is a refused CONFIG_GET or CONFIG_SET: the node's own state and version,
// and why ({code, message}, which SY.admin reports).
func wfConfigErrorPayload(nodeName string, plane *ControlPlane, code, message string) map[string]any {
	return map[string]any{
		"ok":               false,
		"node_name":        nodeName,
		"state":            plane.State.CurrentState,
		"schema_version":   wfConfigSchemaVersion,
		"config_version":   plane.State.ConfigVersion,
		"effective_config": nil,
		"error":            sdk.ControlPlaneError{Code: code, Message: message},
	}
}
