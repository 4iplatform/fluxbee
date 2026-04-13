package sdk

import (
	"encoding/json"
	"fmt"
)

const (
	MsgTimerResponse = "TIMER_RESPONSE"
	MsgTimerHelp     = "TIMER_HELP"
)

type TimerError struct {
	Code    string `json:"code"`
	Message string `json:"message"`
}

type TimerResponse struct {
	OK           bool           `json:"ok"`
	Verb         string         `json:"verb,omitempty"`
	TimerUUID    *string        `json:"timer_uuid,omitempty"`
	FireAtUTCMS  *int64         `json:"fire_at_utc_ms,omitempty"`
	Status       *string        `json:"status,omitempty"`
	DeletedCount *int64         `json:"deleted_count,omitempty"`
	Error        *TimerError    `json:"error,omitempty"`
	Extra        map[string]any `json:"-"`
}

func (r *TimerResponse) ResponseError() *SystemResponseError {
	if r == nil || r.OK {
		return nil
	}
	if r.Error == nil {
		return &SystemResponseError{
			ResponseMsg: MsgTimerResponse,
			Verb:        r.Verb,
			Message:     "timer response marked as not ok without error detail",
		}
	}
	return &SystemResponseError{
		ResponseMsg: MsgTimerResponse,
		Verb:        r.Verb,
		Code:        r.Error.Code,
		Message:     r.Error.Message,
	}
}

func (r *TimerResponse) UnmarshalJSON(data []byte) error {
	type alias TimerResponse
	var raw map[string]json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	var out alias
	if err := json.Unmarshal(data, &out); err != nil {
		return err
	}
	*r = TimerResponse(out)
	r.Extra = map[string]any{}
	for _, key := range []string{"ok", "verb", "timer_uuid", "fire_at_utc_ms", "status", "deleted_count", "error"} {
		delete(raw, key)
	}
	for k, v := range raw {
		var decoded any
		if err := json.Unmarshal(v, &decoded); err != nil {
			return err
		}
		r.Extra[k] = decoded
	}
	return nil
}

func ParseTimerResponse(msg *Message) (*TimerResponse, error) {
	if msg == nil {
		return nil, fmt.Errorf("message must be non-nil")
	}
	if msg.Meta.MsgType != SYSTEMKind || stringValue(msg.Meta.Msg) != MsgTimerResponse {
		return nil, fmt.Errorf("message is not TIMER_RESPONSE")
	}
	var payload TimerResponse
	if err := json.Unmarshal(msg.Payload, &payload); err != nil {
		return nil, err
	}
	return &payload, nil
}
