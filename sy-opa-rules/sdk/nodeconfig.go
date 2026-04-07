package sdk

import (
	"encoding/json"
	"errors"
	"fmt"
)

const (
	NodeConfigControlTarget    = "node_config_control"
	NodeConfigApplyModeReplace = "replace"
)

type NodeConfigGetPayload struct {
	NodeName        string         `json:"node_name"`
	RequestID       *string        `json:"request_id,omitempty"`
	ContractVersion *uint32        `json:"contract_version,omitempty"`
	RequestedBy     *string        `json:"requested_by,omitempty"`
	Extra           map[string]any `json:"-"`
}

type NodeConfigSetPayload struct {
	NodeName        string         `json:"node_name"`
	SchemaVersion   uint32         `json:"schema_version"`
	ConfigVersion   uint64         `json:"config_version"`
	ApplyMode       string         `json:"apply_mode"`
	Config          any            `json:"config,omitempty"`
	RequestID       *string        `json:"request_id,omitempty"`
	ContractVersion *uint32        `json:"contract_version,omitempty"`
	RequestedBy     *string        `json:"requested_by,omitempty"`
	Extra           map[string]any `json:"-"`
}

type NodeConfigControlResponse struct {
	OK              bool           `json:"ok"`
	NodeName        string         `json:"node_name"`
	State           *string        `json:"state,omitempty"`
	SchemaVersion   *uint32        `json:"schema_version,omitempty"`
	ConfigVersion   *uint64        `json:"config_version,omitempty"`
	Error           any            `json:"error,omitempty"`
	EffectiveConfig any            `json:"effective_config,omitempty"`
	Contract        any            `json:"contract,omitempty"`
	Extra           map[string]any `json:"-"`
}

type NodeConfigEnvelopeOptions struct {
	TTL     uint8
	SrcIlk  *string
	Scope   *string
	Context json.RawMessage
}

type NodeConfigControlRequest struct {
	Get *NodeConfigGetPayload
	Set *NodeConfigSetPayload
}

func (p NodeConfigGetPayload) MarshalJSON() ([]byte, error) {
	type alias NodeConfigGetPayload
	base := map[string]any{}
	data, err := json.Marshal(alias(p))
	if err != nil {
		return nil, err
	}
	if err := json.Unmarshal(data, &base); err != nil {
		return nil, err
	}
	for k, v := range p.Extra {
		base[k] = v
	}
	return json.Marshal(base)
}

func (p *NodeConfigGetPayload) UnmarshalJSON(data []byte) error {
	type alias NodeConfigGetPayload
	var raw map[string]json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	var out alias
	if err := json.Unmarshal(data, &out); err != nil {
		return err
	}
	p.NodeName = out.NodeName
	p.RequestID = out.RequestID
	p.ContractVersion = out.ContractVersion
	p.RequestedBy = out.RequestedBy
	p.Extra = map[string]any{}
	for _, key := range []string{"node_name", "request_id", "contract_version", "requested_by"} {
		delete(raw, key)
	}
	for k, v := range raw {
		var decoded any
		if err := json.Unmarshal(v, &decoded); err != nil {
			return err
		}
		p.Extra[k] = decoded
	}
	return nil
}

func (p NodeConfigSetPayload) MarshalJSON() ([]byte, error) {
	type alias NodeConfigSetPayload
	base := map[string]any{}
	data, err := json.Marshal(alias(p))
	if err != nil {
		return nil, err
	}
	if err := json.Unmarshal(data, &base); err != nil {
		return nil, err
	}
	for k, v := range p.Extra {
		base[k] = v
	}
	return json.Marshal(base)
}

func (p *NodeConfigSetPayload) UnmarshalJSON(data []byte) error {
	type alias NodeConfigSetPayload
	var raw map[string]json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	var out alias
	if err := json.Unmarshal(data, &out); err != nil {
		return err
	}
	*p = NodeConfigSetPayload(out)
	p.Extra = map[string]any{}
	for _, key := range []string{"node_name", "schema_version", "config_version", "apply_mode", "config", "request_id", "contract_version", "requested_by"} {
		delete(raw, key)
	}
	for k, v := range raw {
		var decoded any
		if err := json.Unmarshal(v, &decoded); err != nil {
			return err
		}
		p.Extra[k] = decoded
	}
	return nil
}

func (p *NodeConfigControlResponse) UnmarshalJSON(data []byte) error {
	type alias NodeConfigControlResponse
	var raw map[string]json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	var out alias
	if err := json.Unmarshal(data, &out); err != nil {
		return err
	}
	*p = NodeConfigControlResponse(out)
	p.Extra = map[string]any{}
	for _, key := range []string{"ok", "node_name", "state", "schema_version", "config_version", "error", "effective_config", "contract"} {
		delete(raw, key)
	}
	for k, v := range raw {
		var decoded any
		if err := json.Unmarshal(v, &decoded); err != nil {
			return err
		}
		p.Extra[k] = decoded
	}
	return nil
}

