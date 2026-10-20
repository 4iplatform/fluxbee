package node

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// WF control message names
const (
	MsgWFHelp                   = "WF_HELP"
	MsgWFHelpResponse           = "WF_HELP_RESPONSE"
	MsgWFGetInstance            = "WF_GET_INSTANCE"
	MsgWFGetInstanceResponse    = "WF_GET_INSTANCE_RESPONSE"
	MsgWFListInstances          = "WF_LIST_INSTANCES"
	MsgWFListInstancesResponse  = "WF_LIST_INSTANCES_RESPONSE"
	MsgWFCancelInstance         = "WF_CANCEL_INSTANCE"
	MsgWFCancelInstanceResponse = "WF_CANCEL_INSTANCE_RESPONSE"
	MsgWFGetCode                = "WF_GET_CODE"
	MsgWFGetCodeResponse        = "WF_GET_CODE_RESPONSE"

	// Timer response messages — informational only, no state mutation needed
	MsgTimerScheduleResponse   = "TIMER_SCHEDULE_RESPONSE"
	MsgTimerCancelResponse     = "TIMER_CANCEL_RESPONSE"
	MsgTimerRescheduleResponse = "TIMER_RESCHEDULE_RESPONSE"
)

// NodeRuntime holds all runtime state needed to dispatch inbound messages. Def, Registry and
// Store are set only while the node's config is in use (Plane.Configured() != nil).
type NodeRuntime struct {
	Def        *WorkflowDefinition
	DefJSON    string // raw JSON of the definition (for WF_GET_CODE)
	Registry   *InstanceRegistry
	Store      *Store
	ActCtx     ActionContext
	NodeUUID   string
	NodeName   string
	Plane      *ControlPlane
	VersionStr string
}

// wfControlRequests maps each WF control request to its response, so a node without a config in
// use can still answer it.
var wfControlRequests = map[string]string{
	MsgWFHelp:           MsgWFHelpResponse,
	MsgWFGetInstance:    MsgWFGetInstanceResponse,
	MsgWFListInstances:  MsgWFListInstancesResponse,
	MsgWFGetCode:        MsgWFGetCodeResponse,
	MsgWFCancelInstance: MsgWFCancelInstanceResponse,
}

// Dispatch routes a single inbound message to the appropriate handler.
func Dispatch(ctx context.Context, msg sdk.Message, rt *NodeRuntime) error {
	msgName := ""
	if msg.Meta.Msg != nil {
		msgName = *msg.Meta.Msg
	}

	switch {
	case sdk.IsNodeStatusGetMessage(&msg):
		return handleNodeStatusGet(ctx, msg, rt)

	case sdk.IsNodeConfigGetMessage(&msg):
		return handleNodeConfigGet(ctx, msg, rt)

	case sdk.IsNodeConfigSetMessage(&msg):
		return handleNodeConfigSet(ctx, msg, rt)
	}

	// The engine runs (Def is set) only with a config in use.
	if rt.Def == nil {
		return dispatchWithoutConfig(ctx, msg, msgName, rt)
	}

	switch {
	case msgName == MsgWFHelp:
		return handleWFHelp(ctx, msg, rt)

	case msgName == MsgWFGetInstance:
		return handleWFGetInstance(ctx, msg, rt)

	case msgName == MsgWFListInstances:
		return handleWFListInstances(ctx, msg, rt)

	case msgName == MsgWFGetCode:
		return handleWFGetCode(ctx, msg, rt)

	case msgName == MsgWFCancelInstance:
		return handleWFCancelInstance(ctx, msg, rt)

	case msgName == MsgTimerScheduleResponse,
		msgName == MsgTimerCancelResponse,
		msgName == MsgTimerRescheduleResponse:
		// Informational only — we use client_ref so UUIDs are not needed
		log.Printf("wf: %s received (informational, no action needed)", msgName)
		return nil

	case shouldIgnoreInfrastructureMessage(msg):
		log.Printf("wf: ignoring infrastructure message type=%s msg=%q trace_id=%s", msg.Meta.MsgType, msgName, msg.Routing.TraceID)
		return nil

	default:
		// All other messages: correlate to existing instance or create new one
		return CorrelateAndDispatch(ctx, msg, rt.Registry, rt.Def, rt.Store, rt.ActCtx)
	}
}

