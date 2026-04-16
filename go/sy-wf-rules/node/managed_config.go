package node

import "fmt"

const (
	defaultWFGCRetentionDays   = 7
	defaultWFGCIntervalSeconds = 3600
)

type managedRuntimeBinding struct {
	Runtime                 string
	RuntimeVersion          string
	RequestedRuntimeVersion string
	RuntimeBase             string
	PackagePath             string
}

func (s *Service) buildManagedWFConfig(existing map[string]any) map[string]any {
	cfg := map[string]any{}
	for key, value := range existing {
		if key == "_system" {
			continue
		}
		cfg[key] = value
	}
	if stringValueFromMap(cfg, "sy_timer_l2_name") == "" {
		cfg["sy_timer_l2_name"] = fmt.Sprintf("SY.timer@%s", s.cfg.HiveID)
	}
	if intValueFromMap(cfg, "gc_retention_days") <= 0 {
		cfg["gc_retention_days"] = defaultWFGCRetentionDays
	}
	if intValueFromMap(cfg, "gc_interval_seconds") <= 0 {
		cfg["gc_interval_seconds"] = defaultWFGCIntervalSeconds
	}
	if stringValueFromMap(cfg, "tenant_id") == "" && s.cfg.TenantID != "" {
		cfg["tenant_id"] = s.cfg.TenantID
	}
	return cfg
}

func buildManagedRuntimeBinding(pkg PackagePublishResult) managedRuntimeBinding {
	return managedRuntimeBinding{
		Runtime:                 pkg.RuntimeName,
		RuntimeVersion:          pkg.Version,
		RequestedRuntimeVersion: pkg.Version,
		RuntimeBase:             workflowRuntimeBase,
		PackagePath:             pkg.PackagePath,
	}
}
