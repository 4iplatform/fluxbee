package node

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// SY.wf-rules launches, rebinds, restarts and deletes its WF nodes through SY.admin, with the same
// node actions every other launcher uses: every action goes through the admin, which lets SY.wf-rules
// act only on the WF.* nodes of its own hive (docs/wf-standard-path-v1.md). The whole deploy runs
// inside the admin's wait for this node, so each call has its own bound.
const (
	nodeQueryTimeout  = 10 * time.Second // get_node_config, get_node_status, set_node_config
	nodeLaunchTimeout = 15 * time.Second // run_node, restart_node
	nodeDeleteTimeout = 25 * time.Second // kill_node with purge_instance (the teardown cleans the ILK and its secrets)
)

type nodeClient interface {
	// GetNodeConfig is the orchestrator's view of the node's config.json ({status, config, ...}).
	GetNodeConfig(ctx context.Context, nodeName string) (map[string]any, error)
	SetNodeConfig(ctx context.Context, nodeName string, config map[string]any, binding *managedRuntimeBinding) (map[string]any, error)
	// RunNode launches the node; launchedBy is who asked for the deploy (the admin stamped it).
	RunNode(ctx context.Context, nodeName, runtimeName, version string, config map[string]any, launchedBy string) (map[string]any, error)
	RestartNode(ctx context.Context, nodeName string) (map[string]any, error)
	KillNode(ctx context.Context, nodeName string, force, purgeInstance bool) (map[string]any, error)
	// GetNodeStatus is the orchestrator's node_status: lifecycle_state, health_state, last_error.
	GetNodeStatus(ctx context.Context, nodeName string) (map[string]any, error)
}

type nodeActionError struct {
	Status  string
	Code    string
	Message string
	Payload map[string]any
}

func (e *nodeActionError) Error() string {
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
	return "node action failed"
}

type adminNodeClient struct {
	// send delivers one ADMIN_COMMAND and waits for its answer (the router dispatcher's SendAdminRPC).
	send func(ctx context.Context, request fluxbeesdk.AdminRpcRequest) (fluxbeesdk.Message, error)
	hive string
}

func newNodeClient(dispatcher *fluxbeesdk.RouterDispatcher, hive string) nodeClient {
	if dispatcher == nil {
		return nil
	}
	return &adminNodeClient{send: dispatcher.SendAdminRPC, hive: hive}
}

func (c *adminNodeClient) GetNodeConfig(ctx context.Context, nodeName string) (map[string]any, error) {
	return c.request(ctx, "get_node_config", nodeName, map[string]any{}, nodeQueryTimeout)
}

func (c *adminNodeClient) SetNodeConfig(ctx context.Context, nodeName string, config map[string]any, binding *managedRuntimeBinding) (map[string]any, error) {
	params := map[string]any{
		"config":  config,
		"replace": false,
	}
	if binding != nil {
		params["runtime"] = binding.Runtime
		params["runtime_version"] = binding.RuntimeVersion
		params["requested_runtime_version"] = binding.RequestedRuntimeVersion
		params["runtime_base"] = binding.RuntimeBase
		params["package_path"] = binding.PackagePath
	}
	return c.request(ctx, "set_node_config", nodeName, params, nodeQueryTimeout)
}

func (c *adminNodeClient) RunNode(ctx context.Context, nodeName, runtimeName, version string, config map[string]any, launchedBy string) (map[string]any, error) {
	params := map[string]any{
		"runtime":         runtimeName,
		"runtime_version": version,
	}
	if config != nil {
		params["config"] = config
	}
	if launchedBy = strings.TrimSpace(launchedBy); launchedBy != "" {
		params["launched_by"] = launchedBy
	}
	return c.request(ctx, "run_node", nodeName, params, nodeLaunchTimeout)
}

func (c *adminNodeClient) RestartNode(ctx context.Context, nodeName string) (map[string]any, error) {
	return c.request(ctx, "restart_node", nodeName, map[string]any{}, nodeLaunchTimeout)
}

func (c *adminNodeClient) KillNode(ctx context.Context, nodeName string, force, purgeInstance bool) (map[string]any, error) {
	return c.request(ctx, "kill_node", nodeName, map[string]any{
		"force":          force,
		"purge_instance": purgeInstance,
	}, nodeDeleteTimeout)
}

func (c *adminNodeClient) GetNodeStatus(ctx context.Context, nodeName string) (map[string]any, error) {
	payload, err := c.request(ctx, "get_node_status", nodeName, map[string]any{}, nodeQueryTimeout)
	if err != nil {
		return nil, err
	}
	status, _ := payload["node_status"].(map[string]any)
	if status == nil {
		return nil, fmt.Errorf("get_node_status answered without node_status")
	}
	return status, nil
}

// request sends one node action to SY.admin for this hive and returns the orchestrator's answer
// (the envelope's payload); a refusal is a *nodeActionError carrying the orchestrator's code.
func (c *adminNodeClient) request(ctx context.Context, action, nodeName string, params map[string]any, timeout time.Duration) (map[string]any, error) {
	if c == nil || c.send == nil {
		return nil, fmt.Errorf("admin client unavailable")
	}
	params["node_name"] = nodeName
	msg, err := c.send(ctx, fluxbeesdk.AdminRpcRequest{
		AdminTarget: fluxbeesdk.PrimaryAdminNode,
		Action:      action,
		Target:      c.hive,
		Params:      params,
		Timeout:     timeout,
	})
	if err != nil {
		return nil, err
	}
	if msg.Meta.MsgType != fluxbeesdk.ADMINKind || stringValue(msg.Meta.Msg) != fluxbeesdk.MsgAdminCommandResponse {
		return nil, fmt.Errorf("unexpected admin response %q", stringValue(msg.Meta.Msg))
	}
	var envelope map[string]any
	if err := json.Unmarshal(msg.Payload, &envelope); err != nil {
		return nil, err
	}
	payload, _ := envelope["payload"].(map[string]any)
	if payload == nil {
		payload = map[string]any{}
	}
	if status := strings.TrimSpace(stringValueFromMap(envelope, "status")); status != "ok" {
		if status == "" {
			status = "error"
		}
		return nil, &nodeActionError{
			Status:  status,
			Code:    stringValueFromMap(envelope, "error_code"),
			Message: adminErrorDetailText(envelope["error_detail"]),
			Payload: payload,
		}
	}
	return payload, nil
}
