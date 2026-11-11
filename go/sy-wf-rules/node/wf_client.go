package node

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"strconv"
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
	// Numbers as the node wrote them (json.Number): a count is read as a whole number, never
	// through a float64 (instanceCount).
	decoder := json.NewDecoder(bytes.NewReader(msg.Payload))
	decoder.UseNumber()
	var decoded map[string]any
	if err := decoder.Decode(&decoded); err != nil {
		return nil, err
	}
	return decoded, nil
}

// CountRunningInstances asks the WF node how many instances it runs: WF_LIST_INSTANCES with
// count_only, which the node answers with a COUNT(*) and no rows. Before 0.1.136 it counted the rows
// of the whole list, which passed the frame past about 350 running instances: the node answered
// INTERNAL_ERROR, and the count was unknown exactly for a busy workflow (FINDINGS A-284). limit
// stays 0 for an engine before 0.1.136, which does not know count_only, ignores it as any field it
// does not know, and lists every running instance: its count is then the whole count, as before, or
// past the frame its INTERNAL_ERROR, a refusal. A limit would cut the count it answers.
func (c *l2WFNodeClient) CountRunningInstances(ctx context.Context, nodeName string) (int, error) {
	decoded, err := c.wfSystemRPC(
		ctx,
		nodeName,
		"WF_LIST_INSTANCES",
		"WF_LIST_INSTANCES_RESPONSE",
		map[string]any{"status": "running", "limit": 0, "count_only": true},
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
	return instanceCount(countValue)
}

// instanceCount reads the count of a WF_LIST_INSTANCES answer, a whole number from 0. Anything else
// is no count, and fails the read: a delete without force goes on only on a count of 0, and a
// negative count, or one an int cannot hold, read as one would have let it purge the node's
// instances.
func instanceCount(value any) (int, error) {
	number, ok := value.(json.Number)
	if !ok {
		return 0, fmt.Errorf("wf node returned invalid count type %T", value)
	}
	count, err := strconv.Atoi(number.String())
	if err != nil || count < 0 {
		return 0, fmt.Errorf("wf node returned invalid count %s", number)
	}
	return count, nil
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
