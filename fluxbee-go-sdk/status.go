package sdk

import "fmt"

func IsNodeStatusGetMessage(incoming *Message) bool {
	return incoming.Meta.MsgType == SYSTEMKind && stringValue(incoming.Meta.Msg) == MSGNodeStatusGet
}

func BuildDefaultNodeStatusResponse(incoming *Message, srcUUID, healthState string) (Message, error) {
	if !IsNodeStatusGetMessage(incoming) {
		return Message{}, fmt.Errorf("message is not NODE_STATUS_GET")
	}
	if healthState == "" {
		healthState = "HEALTHY"
	}
	payload := map[string]any{
		"status":       "ok",
		"health_state": normalizeHealthState(healthState),
	}
	return BuildSystemMessage(
		srcUUID,
		UnicastDestination(incoming.Routing.Src),
		16,
		incoming.Routing.TraceID,
		MSGNodeStatusGetResponse,
		payload,
	)
}

func normalizeHealthState(raw string) string {
	switch raw {
	case "HEALTHY", "DEGRADED", "ERROR", "UNKNOWN":
		return raw
	default:
		return "HEALTHY"
	}
}
