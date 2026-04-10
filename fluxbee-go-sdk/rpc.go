package sdk

import (
	"context"
	"encoding/json"
	"fmt"
)

type SystemEnvelopeOptions struct {
	TTL      uint8
	SrcIlk   *string
	Scope    *string
	Target   *string
	Action   *string
	Priority *string
	Context  json.RawMessage
}

type SystemErrorDetail struct {
	Code    string `json:"code"`
	Message string `json:"message"`
}

type SystemActionResponse struct {
	OK    bool               `json:"ok"`
	Verb  string             `json:"verb,omitempty"`
	Error *SystemErrorDetail `json:"error,omitempty"`
}

type SystemResponseError struct {
	ResponseMsg string
	Verb        string
	Code        string
	Message     string
}

func (e *SystemResponseError) Error() string {
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
	return "system response error"
}

func BuildSystemRequest(srcUUID, targetNode, msg string, payload any, traceID string, options SystemEnvelopeOptions) (Message, error) {
	if srcUUID == "" {
		return Message{}, fmt.Errorf("src_uuid must be non-empty")
	}
	if targetNode == "" {
		return Message{}, fmt.Errorf("target_node must be non-empty")
	}
	if traceID == "" {
		return Message{}, fmt.Errorf("trace_id must be non-empty")
	}
	raw, err := MarshalPayload(payload)
	if err != nil {
		return Message{}, err
	}
	msgCopy := msg
	action := options.Action
	if action == nil {
		action = &msgCopy
	}
	return Message{
		Routing: Routing{
			Src:     srcUUID,
			Dst:     UnicastDestination(targetNode),
			TTL:     normalizeTTL(options.TTL),
			TraceID: traceID,
		},
		Meta: Meta{
			MsgType:  SYSTEMKind,
			Msg:      &msgCopy,
			SrcIlk:   options.SrcIlk,
			Scope:    options.Scope,
			Target:   options.Target,
			Action:   action,
			Priority: options.Priority,
			Context:  options.Context,
		},
		Payload: raw,
	}, nil
}

func BuildSystemResponse(incoming *Message, srcUUID, responseMsg string, payload any, options SystemEnvelopeOptions) (Message, error) {
	raw, err := MarshalPayload(payload)
	if err != nil {
		return Message{}, err
	}
	msgCopy := responseMsg
	action := options.Action
	if action == nil {
		action = &msgCopy
	}
	return Message{
		Routing: buildReplyRouting(incoming, srcUUID),
		Meta: Meta{
			MsgType:  SYSTEMKind,
			Msg:      &msgCopy,
			SrcIlk:   options.SrcIlk,
			Scope:    options.Scope,
			Target:   options.Target,
			Action:   action,
			Priority: options.Priority,
			Context:  options.Context,
		},
		Payload: raw,
	}, nil
}

func AwaitSystemResponse(ctx context.Context, receiver *NodeReceiver, traceID, responseMsg string) (Message, error) {
	if receiver == nil {
		return Message{}, fmt.Errorf("receiver must be non-nil")
	}
	for {
		msg, err := receiver.Recv(ctx)
		if err != nil {
			return Message{}, err
		}
		if msg.Routing.TraceID != traceID {
			continue
		}
		if msg.Meta.MsgType != SYSTEMKind || stringValue(msg.Meta.Msg) != responseMsg {
			continue
		}
		return msg, nil
	}
}

func RequestSystemRPC(ctx context.Context, sender *NodeSender, receiver *NodeReceiver, request Message, responseMsg string) (Message, error) {
	if sender == nil {
		return Message{}, fmt.Errorf("sender must be non-nil")
	}
	if err := sender.Send(request); err != nil {
		return Message{}, err
	}
	return AwaitSystemResponse(ctx, receiver, request.Routing.TraceID, responseMsg)
}

func ParseSystemResponse(msg *Message, responseMsg string, out any) error {
	if msg == nil {
		return fmt.Errorf("message must be non-nil")
	}
	if msg.Meta.MsgType != SYSTEMKind || stringValue(msg.Meta.Msg) != responseMsg {
		return fmt.Errorf("message is not %s", responseMsg)
	}
	if out == nil {
		return nil
	}
	return json.Unmarshal(msg.Payload, out)
}

func ParseSystemResponseError(msg *Message, responseMsg string) (*SystemResponseError, error) {
	var payload SystemActionResponse
	if err := ParseSystemResponse(msg, responseMsg, &payload); err != nil {
		return nil, err
	}
	if payload.OK {
		return nil, nil
	}
	errDetail := payload.Error
	if errDetail == nil {
		return &SystemResponseError{
			ResponseMsg: responseMsg,
			Verb:        payload.Verb,
			Message:     "response marked as not ok without error detail",
		}, nil
	}
	return &SystemResponseError{
		ResponseMsg: responseMsg,
		Verb:        payload.Verb,
		Code:        errDetail.Code,
		Message:     errDetail.Message,
	}, nil
}
