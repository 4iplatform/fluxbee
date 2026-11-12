package sdk

import (
	"encoding/json"
	"fmt"
	"strings"
)

// NOT_TAKEN: a node's answer to the sender of a message it took nowhere (docs/reference/protocol.md
// §6.10). A system message to the sender's uuid (routing.src), on the message's trace_id and
// thread_id, with NotTakenPayload. Two kinds of node send it:
//   - the SDK of a Go node, for a message that does not fit its command channel (FINDINGS A-329):
//     reason NotTakenReasonNodeBusy, retryable;
//   - the WF engine, for a message its workflow takes nowhere (docs/spec-systemnode-wf-v1_1.md
//     §14.5): no_thread, no_conversation, invalid_input or not_expected, not retryable, and
//     unavailable, retryable.
//
// A NOT_TAKEN is a reply: it is never answered, by anyone, so two nodes never bounce one.

// MSGNotTaken is NOT_TAKEN's meta.msg.
const MSGNotTaken = "NOT_TAKEN"

// NotTakenReasonNodeBusy is the reason of the SDK's NOT_TAKEN: the node's command channel holds as
// many messages of the message's lane as it takes (commandLaneMaxMessages, commandLaneMaxBytes),
// so the message was not queued. It is retryable: the same message sent again once the node has
// read some is taken.
const NotTakenReasonNodeBusy = "node_busy"

// NotTakenPayload is what a NOT_TAKEN says: ok false, error "not_taken", the reason, a detail for
// the logs, and whether the same message sent again may be taken (retryable). A NOT_TAKEN without
// retryable, from a node before 0.1.141, is not retryable.
type NotTakenPayload struct {
	OK        bool   `json:"ok"`
	Error     string `json:"error"`
	Reason    string `json:"reason"`
	Detail    string `json:"detail"`
	Retryable bool   `json:"retryable"`
}

// BuildNotTaken is the NOT_TAKEN a node whose uuid is srcUUID sends the sender of msg: to msg's
// routing.src, on its trace_id and thread_id, ttl 16 (to reach a sender on another hive), kind
// system, no meta.action. It is an error to build one for a message without a sender uuid; the
// caller decides first whether msg is answered at all (IsNoticeOrReply).
func BuildNotTaken(srcUUID string, msg Message, reason, detail string, retryable bool) (Message, error) {
	replyTo := strings.TrimSpace(msg.Routing.Src)
	if replyTo == "" {
		return Message{}, fmt.Errorf("%s: the message has no sender uuid", MSGNotTaken)
	}
	payload, err := json.Marshal(NotTakenPayload{
		OK:        false,
		Error:     "not_taken",
		Reason:    reason,
		Detail:    detail,
		Retryable: retryable,
	})
	if err != nil {
		return Message{}, err
	}
	name := MSGNotTaken
	answer := Message{
		Routing: Routing{
			Src:     srcUUID,
			Dst:     UnicastDestination(replyTo),
			TTL:     16,
			TraceID: msg.Routing.TraceID,
		},
		Meta: Meta{
			MsgType: SYSTEMKind,
			Msg:     &name,
		},
		Payload: payload,
	}
	if msg.Meta.ThreadID != nil {
		thread := *msg.Meta.ThreadID
		answer.Meta.ThreadID = &thread
	}
	return answer, nil
}

// ParseNotTaken reads msg as a NOT_TAKEN: its payload, and true, when msg is one (kind system, any
// case, msg NOT_TAKEN). A payload that does not parse reads as empty, not retryable.
func ParseNotTaken(msg Message) (NotTakenPayload, bool) {
	if !IsSystemKind(msg.Meta.MsgType) || stringValue(msg.Meta.Msg) != MSGNotTaken {
		return NotTakenPayload{}, false
	}
	var payload NotTakenPayload
	if json.Unmarshal(msg.Payload, &payload) != nil {
		return NotTakenPayload{}, true
	}
	return payload, true
}

// IsNoticeOrReply reports whether msg is answered by no one, an error or a NOT_TAKEN included: the
// router made it (it carries no routing.src_l2_name, which the router stamps on every message a node
// sends), it is a presence or router notice (HELLO, ANNOUNCE, WITHDRAW, UNREACHABLE, TTL_EXCEEDED)
// or a system message without a name, or it is a reply (IsReplyName). An answer to the router's
// notice would go to the router's uuid, which no node has, and the router would answer UNREACHABLE
// again, forever (FINDINGS A-112); an answer to a reply could bounce between two nodes (FINDINGS
// A-119). It is the rule of the WF engine (isNotice and isReply), SY.timer (isInfrastructureMessage)
// and the AI SDK (is_infrastructure_message), which also leave a message without a name unanswered
// when they would answer it with an error; a person's text, a user message without a name, gets a
// NOT_TAKEN.
func IsNoticeOrReply(msg *Message) bool {
	if strings.TrimSpace(msg.SourceL2Name()) == "" {
		return true
	}
	name := stringValue(msg.Meta.Msg)
	switch name {
	case MSGHello, MSGAnnounce, MSGWithdraw, MSGUnreachable, MSGTTLExceeded:
		return true
	case "":
		return IsSystemKind(msg.Meta.MsgType)
	}
	return IsReplyName(name)
}

// IsReplyName reports whether a message named name answers another: any *_RESPONSE, a NOT_TAKEN, or
// an agent's error (<msg>.error).
func IsReplyName(name string) bool {
	return strings.HasSuffix(name, "_RESPONSE") || name == MSGNotTaken || strings.HasSuffix(name, ".error")
}
