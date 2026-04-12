package sdk

import (
	"encoding/json"
	"fmt"
)

type Message struct {
	Routing Routing         `json:"routing"`
	Meta    Meta            `json:"meta"`
	Payload json.RawMessage `json:"payload"`
}

type Routing struct {
	Src       string      `json:"src"`
	SrcL2Name *string     `json:"src_l2_name,omitempty"`
	Dst       Destination `json:"dst"`
	TTL       uint8       `json:"ttl"`
	TraceID   string      `json:"trace_id"`
}

type destinationKind uint8

const (
	destinationUnicast destinationKind = iota + 1
	destinationBroadcast
	destinationResolve
)

type Destination struct {
	kind  destinationKind
	value string
}

func UnicastDestination(value string) Destination {
	return Destination{kind: destinationUnicast, value: value}
}

func BroadcastDestination() Destination {
	return Destination{kind: destinationBroadcast}
}

func ResolveDestination() Destination {
	return Destination{kind: destinationResolve}
}

func (d Destination) IsUnicast() bool {
	return d.kind == destinationUnicast
}

func (d Destination) IsBroadcast() bool {
	return d.kind == destinationBroadcast
}

func (d Destination) IsResolve() bool {
	return d.kind == destinationResolve
}

func (d Destination) Value() string {
	return d.value
}

func (d Destination) MarshalJSON() ([]byte, error) {
	switch d.kind {
	case destinationUnicast:
		return json.Marshal(d.value)
	case destinationBroadcast:
		return json.Marshal("broadcast")
	case destinationResolve, 0:
		return []byte("null"), nil
	default:
		return nil, fmt.Errorf("invalid destination kind: %d", d.kind)
	}
}

func (d *Destination) UnmarshalJSON(data []byte) error {
	if string(data) == "null" {
		*d = ResolveDestination()
		return nil
	}
	var value string
	if err := json.Unmarshal(data, &value); err != nil {
		return fmt.Errorf("invalid dst: %w", err)
	}
	if value == "broadcast" {
		*d = BroadcastDestination()
		return nil
	}
	*d = UnicastDestination(value)
	return nil
}

type Meta struct {
	MsgType          string          `json:"type"`
	Msg              *string         `json:"msg,omitempty"`
	SrcIlk           *string         `json:"src_ilk,omitempty"`
	DstIlk           *string         `json:"dst_ilk,omitempty"`
	Ich              *string         `json:"ich,omitempty"`
	ThreadID         *string         `json:"thread_id,omitempty"`
	ThreadSeq        *uint64         `json:"thread_seq,omitempty"`
	Ctx              *string         `json:"ctx,omitempty"`
	CtxSeq           *uint64         `json:"ctx_seq,omitempty"`
	CtxWindow        []CtxTurn       `json:"ctx_window,omitempty"`
	MemoryPackage    *MemoryPackage  `json:"memory_package,omitempty"`
	Scope            *string         `json:"scope,omitempty"`
	Target           *string         `json:"target,omitempty"`
	Action           *string         `json:"action,omitempty"`
	ActionClass      *string         `json:"action_class,omitempty"`
	ActionResult     *string         `json:"action_result,omitempty"`
	ResultOrigin     *string         `json:"result_origin,omitempty"`
	ResultDetailCode *string         `json:"result_detail_code,omitempty"`
	Priority         *string         `json:"priority,omitempty"`
	Context          json.RawMessage `json:"context,omitempty"`
}

type MemoryPackage struct {
	PackageVersion  uint32                  `json:"package_version"`
	ThreadID        string                  `json:"thread_id"`
	DominantContext *MemoryContextSummary   `json:"dominant_context,omitempty"`
	DominantReason  *MemoryReasonSummary    `json:"dominant_reason,omitempty"`
	Contexts        []MemoryContextSummary  `json:"contexts,omitempty"`
	Reasons         []MemoryReasonSummary   `json:"reasons,omitempty"`
	Memories        []MemorySummary         `json:"memories,omitempty"`
	Episodes        []EpisodeSummary        `json:"episodes,omitempty"`
	Truncated       *MemoryPackageTruncated `json:"truncated,omitempty"`
}

type MemoryContextSummary struct {
	ContextID string  `json:"context_id"`
	Label     string  `json:"label"`
	Weight    float64 `json:"weight"`
}

type MemoryReasonSummary struct {
	ReasonID string  `json:"reason_id"`
	Label    string  `json:"label"`
	Weight   float64 `json:"weight"`
}

type MemorySummary struct {
	MemoryID          string  `json:"memory_id"`
	Summary           string  `json:"summary"`
	Weight            float64 `json:"weight"`
	DominantContextID *string `json:"dominant_context_id,omitempty"`
	DominantReasonID  *string `json:"dominant_reason_id,omitempty"`
}

type EpisodeSummary struct {
	EpisodeID string `json:"episode_id"`
	Title     string `json:"title"`
	Intensity uint8  `json:"intensity"`
}

