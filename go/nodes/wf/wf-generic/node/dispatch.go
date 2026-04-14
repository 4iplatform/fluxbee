package node

import (
	"context"
	"encoding/json"
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

// Dispatcher holds all runtime state needed to dispatch inbound messages.
type NodeRuntime struct {
	Def        *WorkflowDefinition
	DefJSON    string // raw JSON of the definition (for WF_GET_CODE)
	Registry   *InstanceRegistry
	Store      *Store
	ActCtx     ActionContext
	NodeUUID   string
	VersionStr string
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

	default:
		// All other messages: correlate to existing instance or create new one
		return CorrelateAndDispatch(ctx, msg, rt.Registry, rt.Def, rt.Store, rt.ActCtx)
	}
}

// --- Control verb handlers ---

func handleNodeStatusGet(ctx context.Context, msg sdk.Message, rt *NodeRuntime) error {
	resp, err := sdk.BuildDefaultNodeStatusResponse(&msg, rt.NodeUUID, "ok")
	if err != nil {
		return err
	}
	return rt.ActCtx.Dispatcher.SendMsg(resp)
}

func handleWFHelp(ctx context.Context, msg sdk.Message, rt *NodeRuntime) error {
	payload := map[string]any{
		"ok":            true,
		"workflow_type": rt.Def.WorkflowType,
		"description":   rt.Def.Description,
		"initial_state": rt.Def.InitialState,
		"states":        stateNames(rt.Def),
		"terminal_states": rt.Def.TerminalStates,
		"input_schema":  rt.Def.InputSchema,
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
	if req.Limit <= 0 {
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
		// Check if it exists but is already terminated
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

	if inst.Status == "cancelling" {
		return sendErrorReply(ctx, msg, MsgWFCancelInstanceResponse, "INSTANCE_CANCELLING",
			fmt.Sprintf("instance %q is already being cancelled", req.InstanceID), rt)
	}
	if inst.Status != "running" {
		return sendErrorReply(ctx, msg, MsgWFCancelInstanceResponse, "INSTANCE_ALREADY_TERMINATED",
			fmt.Sprintf("instance %q is already %s", req.InstanceID, inst.Status), rt)
	}

	inst.Status = "cancelling"
	if err := rt.Store.UpdateInstance(ctx, mustToRow(inst, rt.ActCtx.Clock)); err != nil {
		inst.Status = "running" // rollback in-memory
		return fmt.Errorf("cancel instance %q: persist: %w", req.InstanceID, err)
	}

	_ = rt.Store.AppendLog(ctx, WFLogEntry{
		InstanceID: req.InstanceID,
		LoggedAtMS: nowMS(rt.ActCtx.Clock),
		ActionType: "cancel_requested",
		Summary:    fmt.Sprintf("cancel requested: %s", req.Reason),
		OK:         true,
	})

	payload := map[string]any{
		"ok":          true,
		"instance_id": req.InstanceID,
		"status":      "cancelling",
	}
	return sendReply(ctx, msg, MsgWFCancelInstanceResponse, payload, rt)
}

// --- Helpers ---

func sendReply(ctx context.Context, orig sdk.Message, replyMsg string, payload any, rt *NodeRuntime) error {
	if rt.ActCtx.Dispatcher == nil {
		return nil
	}
	resp, err := sdk.BuildSystemResponse(&orig, rt.NodeUUID, replyMsg, payload, sdk.SystemEnvelopeOptions{})
	if err != nil {
		return err
	}
	return rt.ActCtx.Dispatcher.SendMsg(resp)
}

func sendErrorReply(ctx context.Context, orig sdk.Message, responseMsg, code, detail string, rt *NodeRuntime) error {
	return sendReply(ctx, orig, responseMsg, map[string]any{
		"ok":     false,
		"error":  code,
		"detail": detail,
	}, rt)
}

func stateNames(def *WorkflowDefinition) []string {
	names := make([]string, len(def.States))
	for i, s := range def.States {
		names[i] = s.Name
	}
	return names
}
