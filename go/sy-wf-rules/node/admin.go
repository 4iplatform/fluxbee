package node

import (
	"context"
	"encoding/json"
	"fmt"
	"path/filepath"
	"strings"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
	"github.com/google/uuid"
)

const (
	adminRPCTimeout             = 5 * time.Second
	adminCommandMsg             = "ADMIN_COMMAND"
	adminCommandResponseMsg     = "ADMIN_COMMAND_RESPONSE"
	publishRuntimePackageAction = "publish_runtime_package"
)

type adminClient interface {
	PublishRuntimePackage(ctx context.Context, packageFiles map[string]string) (*PackagePublishResult, error)
}

type adminActionError struct {
	Status  string
	Code    string
	Message string
	Payload map[string]any
}

func (e *adminActionError) Error() string {
	if e == nil {
		return ""
	}
	if e.Code != "" && e.Message != "" {
		return fmt.Sprintf("%s: %s", e.Code, e.Message)
	}
	if e.Message != "" {
		return e.Message
	}
	if e.Code != "" {
		return e.Code
	}
	if e.Status != "" {
		return e.Status
	}
	return "admin action failed"
}

type l2AdminClient struct {
	sender     sender
	receiver   rpcReceiver
	targetNode string
	distRoot   string
}

func newAdminClient(cfg NodeConfig, snd sender, rcv rpcReceiver) adminClient {
	if snd == nil || rcv == nil {
		return nil
	}
	return &l2AdminClient{
		sender:     snd,
		receiver:   rcv,
		targetNode: fmt.Sprintf("SY.admin@%s", cfg.HiveID),
		distRoot:   cfg.DistRuntimeRoot,
	}
}

func (c *l2AdminClient) PublishRuntimePackage(ctx context.Context, packageFiles map[string]string) (*PackagePublishResult, error) {
	if c == nil || c.sender == nil || c.receiver == nil {
		return nil, fmt.Errorf("admin client unavailable")
	}
	params := map[string]any{
		"source": map[string]any{
			"kind":  "inline_package",
			"files": packageFiles,
		},
		"set_current": true,
	}
	response, err := c.request(ctx, map[string]any{
		"action":     publishRuntimePackageAction,
		"params":     params,
		"request_id": uuid.NewString(),
	})
	if err != nil {
		return nil, err
	}
	payload, _ := response["payload"].(map[string]any)
	if payload == nil {
		return nil, fmt.Errorf("admin publish response missing payload")
	}
	runtimeName := strings.TrimSpace(stringValueFromMap(payload, "runtime_name"))
	runtimeVersion := strings.TrimSpace(stringValueFromMap(payload, "runtime_version"))
	if runtimeName == "" || runtimeVersion == "" {
		return nil, fmt.Errorf("admin publish response missing runtime_name/runtime_version")
	}
	packagePath := strings.TrimSpace(stringValueFromMap(payload, "installed_path"))
	if packagePath == "" {
		packagePath = filepath.Join(c.distRoot, runtimeName, runtimeVersion)
	}
	return &PackagePublishResult{
		RuntimeName: runtimeName,
		Version:     runtimeVersion,
		PackagePath: packagePath,
	}, nil
}

func (c *l2AdminClient) request(ctx context.Context, payload map[string]any) (map[string]any, error) {
	traceID := uuid.NewString()
	responseCh, cancelTrace, err := c.receiver.SubscribeTrace(traceID)
	if err != nil {
		return nil, err
	}
	defer cancelTrace()

	raw, err := fluxbeesdk.MarshalPayload(payload)
	if err != nil {
		return nil, err
	}
	msgCopy := adminCommandMsg
	action := publishRuntimePackageAction
	request := fluxbeesdk.Message{
		Routing: fluxbeesdk.Routing{
			Src:     c.sender.UUID(),
			Dst:     fluxbeesdk.UnicastDestination(c.targetNode),
			TTL:     16,
			TraceID: traceID,
		},
		Meta: fluxbeesdk.Meta{
			MsgType: "admin",
			Msg:     &msgCopy,
			Action:  &action,
		},
		Payload: raw,
	}
	if err := c.sender.Send(request); err != nil {
		return nil, err
	}

	var msg fluxbeesdk.Message
	select {
	case <-ctx.Done():
		return nil, ctx.Err()
	case item := <-responseCh:
		if item.err != nil {
			return nil, item.err
		}
		msg = item.msg
	}
	if msg.Meta.MsgType != "admin" || stringValue(msg.Meta.Msg) != adminCommandResponseMsg {
		return nil, fmt.Errorf("unexpected admin response %q", stringValue(msg.Meta.Msg))
	}
	var decoded map[string]any
	if err := json.Unmarshal(msg.Payload, &decoded); err != nil {
		return nil, err
	}
	status := strings.TrimSpace(stringValueFromMap(decoded, "status"))
	if status == "" {
		status = "error"
	}
	if status != "ok" {
		return nil, &adminActionError{
			Status:  status,
			Code:    stringValueFromMap(decoded, "error_code"),
			Message: adminErrorDetailText(decoded["error_detail"]),
			Payload: decoded,
		}
	}
	return decoded, nil
}

func adminErrorDetailText(value any) string {
	switch detail := value.(type) {
	case nil:
		return ""
	case string:
		return strings.TrimSpace(detail)
	case map[string]any:
		if msg := strings.TrimSpace(stringValueFromMap(detail, "message")); msg != "" {
			return msg
		}
		if raw, err := json.Marshal(detail); err == nil {
			return string(raw)
		}
	case []any:
		if raw, err := json.Marshal(detail); err == nil {
			return string(raw)
		}
	default:
		if raw, err := json.Marshal(detail); err == nil {
			return string(raw)
		}
	}
	return ""
}
