package sdk

import "fmt"

func IsNodeStatusGetMessage(incoming *Message) bool {
	return IsSystemKind(incoming.Meta.MsgType) && stringValue(incoming.Meta.Msg) == MSGNodeStatusGet
}

func BuildDefaultNodeStatusResponse(incoming *Message, srcUUID, healthState string) (Message, error) {
	if healthState == "" {
		healthState = "HEALTHY"
	}
	return buildNodeStatusResponse(incoming, srcUUID, map[string]any{
		"status":       "ok",
		"health_state": normalizeHealthState(healthState),
	})
}

// BuildManagedNodeStatusResponse answers NODE_STATUS_GET for a managed node from its config state:
// the health it implies and, when the config is not in use, the reason. The orchestrator's
// get_node_status reports both (health_source NODE_REPORTED, last_error).
func BuildManagedNodeStatusResponse(incoming *Message, srcUUID string, state ManagedControlPlaneState) (Message, error) {
	payload := map[string]any{
		"status":       "ok",
		"health_state": ManagedHealthState(state),
	}
	if state.LastError != nil {
		payload["last_error"] = state.LastError
	}
	return buildNodeStatusResponse(incoming, srcUUID, payload)
}

func buildNodeStatusResponse(incoming *Message, srcUUID string, payload map[string]any) (Message, error) {
	if !IsNodeStatusGetMessage(incoming) {
		return Message{}, fmt.Errorf("message is not NODE_STATUS_GET")
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
