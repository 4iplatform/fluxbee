package node

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
	"github.com/google/uuid"
)

const orchestratorRPCTimeout = 3 * time.Second

type orchestratorClient interface {
	GetNodeConfig(ctx context.Context, targetNode, nodeName string) (map[string]any, error)
	RunNode(ctx context.Context, targetNode, nodeName, runtimeName, version string, config map[string]any) (map[string]any, error)
	StartNode(ctx context.Context, targetNode, nodeName string) (map[string]any, error)
	RestartNode(ctx context.Context, targetNode, nodeName string) (map[string]any, error)
	SetNodeConfig(ctx context.Context, targetNode, nodeName string, config map[string]any, binding *managedRuntimeBinding, notify bool) (map[string]any, error)
	KillNode(ctx context.Context, targetNode, nodeName string, force, purgeInstance bool) (map[string]any, error)
}

type orchestratorActionError struct {
	Status  string
	Code    string
	Message string
	Payload map[string]any
}

func (e *orchestratorActionError) Error() string {
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
	return "orchestrator action failed"
}

type l2OrchestratorClient struct {
	sender   sender
	receiver receiver
}

func newOrchestratorClient(snd sender, rcv receiver) orchestratorClient {
	if snd == nil || rcv == nil {
		return nil
	}
	return &l2OrchestratorClient{sender: snd, receiver: rcv}
}

func (c *l2OrchestratorClient) GetNodeConfig(rpcCtx context.Context, targetNode, nodeName string) (map[string]any, error) {
	return c.request(rpcCtx, targetNode, "NODE_CONFIG_GET", "NODE_CONFIG_GET_RESPONSE", map[string]any{
		"node_name": nodeName,
	})
}

func (c *l2OrchestratorClient) RunNode(rpcCtx context.Context, targetNode, nodeName, runtimeName, version string, config map[string]any) (map[string]any, error) {
	payload := map[string]any{
		"node_name":       nodeName,
		"runtime":         runtimeName,
		"runtime_version": version,
	}
	if config != nil {
		payload["config"] = config
	}
	return c.request(rpcCtx, targetNode, "SPAWN_NODE", "SPAWN_NODE_RESPONSE", payload)
}

func (c *l2OrchestratorClient) StartNode(rpcCtx context.Context, targetNode, nodeName string) (map[string]any, error) {
	return c.request(rpcCtx, targetNode, "START_NODE", "START_NODE_RESPONSE", map[string]any{
		"node_name": nodeName,
	})
}

func (c *l2OrchestratorClient) RestartNode(rpcCtx context.Context, targetNode, nodeName string) (map[string]any, error) {
	return c.request(rpcCtx, targetNode, "RESTART_NODE", "RESTART_NODE_RESPONSE", map[string]any{
		"node_name": nodeName,
	})
}

func (c *l2OrchestratorClient) SetNodeConfig(rpcCtx context.Context, targetNode, nodeName string, config map[string]any, binding *managedRuntimeBinding, notify bool) (map[string]any, error) {
	payload := map[string]any{
		"node_name": nodeName,
		"config":    config,
		"notify":    notify,
		"replace":   false,
	}
	if binding != nil {
		payload["runtime"] = binding.Runtime
		payload["runtime_version"] = binding.RuntimeVersion
		payload["requested_runtime_version"] = binding.RequestedRuntimeVersion
		payload["runtime_base"] = binding.RuntimeBase
		payload["package_path"] = binding.PackagePath
	}
	return c.request(rpcCtx, targetNode, "NODE_CONFIG_SET", "NODE_CONFIG_SET_RESPONSE", payload)
}

func (c *l2OrchestratorClient) KillNode(rpcCtx context.Context, targetNode, nodeName string, force, purgeInstance bool) (map[string]any, error) {
	return c.request(rpcCtx, targetNode, "KILL_NODE", "KILL_NODE_RESPONSE", map[string]any{
		"node_name":      nodeName,
		"force":          force,
		"purge_instance": purgeInstance,
	})
}

func (c *l2OrchestratorClient) request(rpcCtx context.Context, targetNode, requestMsg, responseMsg string, payload map[string]any) (map[string]any, error) {
	if c == nil || c.sender == nil || c.receiver == nil {
		return nil, fmt.Errorf("orchestrator client unavailable")
	}
	traceID := uuid.NewString()
	request, err := fluxbeesdk.BuildSystemRequest(
		c.sender.UUID(),
		targetNode,
		requestMsg,
		payload,
		traceID,
		fluxbeesdk.SystemEnvelopeOptions{},
	)
	if err != nil {
		return nil, err
	}
	if err := c.sender.Send(request); err != nil {
		return nil, err
	}
	for {
		msg, err := c.receiver.Recv(rpcCtx)
		if err != nil {
			return nil, err
		}
		if msg.Routing.TraceID != traceID {
			continue
		}
		if msg.Meta.MsgType != fluxbeesdk.SYSTEMKind || stringValue(msg.Meta.Msg) != responseMsg {
			continue
		}
		var decoded map[string]any
		if err := json.Unmarshal(msg.Payload, &decoded); err != nil {
			return nil, err
		}
		status := strings.TrimSpace(stringValueFromMap(decoded, "status"))
		if status == "" {
			status = "error"
		}
		if status != "ok" && status != "not_found" {
			return nil, &orchestratorActionError{
				Status:  status,
				Code:    stringValueFromMap(decoded, "error_code"),
				Message: stringValueFromMap(decoded, "message"),
				Payload: decoded,
			}
		}
		return decoded, nil
	}
}
