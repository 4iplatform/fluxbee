package node

import (
	"context"
	"encoding/json"
	"fmt"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// wfNodeRPCTimeout bounds a direct read of a WF node's running instances (WF_LIST_INSTANCES).
const wfNodeRPCTimeout = 2 * time.Second

// wfNodeClient reads a WF node's own data. Counting a workflow's running instances is the one
// thing SY.wf-rules asks a WF node directly: a read of the workflow's own state, not an action on
// the node; launching, restarting, deleting and the node's status go through SY.admin
// (node_lifecycle.go).
type wfNodeClient interface {
	CountRunningInstances(ctx context.Context, nodeName string) (int, error)
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
		code, detail := wfControlError(decoded)
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

// wfControlError reads a WF node's refusal of a WF_* request: `{ok:false, error:<code>,
// detail:<text>}`, the shape the node's control verbs answer with (e.g. node_not_configured while
// its config is not in use); an `error` object with code and message/detail is read as well.
func wfControlError(decoded map[string]any) (string, string) {
	switch value := decoded["error"].(type) {
	case string:
		return value, stringValueFromMap(decoded, "detail")
	case map[string]any:
		detail := stringValueFromMap(value, "message")
		if detail == "" {
			detail = stringValueFromMap(value, "detail")
		}
		return stringValueFromMap(value, "code"), detail
	}
	return "", stringValueFromMap(decoded, "detail")
}
