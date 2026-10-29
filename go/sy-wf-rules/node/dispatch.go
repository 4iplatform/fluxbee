package node

import (
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"strings"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

func (s *Service) handleSystemMessage(msg fluxbeesdk.Message) {
	switch stringValue(msg.Meta.Msg) {
	case fluxbeesdk.MSGNodeStatusGet:
		s.handleNodeStatusGet(msg)
	case fluxbeesdk.MSGConfigGet, fluxbeesdk.MSGConfigSet:
		// The system policy already lets only the primary admin send these; checked here too.
		if !fromAdmin(msg) {
			detail := notFromAdmin(msg)["error_detail"]
			s.sendNodeConfigResponse(msg, map[string]any{
				"ok":        false,
				"node_name": s.nodeName(),
				"state":     "error",
				"error":     map[string]any{"code": "UNAUTHORIZED", "message": detail},
			})
			return
		}
		if stringValue(msg.Meta.Msg) == fluxbeesdk.MSGConfigGet {
			s.handleNodeConfigGet(msg)
		} else {
			s.handleNodeConfigSet(msg)
		}
	}
}

func (s *Service) handleCommand(msg fluxbeesdk.Message) {
	action := strings.ToLower(stringValue(msg.Meta.Action))
	switch action {
	case "compile_workflow":
		req, err := decodeCompileRequest(msg.Payload)
		if err != nil {
			s.sendCommandResponse(msg, action, map[string]any{
				"status":       "error",
				"error_code":   "INVALID_CONFIG_SET",
				"error_detail": err.Error(),
			})
			return
		}
		meta, err := s.CompileWorkflow(req)
		if err != nil {
			s.sendCommandResponse(msg, action, commandErrorPayload(err))
			return
		}
		s.sendCommandResponse(msg, action, map[string]any{
			"status":      "ok",
			"version":     meta.Version,
			"staged":      true,
			"guard_count": meta.GuardCount,
			"state_count": meta.StateCount,
		})
	case "apply_workflow":
		var req ApplyRequest
		if err := json.Unmarshal(msg.Payload, &req); err != nil {
			s.sendCommandResponse(msg, action, map[string]any{
				"status":       "error",
				"error_code":   "INVALID_CONFIG_SET",
				"error_detail": err.Error(),
			})
			return
		}
		result, err := s.ApplyWorkflowAndDeploy(req)
		if err != nil {
			s.sendCommandResponse(msg, action, commandErrorPayload(err))
			return
		}
		payload := map[string]any{
			"status":  "ok",
			"version": result.Current.Version,
			"current": buildMetadataSnapshot(result.Current),
			"wf_node": result.WFNode,
		}
		if result.Warning != "" {
			payload["warning"] = result.Warning
		}
		s.sendCommandResponse(msg, action, payload)
	case "rollback_workflow":
		var req RollbackRequest
		if err := json.Unmarshal(msg.Payload, &req); err != nil {
			s.sendCommandResponse(msg, action, map[string]any{
				"status":       "error",
				"error_code":   "INVALID_CONFIG_SET",
				"error_detail": err.Error(),
			})
			return
		}
		result, err := s.RollbackWorkflowAndDeploy(req)
		if err != nil {
			s.sendCommandResponse(msg, action, commandErrorPayload(err))
			return
		}
		payload := map[string]any{
			"status":  "ok",
			"version": result.Current.Version,
			"current": buildMetadataSnapshot(result.Current),
			"wf_node": result.WFNode,
		}
		if result.Warning != "" {
			payload["warning"] = result.Warning
		}
		s.sendCommandResponse(msg, action, payload)
	case "delete_workflow":
		var req DeleteWorkflowRequest
		if err := json.Unmarshal(msg.Payload, &req); err != nil {
			s.sendCommandResponse(msg, action, map[string]any{
				"status":       "error",
				"error_code":   "INVALID_CONFIG_SET",
				"error_detail": err.Error(),
			})
			return
		}
		result, err := s.DeleteWorkflow(req)
		if err != nil {
			s.sendCommandResponse(msg, action, commandErrorPayload(err))
			return
		}
		s.sendCommandResponse(msg, action, map[string]any{
			"status":  "ok",
			"deleted": result.Deleted,
			"wf_node": result.WFNode,
		})
	}
}

func (s *Service) handleQuery(msg fluxbeesdk.Message) {
	action := strings.ToLower(stringValue(msg.Meta.Action))
	switch action {
	case "get_workflow":
		var req GetWorkflowRequest
		if len(msg.Payload) > 0 {
			if err := json.Unmarshal(msg.Payload, &req); err != nil {
				s.sendQueryResponse(msg, action, map[string]any{
					"status":       "error",
					"error_code":   "INVALID_CONFIG_SET",
					"error_detail": err.Error(),
				})
				return
			}
		}
		workflow, err := s.GetWorkflow(req)
		if err != nil {
			s.sendQueryResponse(msg, action, commandErrorPayload(err))
			return
		}
		s.sendQueryResponse(msg, action, map[string]any{
			"status":        "ok",
			"hive":          s.cfg.HiveID,
			"workflow_name": workflow.Metadata.WorkflowName,
			"version":       workflow.Metadata.Version,
			"hash":          workflow.Metadata.Hash,
			"compiled_at":   workflow.Metadata.CompiledAt,
			"definition":    workflow.Definition,
		})
		return
	case "list_workflows":
		workflows, err := s.ListWorkflowStatuses()
		if err != nil {
			s.sendQueryResponse(msg, action, map[string]any{
				"status":       "error",
				"error_code":   "WFRULES_ERROR",
				"error_detail": err.Error(),
			})
			return
		}
		items := make([]map[string]any, 0, len(workflows))
		for _, workflow := range workflows {
			items = append(items, map[string]any{
				"workflow_name":        workflow.WorkflowName,
				"current_version":      formatOptionalUint64(workflow.CurrentVersion),
				"current_hash":         workflow.CurrentHash,
				"published_version":    workflow.PublishedVersion,
				"deployed_version":     workflow.DeployedVersion,
				"wf_node_running":      workflow.WFNodeRunning,
				"wf_node_reachable":    workflow.WFNodeReachable,
				"wf_node_health_state": workflow.WFNodeHealthState,
				"active_instances":     formatOptionalInt(workflow.ActiveInstances),
				"wf_node_timeout":      workflow.WFNodeTimeout,
				"instances_error":      workflow.InstancesError,
			})
		}
		s.sendQueryResponse(msg, action, map[string]any{
			"status":    "ok",
			"hive":      s.cfg.HiveID,
			"workflows": items,
		})
	case "get_status":
		var req GetStatusRequest
		if len(msg.Payload) > 0 {
			if err := json.Unmarshal(msg.Payload, &req); err != nil {
				s.sendQueryResponse(msg, action, map[string]any{
					"status":       "error",
					"error_code":   "INVALID_CONFIG_SET",
					"error_detail": err.Error(),
				})
				return
			}
		}
		statusView, err := s.GetWorkflowStatus(req)
		if err != nil {
			s.sendQueryResponse(msg, action, commandErrorPayload(err))
			return
		}
		payload := map[string]any{
			"status":            "ok",
			"hive":              s.cfg.HiveID,
			"workflow_name":     statusView.WorkflowName,
			"current_version":   formatOptionalUint64(statusView.CurrentVersion),
			"current_hash":      statusView.CurrentHash,
			"staged_version":    formatOptionalUint64(statusView.StagedVersion),
			"last_error":        statusView.LastError,
			"published_version": statusView.PublishedVersion,
			"wf_node": map[string]any{
				"node_name":        statusView.WFNode.NodeName,
				"running":          statusView.WFNode.Running,
				"status_reachable": statusView.WFNode.StatusReachable,
				"health_state":     statusView.WFNode.HealthState,
				"active_instances": formatOptionalInt(statusView.WFNode.ActiveInstances),
				"wf_node_timeout":  statusView.WFNode.Timeout,
				"instances_error":  statusView.WFNode.InstancesError,
				"deployed_version": statusView.WFNode.RuntimeVersion,
			},
		}
		s.sendQueryResponse(msg, action, payload)
	}
}

func (s *Service) handleNodeStatusGet(msg fluxbeesdk.Message) {
	if s.sender == nil {
		return
	}
	resp, err := fluxbeesdk.BuildDefaultNodeStatusResponse(&msg, s.sender.UUID(), "HEALTHY")
	if err == nil {
		err = s.sender.Send(resp)
	}
	if err != nil {
		log.Printf("%s not sent (trace_id=%s): %v", fluxbeesdk.MSGNodeStatusGetResponse, msg.Routing.TraceID, err)
	}
}

func (s *Service) handleNodeConfigGet(msg fluxbeesdk.Message) {
	response := map[string]any{
		"ok":             true,
		"node_name":      s.nodeName(),
		"state":          "configured",
		"schema_version": 1,
		"config_version": 0,
		"effective_config": map[string]any{
			"hive_id":   s.cfg.HiveID,
			"state_dir": s.cfg.StateDir,
			// WF nodes are managed through the admin, like every other launcher.
			"node_actions_via": fluxbeesdk.PrimaryAdminNode,
		},
		"contract": map[string]any{
			"supports":             []string{fluxbeesdk.MSGConfigGet, fluxbeesdk.MSGConfigSet},
			"target":               fluxbeesdk.NodeConfigControlTarget,
			"schema_version":       1,
			"apply_modes":          []string{fluxbeesdk.NodeConfigApplyModeReplace},
			"supported_operations": []string{"check", "compile", "compile_apply", "apply", "rollback"},
		},
	}
	s.sendNodeConfigResponse(msg, response)
}

func (s *Service) handleNodeConfigSet(msg fluxbeesdk.Message) {
	reqMsg, err := fluxbeesdk.ParseNodeConfigRequest(&msg)
	if err != nil || reqMsg == nil || reqMsg.Set == nil {
		s.sendNodeConfigResponse(msg, map[string]any{
			"ok":        false,
			"node_name": s.nodeName(),
			"state":     "error",
			"error": map[string]any{
				"code":   "INVALID_CONFIG_SET",
				"detail": errorString(err, "invalid CONFIG_SET request"),
			},
		})
		return
	}
	req := reqMsg.Set
	if req.ApplyMode != "" && req.ApplyMode != fluxbeesdk.NodeConfigApplyModeReplace {
		s.sendNodeConfigResponse(msg, map[string]any{
			"ok":        false,
			"node_name": s.nodeName(),
			"state":     "error",
			"error": map[string]any{
				"code":   "UNSUPPORTED_APPLY_MODE",
				"detail": fmt.Sprintf("unsupported apply_mode: %s", req.ApplyMode),
			},
		})
		return
	}
	configMap, ok := req.Config.(map[string]any)
	if !ok {
		s.sendNodeConfigResponse(msg, map[string]any{
			"ok":        false,
			"node_name": s.nodeName(),
			"state":     "error",
			"error": map[string]any{
				"code":   "INVALID_CONFIG_SET",
				"detail": "config must be a JSON object",
			},
		})
		return
	}
	operation := strings.TrimSpace(strings.ToLower(stringValueFromMap(configMap, "operation")))
	if operation == "" {
		operation = "compile"
	}
	if operation == "check" {
		operation = "compile"
	}
	switch operation {
	case "compile":
		compileReq, err := configCompileRequest(msg)
		if err != nil {
			s.sendNodeConfigResponse(msg, map[string]any{
				"ok":        false,
				"node_name": s.nodeName(),
				"state":     "error",
				"error": map[string]any{
					"code":   "INVALID_CONFIG_SET",
					"detail": err.Error(),
				},
			})
			return
		}
		meta, err := s.CompileWorkflow(compileReq)
		if err != nil {
			s.sendNodeConfigResponse(msg, configErrorPayload(s.nodeName(), req.SchemaVersion, req.ConfigVersion, err))
			return
		}
		s.sendNodeConfigResponse(msg, map[string]any{
			"ok":             true,
			"node_name":      s.nodeName(),
			"state":          "configured",
			"schema_version": req.SchemaVersion,
			"config_version": meta.Version,
			"effective_config": map[string]any{
				"staged": buildMetadataSnapshot(*meta),
			},
		})
	case "apply":
		var applyReq ApplyRequest
		raw, err := json.Marshal(configMap)
		if err != nil {
			s.sendNodeConfigResponse(msg, configErrorPayload(s.nodeName(), req.SchemaVersion, req.ConfigVersion, WfRulesError{Code: "INVALID_CONFIG_SET", Detail: err.Error()}))
			return
		}
		if err := json.Unmarshal(raw, &applyReq); err != nil {
			s.sendNodeConfigResponse(msg, configErrorPayload(s.nodeName(), req.SchemaVersion, req.ConfigVersion, WfRulesError{Code: "INVALID_CONFIG_SET", Detail: err.Error()}))
			return
		}
		result, err := s.ApplyWorkflowAndDeploy(applyReq)
		if err != nil {
			s.sendNodeConfigResponse(msg, configErrorPayload(s.nodeName(), req.SchemaVersion, req.ConfigVersion, err))
			return
		}
		payload := map[string]any{
			"ok":             true,
			"node_name":      s.nodeName(),
			"state":          "configured",
			"schema_version": req.SchemaVersion,
			"config_version": result.Current.Version,
			"effective_config": map[string]any{
				"current": buildMetadataSnapshot(result.Current),
			},
			"wf_node": result.WFNode,
		}
		if result.Warning != "" {
			payload["warning"] = result.Warning
		}
		s.sendNodeConfigResponse(msg, payload)
	case "rollback":
		var rollbackReq RollbackRequest
		raw, err := json.Marshal(configMap)
		if err != nil {
			s.sendNodeConfigResponse(msg, configErrorPayload(s.nodeName(), req.SchemaVersion, req.ConfigVersion, WfRulesError{Code: "INVALID_CONFIG_SET", Detail: err.Error()}))
			return
		}
		if err := json.Unmarshal(raw, &rollbackReq); err != nil {
			s.sendNodeConfigResponse(msg, configErrorPayload(s.nodeName(), req.SchemaVersion, req.ConfigVersion, WfRulesError{Code: "INVALID_CONFIG_SET", Detail: err.Error()}))
			return
		}
		result, err := s.RollbackWorkflowAndDeploy(rollbackReq)
		if err != nil {
			s.sendNodeConfigResponse(msg, configErrorPayload(s.nodeName(), req.SchemaVersion, req.ConfigVersion, err))
			return
		}
		payload := map[string]any{
			"ok":             true,
			"node_name":      s.nodeName(),
			"state":          "configured",
			"schema_version": req.SchemaVersion,
			"config_version": result.Current.Version,
			"effective_config": map[string]any{
				"current": buildMetadataSnapshot(result.Current),
			},
			"wf_node": result.WFNode,
		}
		if result.Warning != "" {
			payload["warning"] = result.Warning
		}
		s.sendNodeConfigResponse(msg, payload)
	case "compile_apply":
		compileReq, err := configCompileRequest(msg)
		if err != nil {
			s.sendNodeConfigResponse(msg, configErrorPayload(s.nodeName(), req.SchemaVersion, req.ConfigVersion, WfRulesError{Code: "INVALID_CONFIG_SET", Detail: err.Error()}))
			return
		}
		meta, err := s.CompileWorkflow(compileReq)
		if err != nil {
			s.sendNodeConfigResponse(msg, configErrorPayload(s.nodeName(), req.SchemaVersion, req.ConfigVersion, err))
			return
		}
		result, err := s.ApplyWorkflowAndDeploy(ApplyRequest{
			WorkflowName: compileReq.WorkflowName,
			Version:      meta.Version,
			AutoSpawn:    compileReq.AutoSpawn,
			TenantID:     compileReq.TenantID,
			LaunchedBy:   compileReq.LaunchedBy,
		})
		if err != nil {
			s.sendNodeConfigResponse(msg, configErrorPayload(s.nodeName(), req.SchemaVersion, req.ConfigVersion, err))
			return
		}
		payload := map[string]any{
			"ok":             true,
			"node_name":      s.nodeName(),
			"state":          "configured",
			"schema_version": req.SchemaVersion,
			"config_version": result.Current.Version,
			"effective_config": map[string]any{
				"current": buildMetadataSnapshot(result.Current),
			},
			"wf_node": result.WFNode,
		}
		if result.Warning != "" {
			payload["warning"] = result.Warning
		}
		s.sendNodeConfigResponse(msg, payload)
	default:
		s.sendNodeConfigResponse(msg, map[string]any{
			"ok":        false,
			"node_name": s.nodeName(),
			"state":     "error",
			"error": map[string]any{
				"code":   "UNSUPPORTED_OPERATION",
				"detail": fmt.Sprintf("operation %q not implemented yet", operation),
			},
		})
	}
}

func (s *Service) sendNodeConfigResponse(request fluxbeesdk.Message, payload map[string]any) {
	s.sendReply(request, fluxbeesdk.MSGConfigResponse, payload, func(payload map[string]any) (fluxbeesdk.Message, error) {
		return fluxbeesdk.BuildNodeConfigResponseMessage(&request, s.sender.UUID(), payload)
	}, func(err error) map[string]any {
		return map[string]any{
			"ok":        false,
			"node_name": s.nodeName(),
			"state":     "error",
			"error":     map[string]any{"code": "WFRULES_ERROR", "detail": err.Error()},
		}
	})
}

func (s *Service) sendCommandResponse(request fluxbeesdk.Message, action string, payload map[string]any) {
	s.sendActionResponse(request, "command", action, payload)
}

func (s *Service) sendQueryResponse(request fluxbeesdk.Message, action string, payload map[string]any) {
	s.sendActionResponse(request, "query", action, payload)
}

// sendActionResponse answers a command or a query with <ACTION>_RESPONSE.
func (s *Service) sendActionResponse(request fluxbeesdk.Message, kind, action string, payload map[string]any) {
	name := strings.ToUpper(action) + "_RESPONSE"
	s.sendReply(request, name, payload, func(payload map[string]any) (fluxbeesdk.Message, error) {
		return fluxbeesdk.BuildMessageEnvelope(
			s.sender.UUID(),
			fluxbeesdk.UnicastDestination(request.Routing.Src),
			16,
			request.Routing.TraceID,
			kind,
			stringPtr(name),
			stringPtr(action),
			payload,
		)
	}, commandErrorPayload)
}

// sendReply sends the reply that build makes of payload; a failure is logged. A reply that cannot
// go (it does not build, or is over the frame limit) is answered with errorPayload instead, so the
// requester does not wait out its timeout. Any other send failure (the send queue full during a
// reconnect, say) gets no error reply: the work may be done, and the requester's timeout reads
// "unknown, not failed".
func (s *Service) sendReply(
	request fluxbeesdk.Message,
	name string,
	payload map[string]any,
	build func(map[string]any) (fluxbeesdk.Message, error),
	errorPayload func(error) map[string]any,
) {
	if s.sender == nil {
		return
	}
	// send reports refused when the reply itself cannot go: it does not build, or is too large.
	send := func(payload map[string]any) (refused bool, err error) {
		resp, err := build(payload)
		if err != nil {
			return true, err
		}
		err = s.sender.Send(resp)
		var tooLarge *fluxbeesdk.MessageTooLargeError
		return errors.As(err, &tooLarge), err
	}
	refused, err := send(payload)
	if err == nil {
		return
	}
	log.Printf("%s not sent (trace_id=%s): %v", name, request.Routing.TraceID, err)
	if !refused {
		return
	}
	if _, err := send(errorPayload(fmt.Errorf("%s not sent: %w", name, err))); err != nil {
		log.Printf("error %s not sent either (trace_id=%s): %v", name, request.Routing.TraceID, err)
	}
}

func buildMetadataSnapshot(meta WfRulesMetadata) map[string]any {
	return map[string]any{
		"version":       meta.Version,
		"hash":          meta.Hash,
		"workflow_name": meta.WorkflowName,
		"compiled_at":   meta.CompiledAt,
		"guard_count":   meta.GuardCount,
		"state_count":   meta.StateCount,
		"action_count":  meta.ActionCount,
	}
}

func configErrorPayload(nodeName string, schemaVersion uint32, configVersion uint64, err error) map[string]any {
	code := "WFRULES_ERROR"
	detail := errorString(err, "unknown error")
	var wfErr WfRulesError
	if ok := asWfRulesError(err, &wfErr); ok {
		code = wfErr.Code
		detail = wfErr.Detail
	}
	return map[string]any{
		"ok":             false,
		"node_name":      nodeName,
		"state":          "error",
		"schema_version": schemaVersion,
		"config_version": configVersion,
		"error": map[string]any{
			"code":   code,
			"detail": detail,
		},
	}
}

func commandErrorPayload(err error) map[string]any {
	code := "WFRULES_ERROR"
	detail := errorString(err, "unknown error")
	var wfErr WfRulesError
	if ok := asWfRulesError(err, &wfErr); ok {
		code = wfErr.Code
		detail = wfErr.Detail
	}
	return map[string]any{
		"status":       "error",
		"error_code":   code,
		"error_detail": detail,
	}
}

func asWfRulesError(err error, out *WfRulesError) bool {
	value, ok := err.(WfRulesError)
	if !ok {
		return false
	}
	*out = value
	return true
}

func errorString(err error, fallback string) string {
	if err == nil {
		return fallback
	}
	return err.Error()
}

func stringPtr(value string) *string {
	return &value
}

func stringValue(value *string) string {
	if value == nil {
		return ""
	}
	return *value
}

func stringValueFromMap(values map[string]any, key string) string {
	raw, _ := values[key].(string)
	return raw
}

func intValueFromMap(values map[string]any, key string) int {
	switch value := values[key].(type) {
	case int:
		return value
	case int64:
		return int(value)
	case float64:
		return int(value)
	case uint64:
		return int(value)
	default:
		return 0
	}
}
