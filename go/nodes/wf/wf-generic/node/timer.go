package node

import (
	"encoding/json"
	"fmt"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// timerClientRef constructs the canonical client_ref for a workflow timer.
// Format: "wf:<instance_id>::<timer_key>"
// This is the key used in all TIMER_CANCEL / TIMER_RESCHEDULE / TIMER_GET operations
// with SY.timer v1.1, eliminating the need to track UUIDs locally.
func timerClientRef(instanceID, timerKey string) string {
	return fmt.Sprintf("wf:%s::%s", instanceID, timerKey)
}

// firedTimer reads whose timer a TIMER_FIRED is and its key, from the client_ref the WF schedules
// every timer with (the payload it gives SY.timer, {instance_id, timer_key}, is not read: a fire
// without the client_ref is no timer of the WF's).
func firedTimer(msg sdk.Message) (instanceID, timerKey string, err error) {
	var fired sdk.FiredEvent
	if err := json.Unmarshal(msg.Payload, &fired); err != nil {
		return "", "", fmt.Errorf("parse TIMER_FIRED: %w", err)
	}
	if fired.ClientRef == nil {
		return "", "", fmt.Errorf("a TIMER_FIRED without the client_ref of a WF timer")
	}
	return parseTimerClientRef(*fired.ClientRef)
}

// parseTimerClientRef extracts instanceID and timerKey from a client_ref.
// Returns an error if the format does not match.
func parseTimerClientRef(ref string) (instanceID, timerKey string, err error) {
	const prefix = "wf:"
	const sep = "::"
	if len(ref) <= len(prefix) {
		return "", "", fmt.Errorf("invalid timer client_ref: %q", ref)
	}
	after, ok := cutPrefix(ref, prefix)
	if !ok {
		return "", "", fmt.Errorf("invalid timer client_ref (missing wf: prefix): %q", ref)
	}
	idx := indexOf(after, sep)
	if idx < 0 {
		return "", "", fmt.Errorf("invalid timer client_ref (missing :: separator): %q", ref)
	}
	instanceID = after[:idx]
	timerKey = after[idx+len(sep):]
	if instanceID == "" || timerKey == "" {
		return "", "", fmt.Errorf("invalid timer client_ref (empty segment): %q", ref)
	}
	return instanceID, timerKey, nil
}

func cutPrefix(s, prefix string) (string, bool) {
	if len(s) >= len(prefix) && s[:len(prefix)] == prefix {
		return s[len(prefix):], true
	}
	return s, false
}

func indexOf(s, sub string) int {
	if len(sub) == 0 {
		return 0
	}
	for i := 0; i <= len(s)-len(sub); i++ {
		if s[i:i+len(sub)] == sub {
			return i
		}
	}
	return -1
}