// dispatchWithoutConfig serves a node whose config is not in use: a WF control request gets a
// node_not_configured answer, and workflow traffic is logged and dropped, as IO nodes do with
// traffic they cannot serve.
func dispatchWithoutConfig(ctx context.Context, msg sdk.Message, msgName string, rt *NodeRuntime) error {
	if responseMsg, ok := wfControlRequests[msgName]; ok {
		return sendErrorReply(ctx, msg, responseMsg, errCodeNodeNotConfigured, rt.Plane.notConfiguredReason(), rt)
	}
	if shouldIgnoreInfrastructureMessage(msg) {
		return nil
	}
	log.Printf("wf: dropping msg_type=%s msg=%q trace_id=%s: %s", msg.Meta.MsgType, msgName, msg.Routing.TraceID, rt.Plane.notConfiguredReason())
	return nil
}

func shouldIgnoreInfrastructureMessage(msg sdk.Message) bool {
	if !sdk.IsSystemKind(msg.Meta.MsgType) {
		return false
	}
	msgName := ""
	if msg.Meta.Msg != nil {
		msgName = *msg.Meta.Msg
	}
	switch msgName {
	case "",
		sdk.MSGHello,
		sdk.MSGAnnounce,
		sdk.MSGWithdraw,
		sdk.MSGEcho,
		sdk.MSGEchoReply,
		sdk.MSGUnreachable,
		sdk.MSGTTLExceeded,
		sdk.MSGConfigResponse,
		sdk.MSGNodeStatusGetResponse,
		sdk.MsgTimerResponse,
		sdk.MsgTimerHelp:
		return true
	default:
		return false
	}
}

// --- Control verb handlers ---

func handleNodeStatusGet(ctx context.Context, msg sdk.Message, rt *NodeRuntime) error {
	resp, err := sdk.BuildManagedNodeStatusResponse(&msg, rt.NodeUUID, rt.Plane.State)
	if err != nil {
		return err
	}
	return rt.ActCtx.Dispatcher.SendMsg(resp)
}

func handleNodeConfigGet(ctx context.Context, msg sdk.Message, rt *NodeRuntime) error {
	reqMsg, err := sdk.ParseNodeConfigRequest(&msg)
	if err != nil || reqMsg == nil || reqMsg.Get == nil {
		return sendNodeConfigResponse(ctx, msg, wfConfigErrorPayload(rt.NodeName, rt.Plane, sdk.ErrCodeInvalidConfig, errorString(err, "invalid CONFIG_GET request")), rt)
	}
	req := reqMsg.Get
	if req.NodeName != "" && rt.NodeName != "" && req.NodeName != rt.NodeName {
		return sendNodeConfigResponse(ctx, msg, wfConfigErrorPayload(rt.NodeName, rt.Plane, sdk.ErrCodeInvalidConfig, fmt.Sprintf("request node_name %q does not match runtime node %q", req.NodeName, rt.NodeName)), rt)
	}
	return sendNodeConfigResponse(ctx, msg, buildWFConfigGetPayload(rt.NodeName, rt.Plane), rt)
}

func handleNodeConfigSet(ctx context.Context, msg sdk.Message, rt *NodeRuntime) error {
	reqMsg, err := sdk.ParseNodeConfigRequest(&msg)
	if err != nil || reqMsg == nil || reqMsg.Set == nil {
		return sendNodeConfigResponse(ctx, msg, wfConfigErrorPayload(rt.NodeName, rt.Plane, sdk.ErrCodeInvalidConfig, errorString(err, "invalid CONFIG_SET request")), rt)
	}
	req := reqMsg.Set
	if req.NodeName != "" && rt.NodeName != "" && req.NodeName != rt.NodeName {
		return sendNodeConfigResponse(ctx, msg, wfConfigErrorPayload(rt.NodeName, rt.Plane, sdk.ErrCodeInvalidConfig, fmt.Sprintf("request node_name %q does not match runtime node %q", req.NodeName, rt.NodeName)), rt)
	}
	return sendNodeConfigResponse(ctx, msg, applyWFConfigSet(rt.NodeName, rt.Plane, req), rt)
}

