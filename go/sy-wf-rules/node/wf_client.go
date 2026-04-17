package node

import (
	"context"
	"encoding/json"
	"fmt"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
	"github.com/google/uuid"
)

const wfNodeRPCTimeout = 2 * orchestratorRPCTimeout / 3

type wfNodeClient interface {
	CountRunningInstances(ctx context.Context, nodeName string) (int, error)
}

type l2WFNodeClient struct {
	sender   sender
	receiver receiver
}

func newWFNodeClient(snd sender, rcv receiver) wfNodeClient {
	if snd == nil || rcv == nil {
		return nil
	}
	return &l2WFNodeClient{sender: snd, receiver: rcv}
}

func (c *l2WFNodeClient) CountRunningInstances(rpcCtx context.Context, nodeName string) (int, error) {
	if c == nil || c.sender == nil || c.receiver == nil {
		return 0, fmt.Errorf("wf node client unavailable")
	}
	traceID := uuid.NewString()
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
	for {
		msg, err := c.receiver.Recv(rpcCtx)
		if err != nil {
			return 0, err
		}
		if msg.Routing.TraceID != traceID {
			continue
		}
		if msg.Meta.MsgType != fluxbeesdk.SYSTEMKind || stringValue(msg.Meta.Msg) != "WF_LIST_INSTANCES_RESPONSE" {
			continue
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
}
