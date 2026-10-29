package node

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"strings"

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

	// msgInvalidInput is the engine's own error answer: a message that starts no instance.
	msgInvalidInput = "INVALID_INPUT"

	errCodeUnauthorized = "UNAUTHORIZED"
	// errCodeTimersNotReconciled: the node runs, but its timers wait for SY.timer to answer
	// (NODE_STATUS_GET reports it as DEGRADED).
	errCodeTimersNotReconciled = "timers_not_reconciled"
)

// NodeRuntime holds all runtime state needed to dispatch inbound messages. Def, DefHash, Registry
// and Store are set only while the node's config is in use (Plane.Configured() != nil).
type NodeRuntime struct {
	// Def is the definition a new instance takes, DefJSON its bytes and DefHash their hash. A running
	// instance keeps the one it was born with.
	Def        *WorkflowDefinition
	DefJSON    string // raw JSON of the definition (for WF_GET_CODE)
	DefHash    string
	Registry   *InstanceRegistry
	Store      *Store
	ActCtx     ActionContext
	NodeUUID   string
	NodeName   string
	Plane      *ControlPlane
	VersionStr string
	// timersPending is why the timer index is not reconciled with SY.timer yet (it did not answer
	// at start): the node runs DEGRADED, and the receive loop tries again until it does.
	timersPending error
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
	msgName := rawMsgName(msg)

	// The managed control plane's verbs: the router delivers them only from the origins the system
	// policy admits (they are protected system actions).
	switch {
	case sdk.IsNodeStatusGetMessage(&msg):
		return handleNodeStatusGet(ctx, msg, rt)

	case sdk.IsNodeConfigGetMessage(&msg):
		return handleNodeConfigGet(ctx, msg, rt)

	case sdk.IsNodeConfigSetMessage(&msg):
		return handleNodeConfigSet(ctx, msg, rt)
	}

	if isNotice(msg) {
		log.Printf("wf: ignoring notice type=%s msg=%q src_l2=%q trace_id=%s", msg.Meta.MsgType, msgName, msg.SourceL2Name(), msg.Routing.TraceID)
		return nil
	}

	// The engine's control verbs answer only the nodes that use them (the router does not protect
	// them, FINDINGS A-119): SY.admin@motherbee, through which every user action goes, and SY.wf-rules
	// of the WF's own hive, which counts a workflow's running instances.
	if responseMsg, ok := wfControlRequests[msgName]; ok {
		if !controlCallerAuthorized(rt.NodeName, msg.SourceL2Name()) {
			log.Printf("wf: %s from %q refused (trace_id=%s): unauthorized", msgName, msg.SourceL2Name(), msg.Routing.TraceID)
			return sendErrorReply(ctx, msg, responseMsg, errCodeUnauthorized,
				fmt.Sprintf("%s is answered only to %s and %s, not %q", msgName, sdk.PrimaryAdminNode, wfRulesNode(rt.NodeName), msg.SourceL2Name()), rt)
		}
		// The engine runs (Def is set) only with a config in use.
		if rt.Def == nil {
			return sendErrorReply(ctx, msg, responseMsg, errCodeNodeNotConfigured, rt.Plane.notConfiguredReason(), rt)
		}
		switch msgName {
		case MsgWFHelp:
			return handleWFHelp(ctx, msg, rt)
		case MsgWFGetInstance:
			return handleWFGetInstance(ctx, msg, rt)
		case MsgWFListInstances:
			return handleWFListInstances(ctx, msg, rt)
		case MsgWFGetCode:
			return handleWFGetCode(ctx, msg, rt)
		default:
			return handleWFCancelInstance(ctx, msg, rt)
		}
	}

	// A system message is control, never a workflow event, but a TIMER_FIRED: since 0.1.90 a WF sends
	// user, an IO node relays user and an agent answers with the request's type (the AI and IO nodes
	// take the system kind only as control, too). It is dropped and never answered.
	if sdk.IsSystemKind(msg.Meta.MsgType) && msgName != sdk.MsgTimerFired {
		log.Printf("wf: ignoring system msg=%q from %q (trace_id=%s): a system message is no workflow event", msgName, msg.SourceL2Name(), msg.Routing.TraceID)
		return nil
	}

	if rt.Def == nil {
		// Workflow traffic a node without a config cannot serve is logged and dropped, as IO nodes
		// do with traffic they cannot serve.
		log.Printf("wf: dropping msg_type=%s msg=%q trace_id=%s: %s", msg.Meta.MsgType, msgName, msg.Routing.TraceID, rt.Plane.notConfiguredReason())
		return nil
	}
	return rt.dispatchWorkflowEvent(ctx, msg)
}

// isNotice reports whether msg is no request and no workflow event, so it is dropped before
// anything else and never answered: the router made it (it carries no routing.src_l2_name, which
// the router stamps on every message a node sends), or it is a presence or liveness notice, a
// router notice, or a system message without a name. An answer to the router's own notice would go
// to the router's UUID, which no node has: the router would answer UNREACHABLE again, and the two
// would bounce it forever (FINDINGS A-112). SY.timer's isInfrastructureMessage and the AI SDK's
// is_infrastructure_message are the same rule; a WF also takes replies (isReply) as events.
func isNotice(msg sdk.Message) bool {
	if strings.TrimSpace(msg.SourceL2Name()) == "" {
		return true
	}
	switch rawMsgName(msg) {
	case sdk.MSGHello, sdk.MSGAnnounce, sdk.MSGWithdraw, sdk.MSGEcho, sdk.MSGEchoReply,
		sdk.MSGUnreachable, sdk.MSGTTLExceeded, sdk.MsgTimerHelp:
		return true
	case "":
		return sdk.IsSystemKind(msg.Meta.MsgType)
	}
	return false
}

