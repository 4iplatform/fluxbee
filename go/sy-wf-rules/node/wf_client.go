package node

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
	"github.com/google/uuid"
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
	sender   sender
	receiver rpcReceiver
}

func newWFNodeClient(snd sender, rcv rpcReceiver) wfNodeClient {
	if snd == nil || rcv == nil {
		return nil
	}
	return &l2WFNodeClient{sender: snd, receiver: rcv}
}

func (c *l2WFNodeClient) GetNodeStatus(rpcCtx context.Context, nodeName string) (wfNodeStatusProbe, error) {
	if c == nil || c.sender == nil || c.receiver == nil {
		return wfNodeStatusProbe{}, fmt.Errorf("wf node client unavailable")
	}
	traceID := uuid.NewString()
	responseCh, cancelTrace, err := c.receiver.SubscribeTrace(traceID)
	if err != nil {
		return wfNodeStatusProbe{}, err
	}
	defer cancelTrace()
	request, err := fluxbeesdk.BuildSystemRequest(
		c.sender.UUID(),
		nodeName,
		fluxbeesdk.MSGNodeStatusGet,
		map[string]any{
			"node_name": nodeName,
		},
		traceID,
		fluxbeesdk.SystemEnvelopeOptions{},
	)
	if err != nil {
		return wfNodeStatusProbe{}, err
	}
	if err := c.sender.Send(request); err != nil {
		return wfNodeStatusProbe{}, err
	}
	var msg fluxbeesdk.Message
	select {
	case <-rpcCtx.Done():
		return wfNodeStatusProbe{}, rpcCtx.Err()
	case item := <-responseCh:
		if item.err != nil {
			return wfNodeStatusProbe{}, item.err
		}
		msg = item.msg
	}
	if msg.Meta.MsgType != fluxbeesdk.SYSTEMKind || stringValue(msg.Meta.Msg) != fluxbeesdk.MSGNodeStatusGetResponse {
		return wfNodeStatusProbe{}, fmt.Errorf("unexpected wf node status response %q", stringValue(msg.Meta.Msg))
	}
	var decoded map[string]any
	if err := json.Unmarshal(msg.Payload, &decoded); err != nil {
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

func (c *l2WFNodeClient) CountRunningInstances(rpcCtx context.Context, nodeName string) (int, error) {
	if c == nil || c.sender == nil || c.receiver == nil {
		return 0, fmt.Errorf("wf node client unavailable")
	}
	traceID := uuid.NewString()
	responseCh, cancelTrace, err := c.receiver.SubscribeTrace(traceID)
	if err != nil {
		return 0, err
	}
	defer cancelTrace()
	request, err := fluxbeesdk.BuildSystemRequest(
		c.sender.UUID(),
		nodeName,
		"WF_LIST_INSTANCES",
		map[string]any{
			"status": "running",
			"limit":  0,
		},
		traceID,
		fluxbeesdk.SystemEnvelopeOptions{},
	)
	if err != nil {
		return 0, err
	}
	if err := c.sender.Send(request); err != nil {
		return 0, err
	}
	var msg fluxbeesdk.Message
	select {
	case <-rpcCtx.Done():
		return 0, rpcCtx.Err()
	case item := <-responseCh:
		if item.err != nil {
			return 0, item.err
		}
		msg = item.msg
	}
	if msg.Meta.MsgType != fluxbeesdk.SYSTEMKind || stringValue(msg.Meta.Msg) != "WF_LIST_INSTANCES_RESPONSE" {
		return 0, fmt.Errorf("unexpected wf node response %q", stringValue(msg.Meta.Msg))
	}
	var decoded map[string]any
	if err := json.Unmarshal(msg.Payload, &decoded); err != nil {
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