type MemoryPackageTruncated struct {
	Applied         bool   `json:"applied"`
	DroppedContexts uint32 `json:"dropped_contexts"`
	DroppedReasons  uint32 `json:"dropped_reasons"`
	DroppedMemories uint32 `json:"dropped_memories"`
	DroppedEpisodes uint32 `json:"dropped_episodes"`
}

type CtxTurn struct {
	Seq      uint64 `json:"seq"`
	TS       string `json:"ts"`
	From     string `json:"from"`
	TurnType string `json:"type"`
	Text     string `json:"text"`
}

type NodeHelloPayload struct {
	UUID    string `json:"uuid"`
	Name    string `json:"name"`
	Version string `json:"version"`
}

type NodeAnnouncePayload struct {
	UUID       string `json:"uuid"`
	Name       string `json:"name"`
	Status     string `json:"status"`
	VpnID      uint32 `json:"vpn_id"`
	RouterName string `json:"router_name"`
}

type WithdrawPayload struct {
	UUID string `json:"uuid"`
}

const (
	SYSTEMKind               = "system"
	MSGHello                 = "HELLO"
	MSGAnnounce              = "ANNOUNCE"
	MSGWithdraw              = "WITHDRAW"
	MSGUnreachable           = "UNREACHABLE"
	MSGTTLExceeded           = "TTL_EXCEEDED"
	MSGEcho                  = "ECHO"
	MSGEchoReply             = "ECHO_REPLY"
	MSGConfigChanged         = "CONFIG_CHANGED"
	MSGConfigGet             = "CONFIG_GET"
	MSGConfigSet             = "CONFIG_SET"
	MSGConfigResponse        = "CONFIG_RESPONSE"
	MSGOPAReload             = "OPA_RELOAD"
	MSGNodeStatusGet         = "NODE_STATUS_GET"
	MSGNodeStatusGetResponse = "NODE_STATUS_GET_RESPONSE"
	ScopeVPN                 = "vpn"
	ScopeGlobal              = "global"
)

func MarshalPayload(value any) (json.RawMessage, error) {
	if raw, ok := value.(json.RawMessage); ok {
		return raw, nil
	}
	data, err := json.Marshal(value)
	if err != nil {
		return nil, err
	}
	return json.RawMessage(data), nil
}

func BuildSystemMessage(src string, dst Destination, ttl uint8, traceID, msg string, payload any) (Message, error) {
	raw, err := MarshalPayload(payload)
	if err != nil {
		return Message{}, err
	}
	msgCopy := msg
	return Message{
		Routing: Routing{
			Src:     src,
			Dst:     dst,
			TTL:     ttl,
			TraceID: traceID,
		},
		Meta: Meta{
			MsgType: SYSTEMKind,
			Msg:     &msgCopy,
		},
		Payload: raw,
	}, nil
}

func BuildMessageEnvelope(src string, dst Destination, ttl uint8, traceID, msgType string, msgName, action *string, payload any) (Message, error) {
	raw, err := MarshalPayload(payload)
	if err != nil {
		return Message{}, err
	}
	return Message{
		Routing: Routing{
			Src:     src,
			Dst:     dst,
			TTL:     ttl,
			TraceID: traceID,
		},
		Meta: Meta{
			MsgType: msgType,
			Msg:     msgName,
			Action:  action,
		},
		Payload: raw,
	}, nil
}

func BuildHello(src, traceID string, payload NodeHelloPayload) (Message, error) {
	return BuildSystemMessage(src, ResolveDestination(), 1, traceID, MSGHello, payload)
}

func BuildAnnounce(src, dst, traceID string, payload NodeAnnouncePayload) (Message, error) {
	return BuildSystemMessage(src, UnicastDestination(dst), 1, traceID, MSGAnnounce, payload)
}

func BuildWithdraw(src string, dst Destination, traceID, uuid string) (Message, error) {
	return BuildSystemMessage(src, dst, 1, traceID, MSGWithdraw, WithdrawPayload{UUID: uuid})
}

func BuildCommandResponse(src, dst, traceID, action string, payload any) (Message, error) {
	actionCopy := action
	return BuildMessageEnvelope(src, UnicastDestination(dst), 16, traceID, "command_response", nil, &actionCopy, payload)
}

func BuildQueryResponse(src, dst, traceID, action string, payload any) (Message, error) {
	actionCopy := action
	return BuildMessageEnvelope(src, UnicastDestination(dst), 16, traceID, "query_response", nil, &actionCopy, payload)
}

func BuildSystemUnicast(src, dst, traceID, msg string, payload any) (Message, error) {
	return BuildSystemMessage(src, UnicastDestination(dst), 16, traceID, msg, payload)
}

func BuildSystemBroadcast(src, traceID, msg string, payload any, ttl uint8) (Message, error) {
	return BuildSystemMessage(src, BroadcastDestination(), ttl, traceID, msg, payload)
}