// isReply reports whether a message named name answers a request: any *_RESPONSE, the engine's own
// error answer (INVALID_INPUT), or an agent's error (<msg>.error). A reply reaches the instance
// whose thread it carries, and nothing else: it never starts an instance and is never answered, so
// two WFs cannot bounce an INVALID_INPUT (FINDINGS A-119).
func isReply(name string) bool {
	return strings.HasSuffix(name, "_RESPONSE") || name == msgInvalidInput || strings.HasSuffix(name, ".error")
}

// neverAnswered reports whether msg gets no answer from the engine, an error included: a notice, a
// reply, or a message without a name (the AI SDK's and SY.timer's rule). A message without a name,
// as an IO node's text, still starts an instance.
func neverAnswered(msg sdk.Message) bool {
	name := rawMsgName(msg)
	return isNotice(msg) || name == "" || isReply(name)
}

// controlCallerAuthorized reports whether sender, the router-stamped name of a message to the WF
// node nodeName, may use the engine's control verbs: SY.admin@motherbee or SY.wf-rules of the WF's
// own hive.
func controlCallerAuthorized(nodeName, sender string) bool {
	sender = strings.TrimSpace(sender)
	if sender == sdk.PrimaryAdminNode {
		return true
	}
	wfRules := wfRulesNode(nodeName)
	return wfRules != "" && sender == wfRules
}

// wfRulesNode is SY.wf-rules of the WF node's hive, "" when the name has none.
func wfRulesNode(nodeName string) string {
	if hive := nodeHive(nodeName); hive != "" {
		return "SY.wf-rules@" + hive
	}
	return ""
}

// timerAuthority is the only sender a WF node takes a TIMER_FIRED from: SY.timer of its own hive.
func timerAuthority(nodeName string) string {
	if hive := nodeHive(nodeName); hive != "" {
		return "SY.timer@" + hive
	}
	return ""
}

func nodeHive(nodeName string) string {
	_, hive, ok := strings.Cut(strings.TrimSpace(nodeName), "@")
	if !ok {
		return ""
	}
	return strings.TrimSpace(hive)
}

func rawMsgName(msg sdk.Message) string {
	if msg.Meta.Msg == nil {
		return ""
	}
	return *msg.Meta.Msg
}

// --- Control verb handlers ---

func handleNodeStatusGet(ctx context.Context, msg sdk.Message, rt *NodeRuntime) error {
	resp, err := sdk.BuildManagedNodeStatusResponse(&msg, rt.NodeUUID, rt.Plane.State)
	if rt.timersPending != nil && rt.Plane.Configured() != nil {
		// Running, but its timers wait for SY.timer: DEGRADED, saying why (the orchestrator's
		// get_node_status reports both).
		resp, err = sdk.BuildSystemMessage(rt.NodeUUID, sdk.UnicastDestination(msg.Routing.Src), 16, msg.Routing.TraceID,
			sdk.MSGNodeStatusGetResponse, map[string]any{
				"status":       "ok",
				"health_state": "DEGRADED",
				"last_error":   sdk.ControlPlaneError{Code: errCodeTimersNotReconciled, Message: rt.timersPending.Error()},
			})
	}
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
		"instance": instanceDetail(row, logs),
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
	instances := make([]map[string]any, 0, len(rows))
	for _, row := range rows {
		instances = append(instances, instanceSummary(row))
	}
	payload := map[string]any{
		"ok":        true,
		"count":     len(rows),
		"instances": instances,
	}
	return sendReply(ctx, msg, MsgWFListInstancesResponse, payload, rt)
}

// instanceSummary is an instance as WF_LIST_INSTANCES lists it.
func instanceSummary(row WFInstanceRow) map[string]any {
	var terminatedAt any
	if row.TerminatedAtMS.Valid {
		terminatedAt = row.TerminatedAtMS.Int64
	}
	return map[string]any{
		"instance_id":      row.InstanceID,
		"workflow_type":    row.WorkflowType,
		"definition_hash":  row.DefinitionHash,
		"status":           row.Status,
		"current_state":    row.CurrentState,
		"created_at_ms":    row.CreatedAtMS,
		"updated_at_ms":    row.UpdatedAtMS,
		"terminated_at_ms": terminatedAt,
	}
}

// instanceDetail is an instance as WF_GET_INSTANCE answers it: its summary, its input and its state
// variables as the store keeps them, and its most recent log entries, newest first.
func instanceDetail(row WFInstanceRow, logs []WFLogEntry) map[string]any {
	detail := instanceSummary(row)
	detail["input"] = storedObject(row.InputJSON)
	detail["state_variables"] = storedObject(row.StateJSON)
	recent := make([]map[string]any, 0, len(logs))
	for _, entry := range logs {
		recent = append(recent, map[string]any{
			"logged_at_ms": entry.LoggedAtMS,
			"action_type":  entry.ActionType,
			"summary":      entry.Summary,
			"ok":           entry.OK,
			"error_detail": entry.ErrorDetail,
		})
	}
	detail["recent_log"] = recent
	return detail
}

// storedObject is a JSON object the store keeps, as it is written there.
func storedObject(stored string) json.RawMessage {
	if strings.TrimSpace(stored) == "" {
		return json.RawMessage(`{}`)
	}
	return json.RawMessage(stored)
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
	err := rt.protect(ctx, inst, msg, takenEvent{}, func() error {
		return inst.runCancelTransition(ctx, msg, rt.ActCtx, takenEvent{})
	})
	rt.settle(ctx, inst)
	if err != nil {
		return fmt.Errorf("cancel instance %q: transition to cancelled: %w", req.InstanceID, err)
	}

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