func handleWFHelp(ctx context.Context, msg sdk.Message, rt *NodeRuntime) error {
	payload := map[string]any{
		"ok":              true,
		"workflow_type":   rt.Def.WorkflowType,
		"description":     rt.Def.Description,
		"initial_state":   rt.Def.InitialState,
		"states":          stateNames(rt.Def),
		"terminal_states": rt.Def.TerminalStates,
		"input_schema":    rt.Def.InputSchema,
	}
	return sendReply(ctx, msg, MsgWFHelpResponse, payload, rt)
}

func handleWFGetCode(ctx context.Context, msg sdk.Message, rt *NodeRuntime) error {
	payload := map[string]any{
		"ok":              true,
		"workflow_type":   rt.Def.WorkflowType,
		"definition_json": rt.DefJSON,
	}
	return sendReply(ctx, msg, MsgWFGetCodeResponse, payload, rt)
}

func handleWFGetInstance(ctx context.Context, msg sdk.Message, rt *NodeRuntime) error {
	var req struct {
		InstanceID string `json:"instance_id"`
		LogLimit   int    `json:"log_limit"`
	}
	if err := json.Unmarshal(msg.Payload, &req); err != nil || req.InstanceID == "" {
		return sendErrorReply(ctx, msg, MsgWFGetInstanceResponse, "INVALID_REQUEST", "instance_id required", rt)
	}
	row, err := rt.Store.GetInstance(ctx, req.InstanceID)
	if err != nil {
		return sendErrorReply(ctx, msg, MsgWFGetInstanceResponse, "INSTANCE_NOT_FOUND", err.Error(), rt)
	}
	limit := req.LogLimit
	if limit <= 0 {
		limit = 20
	}
	logs, err := rt.Store.GetRecentLog(ctx, req.InstanceID, limit)
	if err != nil {
		logs = nil
	}
	payload := map[string]any{
		"ok":       true,
		"instance": row,
		"log":      logs,
	}
	return sendReply(ctx, msg, MsgWFGetInstanceResponse, payload, rt)
}

func handleWFListInstances(ctx context.Context, msg sdk.Message, rt *NodeRuntime) error {
	var req struct {
		Status         string `json:"status"`
		Limit          int    `json:"limit"`
		CreatedAfterMS int64  `json:"created_after_ms"`
	}
	_ = json.Unmarshal(msg.Payload, &req)
	if req.Limit < 0 {
		req.Limit = 50
	}
	rows, err := rt.Store.ListInstances(ctx, req.Status, req.Limit, req.CreatedAfterMS)
	if err != nil {
		return sendErrorReply(ctx, msg, MsgWFListInstancesResponse, "STORE_ERROR", err.Error(), rt)
	}
	payload := map[string]any{
		"ok":        true,
		"count":     len(rows),
		"instances": rows,
	}
	return sendReply(ctx, msg, MsgWFListInstancesResponse, payload, rt)
}

func handleWFCancelInstance(ctx context.Context, msg sdk.Message, rt *NodeRuntime) error {
	var req struct {
		InstanceID string `json:"instance_id"`
		Reason     string `json:"reason"`
	}
	if err := json.Unmarshal(msg.Payload, &req); err != nil || req.InstanceID == "" {
		return sendErrorReply(ctx, msg, MsgWFCancelInstanceResponse, "INVALID_REQUEST", "instance_id required", rt)
	}

	inst, ok := rt.Registry.Get(req.InstanceID)
	if !ok {
		row, err := rt.Store.GetInstance(ctx, req.InstanceID)
		if err != nil {
			return sendErrorReply(ctx, msg, MsgWFCancelInstanceResponse, "INSTANCE_NOT_FOUND", fmt.Sprintf("instance %q not found", req.InstanceID), rt)
		}
		if row.Status != "running" && row.Status != "cancelling" {
			return sendErrorReply(ctx, msg, MsgWFCancelInstanceResponse, "INSTANCE_ALREADY_TERMINATED",
				fmt.Sprintf("instance %q is already %s", req.InstanceID, row.Status), rt)
		}
		return sendErrorReply(ctx, msg, MsgWFCancelInstanceResponse, "INSTANCE_NOT_FOUND", "instance not in active registry", rt)
	}

	inst.Lock()
	defer inst.Unlock()

	if inst.Status != "running" && inst.Status != "cancelling" {
		return sendErrorReply(ctx, msg, MsgWFCancelInstanceResponse, "INSTANCE_ALREADY_TERMINATED",
			fmt.Sprintf("instance %q is already %s", req.InstanceID, inst.Status), rt)
	}

	_ = rt.Store.AppendLog(ctx, WFLogEntry{
		InstanceID: req.InstanceID,
		LoggedAtMS: nowMS(rt.ActCtx.Clock),
		ActionType: "cancel_requested",
		Summary:    fmt.Sprintf("cancel requested: %s", req.Reason),
		OK:         true,
	})
	if err := inst.runCancelTransition(ctx, msg, rt.ActCtx, nil); err != nil {
		return fmt.Errorf("cancel instance %q: transition to cancelled: %w", req.InstanceID, err)
	}
	rt.Registry.Remove(req.InstanceID)

	payload := map[string]any{
		"ok":          true,
		"instance_id": req.InstanceID,
		"status":      "cancelled",
	}
	return sendReply(ctx, msg, MsgWFCancelInstanceResponse, payload, rt)
}

