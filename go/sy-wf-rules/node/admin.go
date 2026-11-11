package node

import (
	"context"
	"encoding/json"
	"fmt"
	"path/filepath"
	"strings"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

const (
	adminRPCTimeout = 5 * time.Second
	// runtimeRemovalTimeout is how long SY.wf-rules waits for SY.admin's answer to one
	// remove_runtime_version. Before it removes anything, SY.admin reads the version's use on every
	// hive that may run it (get_runtime to the motherbee's orchestrator, which it waits 30 s for
	// unless JSR_ADMIN_ORCH_TIMEOUT_SECS says otherwise) and removes the version only once each of
	// them answered (FINDINGS A-281): those 30 s and adminRPCTimeout for the rest (its checks of the
	// manifest, its write, the mesh both ways), 35 s in all, so SY.admin's own answer, a refusal
	// naming the hives it could not read included, comes before this wait ends. It can come later:
	// SY.admin first waits for its manifest's lock, which another removal holds across its own read
	// of the use and a publish across its install, and a JSR_ADMIN_ORCH_TIMEOUT_SECS above 30 s waits
	// longer. Such an answer is a TIMEOUT here (packages_left), which says SY.admin may still remove
	// the version. With 5 s, before 0.1.133, a read that took longer was answered as a timeout.
	runtimeRemovalTimeout       = 30*time.Second + adminRPCTimeout
	adminCommandMsg             = "ADMIN_COMMAND"
	adminCommandResponseMsg     = "ADMIN_COMMAND_RESPONSE"
	publishRuntimePackageAction = "publish_runtime_package"
	removeRuntimeVersionAction  = "remove_runtime_version"
)

type adminClient interface {
	PublishRuntimePackage(ctx context.Context, packageFiles map[string]string) (*PackagePublishResult, error)
	// RemoveRuntimeVersion deletes one published version from the motherbee's dist and manifest.
	RemoveRuntimeVersion(ctx context.Context, runtimeName, version string) error
	// SyncRuntimes asks a hive's Syncthing to scan its runtimes folder now (sync_hint), and
	// waits for the folder to settle when waitMs > 0.
	SyncRuntimes(ctx context.Context, hive string, waitMs int) error
}

type adminActionError struct {
	Status  string
	Code    string
	Message string
	Payload map[string]any
}

func (e *adminActionError) Error() string {
	if e == nil {
		return ""
	}
	if e.Code != "" && e.Message != "" {
		return fmt.Sprintf("%s: %s", e.Code, e.Message)
	}
	if e.Message != "" {
		return e.Message
	}
	if e.Code != "" {
		return e.Code
	}
	if e.Status != "" {
		return e.Status
	}
	return "admin action failed"
}

type l2AdminClient struct {
	// send delivers one ADMIN_COMMAND and waits for its answer (the router dispatcher's
	// SendAdminRPC), as adminNodeClient's does.
	send       func(ctx context.Context, request fluxbeesdk.AdminRpcRequest) (fluxbeesdk.Message, error)
	targetNode string
	distRoot   string
}

func newAdminClient(cfg NodeConfig, dispatcher *fluxbeesdk.RouterDispatcher) adminClient {
	if dispatcher == nil {
		return nil
	}
	return &l2AdminClient{
		send: dispatcher.SendAdminRPC,
		// SY.admin runs only on the motherbee; a worker's SY.wf-rules publishes there too.
		targetNode: fluxbeesdk.PrimaryAdminNode,
		distRoot:   cfg.DistRuntimeRoot,
	}
}

func (c *l2AdminClient) PublishRuntimePackage(ctx context.Context, packageFiles map[string]string) (*PackagePublishResult, error) {
	if c == nil || c.send == nil {
		return nil, fmt.Errorf("admin client unavailable")
	}
	params := map[string]any{
		"source": map[string]any{
			"kind":  "inline_package",
			"files": packageFiles,
		},
		"set_current": true,
	}
	response, err := c.request(ctx, publishRuntimePackageAction, "", params)
	if err != nil {
		return nil, err
	}
	payload, _ := response["payload"].(map[string]any)
	if payload == nil {
		return nil, fmt.Errorf("admin publish response missing payload")
	}
	runtimeName := strings.TrimSpace(stringValueFromMap(payload, "runtime_name"))
	runtimeVersion := strings.TrimSpace(stringValueFromMap(payload, "runtime_version"))
	if runtimeName == "" || runtimeVersion == "" {
		return nil, fmt.Errorf("admin publish response missing runtime_name/runtime_version")
	}
	packagePath := strings.TrimSpace(stringValueFromMap(payload, "installed_path"))
	if packagePath == "" {
		packagePath = filepath.Join(c.distRoot, runtimeName, runtimeVersion)
	}
	return &PackagePublishResult{
		RuntimeName: runtimeName,
		Version:     runtimeVersion,
		PackagePath: packagePath,
	}, nil
}

func (c *l2AdminClient) RemoveRuntimeVersion(ctx context.Context, runtimeName, version string) error {
	if c == nil || c.send == nil {
		return fmt.Errorf("admin client unavailable")
	}
	// The admin runs this on a hive's orchestrator: the dist being purged is the motherbee's.
	_, err := c.requestWithTimeout(ctx, removeRuntimeVersionAction, fluxbeesdk.PrimaryHiveID, map[string]any{
		"runtime":         runtimeName,
		"runtime_version": version,
	}, runtimeRemovalTimeout)
	return err
}

// runtimesFolderID is the Syncthing folder of dist/runtimes: sendonly on the motherbee, a
// receive-only mirror on every worker.
const runtimesFolderID = "fluxbee-dist-runtimes"

func (c *l2AdminClient) SyncRuntimes(ctx context.Context, hive string, waitMs int) error {
	if c == nil || c.send == nil {
		return fmt.Errorf("admin client unavailable")
	}
	params := map[string]any{
		"channel":       "dist",
		"folder_id":     runtimesFolderID,
		"wait_for_idle": waitMs > 0,
		"timeout_ms":    max(waitMs, 1000),
	}
	callCtx, cancel := context.WithTimeout(ctx, time.Duration(max(waitMs, 1000))*time.Millisecond+adminRPCTimeout)
	defer cancel()
	_, err := c.requestWithTimeout(callCtx, "sync_hint", hive, params, time.Duration(max(waitMs, 1000))*time.Millisecond+adminRPCTimeout)
	return err
}

func (c *l2AdminClient) request(ctx context.Context, action, target string, params map[string]any) (map[string]any, error) {
	return c.requestWithTimeout(ctx, action, target, params, adminRPCTimeout)
}

func (c *l2AdminClient) requestWithTimeout(ctx context.Context, action, target string, params map[string]any, timeout time.Duration) (map[string]any, error) {
	msg, err := c.send(ctx, fluxbeesdk.AdminRpcRequest{
		AdminTarget: c.targetNode,
		Action:      action,
		Target:      target,
		Params:      params,
		Timeout:     timeout,
	})
	if err != nil {
		return nil, err
	}
	if msg.Meta.MsgType != fluxbeesdk.ADMINKind || stringValue(msg.Meta.Msg) != fluxbeesdk.MsgAdminCommandResponse {
		return nil, fmt.Errorf("unexpected admin response %q", stringValue(msg.Meta.Msg))
	}
	var decoded map[string]any
	if err := json.Unmarshal(msg.Payload, &decoded); err != nil {
		return nil, err
	}
	status := strings.TrimSpace(stringValueFromMap(decoded, "status"))
	if status == "" {
		status = "error"
	}
	if status != "ok" {
		return nil, &adminActionError{
			Status:  status,
			Code:    stringValueFromMap(decoded, "error_code"),
			Message: adminErrorDetailText(decoded["error_detail"]),
			Payload: decoded,
		}
	}
	return decoded, nil
}

func adminErrorDetailText(value any) string {
	switch detail := value.(type) {
	case nil:
		return ""
	case string:
		return strings.TrimSpace(detail)
	case map[string]any:
		if msg := strings.TrimSpace(stringValueFromMap(detail, "message")); msg != "" {
			return msg
		}
		if raw, err := json.Marshal(detail); err == nil {
			return string(raw)
		}
	case []any:
		if raw, err := json.Marshal(detail); err == nil {
			return string(raw)
		}
	default:
		if raw, err := json.Marshal(detail); err == nil {
			return string(raw)
		}
	}
	return ""
}
