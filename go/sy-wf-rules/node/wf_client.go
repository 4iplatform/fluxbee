package node

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

const wfNodeRPCTimeout = 2 * orchestratorRPCTimeout / 3

type wfNodeClient interface {
	GetNodeStatus(ctx context.Context, nodeName string) (wfNodeStatusProbe, error)
	CountRunningInstances(ctx context.Context, nodeName string) (int, error)
}

type wfNodeStatusProbe struct {
	HealthState string
}

type l2WFNodeClient struct {
	dispatcher *fluxbeesdk.RouterDispatcher
}

func newWFNodeClient(dispatcher *fluxbeesdk.RouterDispatcher) wfNodeClient {
	if dispatcher == nil {
		return nil
	}
	return &l2WFNodeClient{dispatcher: dispatcher}
}

func (c *l2WFNodeClient) wfSystemRPC(
	ctx context.Context,
	targetNode, requestMsg, responseMsg string,
	payload map[string]any,
) (map[string]any, error) {
	if c == nil || c.dispatcher == nil {
		return nil, fmt.Errorf("wf node client unavailable")
	}
	msg, err := c.dispatcher.SendSystemRPC(ctx, fluxbeesdk.SystemRpcRequest{
		Target:      targetNode,
		RequestMsg:  requestMsg,
		ResponseMsg: responseMsg,
		Payload:     payload,
		Timeout:     wfNodeRPCTimeout,
	})
	if err != nil {
		return nil, err
	}
	if msg.Meta.MsgType != fluxbeesdk.SYSTEMKind || stringValue(msg.Meta.Msg) != responseMsg {
		return nil, fmt.Errorf("unexpected wf node response %q", stringValue(msg.Meta.Msg))
	}
	var decoded map[string]any
	if err := json.Unmarshal(msg.Payload, &decoded); err != nil {
		return nil, err
	}
	return decoded, nil
}

func (c *l2WFNodeClient) GetNodeStatus(ctx context.Context, nodeName string) (wfNodeStatusProbe, error) {
	decoded, err := c.wfSystemRPC(
		ctx,
		nodeName,
		fluxbeesdk.MSGNodeStatusGet,
		fluxbeesdk.MSGNodeStatusGetResponse,
		map[string]any{"node_name": nodeName},
	)
	if err != nil {
		return wfNodeStatusProbe{}, err
	}
	status := strings.TrimSpace(stringValueFromMap(decoded, "status"))
	if status != "" && status != "ok" {
		code := stringValueFromMap(decoded, "error_code")
		detail := stringValueFromMap(decoded, "error_detail")
		if code == "" && detail == "" {
			return wfNodeStatusProbe{}, fmt.Errorf("wf node returned unsuccessful node status response")
		}
		return wfNodeStatusProbe{}, WfRulesError{Code: code, Detail: detail}
	}
	return wfNodeStatusProbe{
		HealthState: strings.TrimSpace(stringValueFromMap(decoded, "health_state")),
	}, nil
}

func (c *l2WFNodeClient) CountRunningInstances(ctx context.Context, nodeName string) (int, error) {
	decoded, err := c.wfSystemRPC(
		ctx,
		nodeName,
		"WF_LIST_INSTANCES",
		"WF_LIST_INSTANCES_RESPONSE",
		map[string]any{"status": "running", "limit": 0},
	)
	if err != nil {
		return 0, err
	}
	ok, _ := decoded["ok"].(bool)
	if !ok {
		code := stringValueFromMapMap(decoded, "error", "code")
		detail := stringValueFromMapMap(decoded, "error", "detail")
		if code == "" && detail == "" {
			return 0, fmt.Errorf("wf node returned unsuccessful response")
		}
		return 0, WfRulesError{Code: code, Detail: detail}
	}
	countValue, ok := decoded["count"]
	if !ok {
		return 0, fmt.Errorf("wf node response missing count")
	}
	switch value := countValue.(type) {
	case float64:
		return int(value), nil
	case int:
		return value, nil
	case int64:
		return int(value), nil
	default:
		return 0, fmt.Errorf("wf node returned invalid count type %T", countValue)
	}
}