func IsNodeConfigGetMessage(msg *Message) bool {
	return msg.Meta.MsgType == SYSTEMKind && stringValue(msg.Meta.Msg) == MSGConfigGet
}

func IsNodeConfigSetMessage(msg *Message) bool {
	return msg.Meta.MsgType == SYSTEMKind && stringValue(msg.Meta.Msg) == MSGConfigSet
}

func IsNodeConfigResponseMessage(msg *Message) bool {
	return msg.Meta.MsgType == SYSTEMKind && stringValue(msg.Meta.Msg) == MSGConfigResponse
}

func ParseNodeConfigRequest(msg *Message) (*NodeConfigControlRequest, error) {
	switch {
	case IsNodeConfigGetMessage(msg):
		var payload NodeConfigGetPayload
		if err := json.Unmarshal(msg.Payload, &payload); err != nil {
			return nil, err
		}
		return &NodeConfigControlRequest{Get: &payload}, nil
	case IsNodeConfigSetMessage(msg):
		var payload NodeConfigSetPayload
		if err := json.Unmarshal(msg.Payload, &payload); err != nil {
			return nil, err
		}
		return &NodeConfigControlRequest{Set: &payload}, nil
	default:
		return nil, errors.New("message is not a node config control request")
	}
}

func ParseNodeConfigResponse(msg *Message) (*NodeConfigControlResponse, error) {
	if !IsNodeConfigResponseMessage(msg) {
		return nil, errors.New("message is not CONFIG_RESPONSE")
	}
	var payload NodeConfigControlResponse
	if err := json.Unmarshal(msg.Payload, &payload); err != nil {
		return nil, err
	}
	return &payload, nil
}

func BuildNodeConfigGetMessage(srcUUID, targetNode string, payload NodeConfigGetPayload, options NodeConfigEnvelopeOptions, traceID string) (Message, error) {
	return buildNodeConfigMessage(srcUUID, targetNode, MSGConfigGet, payload, options, traceID)
}

func BuildNodeConfigSetMessage(srcUUID, targetNode string, payload NodeConfigSetPayload, options NodeConfigEnvelopeOptions, traceID string) (Message, error) {
	return buildNodeConfigMessage(srcUUID, targetNode, MSGConfigSet, payload, options, traceID)
}

func BuildNodeConfigResponseMessage(incoming *Message, srcUUID string, payload any) (Message, error) {
	raw, err := MarshalPayload(payload)
	if err != nil {
		return Message{}, err
	}
	msg := MSGConfigResponse
	target := NodeConfigControlTarget
	return Message{
		Routing: buildReplyRouting(incoming, srcUUID),
		Meta: Meta{
			MsgType:  SYSTEMKind,
			Msg:      &msg,
			SrcIlk:   incoming.Meta.SrcIlk,
			Scope:    incoming.Meta.Scope,
			Target:   &target,
			Action:   &msg,
			Priority: incoming.Meta.Priority,
			Context:  incoming.Meta.Context,
		},
		Payload: raw,
	}, nil
}

func BuildNodeConfigResponseMessageRuntimeSrc(incoming *Message, payload any) (Message, error) {
	return BuildNodeConfigResponseMessage(incoming, "", payload)
}

func buildNodeConfigMessage(srcUUID, targetNode, requestMsg string, payload any, options NodeConfigEnvelopeOptions, traceID string) (Message, error) {
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
	msgCopy := requestMsg
	target := NodeConfigControlTarget
	return Message{
		Routing: Routing{
			Src:     srcUUID,
			Dst:     UnicastDestination(targetNode),
			TTL:     normalizeTTL(options.TTL),
			TraceID: traceID,
		},
		Meta: Meta{
			MsgType: SYSTEMKind,
			Msg:     &msgCopy,
			SrcIlk:  options.SrcIlk,
			Scope:   options.Scope,
			Target:  &target,
			Action:  &msgCopy,
			Context: options.Context,
		},
		Payload: raw,
	}, nil
}

func buildReplyRouting(incoming *Message, srcUUID string) Routing {
	dst := BroadcastDestination()
	if incoming.Routing.Src != "" {
		dst = UnicastDestination(incoming.Routing.Src)
	}
	return Routing{
		Src:     srcUUID,
		Dst:     dst,
		TTL:     normalizeTTL(incoming.Routing.TTL),
		TraceID: incoming.Routing.TraceID,
	}
}

func normalizeTTL(ttl uint8) uint8 {
	if ttl == 0 {
		return 1
	}
	return ttl
}

func stringValue(value *string) string {
	if value == nil {
		return ""
	}
	return *value
}