// --- Helpers ---

func sendReply(ctx context.Context, orig sdk.Message, replyMsg string, payload any, rt *NodeRuntime) error {
	if rt.ActCtx.Dispatcher == nil {
		return nil
	}
	return replyOrError(rt, replyMsg, payload, func(payload any) (sdk.Message, error) {
		return sdk.BuildSystemResponse(&orig, rt.NodeUUID, replyMsg, payload, sdk.SystemEnvelopeOptions{})
	}, func(detail string) any {
		return wfErrorPayload("INTERNAL_ERROR", detail)
	})
}

func sendNodeConfigResponse(ctx context.Context, orig sdk.Message, payload any, rt *NodeRuntime) error {
	if rt.ActCtx.Dispatcher == nil {
		return nil
	}
	return replyOrError(rt, sdk.MSGConfigResponse, payload, func(payload any) (sdk.Message, error) {
		return sdk.BuildNodeConfigResponseMessage(&orig, rt.NodeUUID, payload)
	}, func(detail string) any {
		return wfConfigErrorPayload(rt.NodeName, rt.Plane, sdk.ErrCodeInternal, detail)
	})
}

// replyOrError sends the reply that build makes of payload. A reply that cannot go (it does not
// build, or is over the frame limit, say a WF_GET_CODE of a large definition) is answered with the
// error that errorPayload makes instead, so the requester does not wait out its timeout. Any other
// send failure (the send queue full during a reconnect, say) gets no error reply: the work may be
// done, and the requester's timeout reads "unknown, not failed". Every failure is returned, for
// the receive loop to log.
func replyOrError(rt *NodeRuntime, name string, payload any, build func(payload any) (sdk.Message, error), errorPayload func(detail string) any) error {
	// send reports refused when the reply itself cannot go: it does not build, or is too large.
	send := func(payload any) (refused bool, err error) {
		resp, err := build(payload)
		if err != nil {
			return true, err
		}
		err = rt.ActCtx.Dispatcher.SendMsg(resp)
		var tooLarge *sdk.MessageTooLargeError
		return errors.As(err, &tooLarge), err
	}
	refused, err := send(payload)
	if err == nil {
		return nil
	}
	err = fmt.Errorf("%s not sent: %w", name, err)
	if !refused {
		return err
	}
	if _, replyErr := send(errorPayload(err.Error())); replyErr != nil {
		return fmt.Errorf("%w (the error reply was not sent either: %v)", err, replyErr)
	}
	return err
}

func sendErrorReply(ctx context.Context, orig sdk.Message, responseMsg, code, detail string, rt *NodeRuntime) error {
	return sendReply(ctx, orig, responseMsg, wfErrorPayload(code, detail), rt)
}

// wfErrorPayload is how a WF_* control request fails: {ok: false, error: <code>, detail}.
func wfErrorPayload(code, detail string) map[string]any {
	return map[string]any{
		"ok":     false,
		"error":  code,
		"detail": detail,
	}
}

func errorString(err error, fallback string) string {
	if err == nil {
		return fallback
	}
	return err.Error()
}

func stateNames(def *WorkflowDefinition) []string {
	names := make([]string, len(def.States))
	for i, s := range def.States {
		names[i] = s.Name
	}
	return names
}
