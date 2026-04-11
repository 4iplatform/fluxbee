package sdk

import (
	"context"
	"encoding/json"
	"fmt"
	"time"
)

const MinTimerDuration = time.Minute

type ScheduleOptions struct {
	TargetL2Name   string
	ClientRef      string
	Payload        any
	MissedPolicy   MissedPolicy
	MissedWithinMS *int64
	Metadata       any
}

type RecurringOptions struct {
	CronSpec       string
	CronTZ         string
	TargetL2Name   string
	ClientRef      string
	Payload        any
	MissedPolicy   MissedPolicy
	MissedWithinMS *int64
	Metadata       any
}

type timerGetResponse struct {
	OK    bool      `json:"ok"`
	Verb  string    `json:"verb"`
	Timer TimerInfo `json:"timer"`
}

type timerListResponse struct {
	OK     bool        `json:"ok"`
	Verb   string      `json:"verb"`
	Count  int         `json:"count"`
	Timers []TimerInfo `json:"timers"`
}

func (c *TimerClient) Schedule(ctx context.Context, fireAt time.Time, opts ScheduleOptions) (TimerID, error) {
	if err := validateAbsoluteScheduleTime(fireAt); err != nil {
		return "", err
	}
	payload, err := schedulePayload(map[string]any{
		"fire_at_utc_ms": fireAt.UTC().UnixMilli(),
	}, opts)
	if err != nil {
		return "", err
	}
	return c.schedule(ctx, "TIMER_SCHEDULE", payload)
}

func (c *TimerClient) ScheduleIn(ctx context.Context, d time.Duration, opts ScheduleOptions) (TimerID, error) {
	if err := validateRelativeScheduleDuration(d); err != nil {
		return "", err
	}
	payload, err := schedulePayload(map[string]any{
		"fire_in_ms": d.Milliseconds(),
	}, opts)
	if err != nil {
		return "", err
	}
	return c.schedule(ctx, "TIMER_SCHEDULE", payload)
}

func (c *TimerClient) ScheduleRecurring(ctx context.Context, opts RecurringOptions) (TimerID, error) {
	if opts.CronSpec == "" {
		return "", fmt.Errorf("cron_spec must be non-empty")
	}
	if err := validateMissedPolicy(opts.MissedPolicy, opts.MissedWithinMS); err != nil {
		return "", err
	}
	payload := map[string]any{
		"cron_spec": opts.CronSpec,
	}
	if opts.CronTZ != "" {
		payload["cron_tz"] = opts.CronTZ
	}
	if opts.TargetL2Name != "" {
		payload["target_l2_name"] = opts.TargetL2Name
	}
	if opts.ClientRef != "" {
		payload["client_ref"] = opts.ClientRef
	}
	if opts.Payload != nil {
		payload["payload"] = opts.Payload
	}
	if opts.Metadata != nil {
		payload["metadata"] = opts.Metadata
	}
	if opts.MissedPolicy != "" {
		payload["missed_policy"] = string(opts.MissedPolicy)
	}
	if opts.MissedWithinMS != nil {
		payload["missed_within_ms"] = *opts.MissedWithinMS
	}
	return c.schedule(ctx, "TIMER_SCHEDULE_RECURRING", payload)
}

func (c *TimerClient) Cancel(ctx context.Context, id TimerID) error {
	msg, err := c.buildRequest("TIMER_CANCEL", map[string]any{"timer_uuid": string(id)})
	if err != nil {
		return err
	}
	resp, err := RequestSystemRPC(ctx, c.sender, c.receiver, msg, MsgTimerResponse)
	if err != nil {
		return err
	}
	timerResp, err := ParseTimerResponse(&resp)
	if err != nil {
		return err
	}
	if respErr := timerResp.ResponseError(); respErr != nil {
		return respErr
	}
	return nil
}

func (c *TimerClient) Reschedule(ctx context.Context, id TimerID, newFireAt time.Time) error {
	if err := validateAbsoluteScheduleTime(newFireAt); err != nil {
		return err
	}
	msg, err := c.buildRequest("TIMER_RESCHEDULE", map[string]any{
		"timer_uuid":         string(id),
		"new_fire_at_utc_ms": newFireAt.UTC().UnixMilli(),
	})
	if err != nil {
		return err
	}
	resp, err := RequestSystemRPC(ctx, c.sender, c.receiver, msg, MsgTimerResponse)
	if err != nil {
		return err
	}
	timerResp, err := ParseTimerResponse(&resp)
	if err != nil {
		return err
	}
	if respErr := timerResp.ResponseError(); respErr != nil {
		return respErr
	}
	return nil
}

func (c *TimerClient) Get(ctx context.Context, id TimerID) (*TimerInfo, error) {
	msg, err := c.buildRequest("TIMER_GET", map[string]any{"timer_uuid": string(id)})
	if err != nil {
		return nil, err
	}
	resp, err := RequestSystemRPC(ctx, c.sender, c.receiver, msg, MsgTimerResponse)
	if err != nil {
		return nil, err
	}
	timerResp, err := ParseTimerResponse(&resp)
	if err != nil {
		return nil, err
	}
	if respErr := timerResp.ResponseError(); respErr != nil {
		return nil, respErr
	}
	var payload timerGetResponse
	if err := ParseSystemResponse(&resp, MsgTimerResponse, &payload); err != nil {
		return nil, err
	}
	return &payload.Timer, nil
}

func (c *TimerClient) List(ctx context.Context, filter ListFilter) ([]TimerInfo, error) {
	payloadMap := map[string]any{}
	if filter.OwnerL2Name != "" {
		payloadMap["owner_l2_name"] = filter.OwnerL2Name
	}
	if filter.StatusFilter != "" {
		payloadMap["status_filter"] = filter.StatusFilter
	}
	if filter.Limit > 0 {
		payloadMap["limit"] = filter.Limit
	}
	msg, err := c.buildRequest("TIMER_LIST", payloadMap)
	if err != nil {
		return nil, err
	}
	resp, err := RequestSystemRPC(ctx, c.sender, c.receiver, msg, MsgTimerResponse)
	if err != nil {
		return nil, err
	}
	timerResp, err := ParseTimerResponse(&resp)
	if err != nil {
		return nil, err
	}
	if respErr := timerResp.ResponseError(); respErr != nil {
		return nil, respErr
	}
	var payload timerListResponse
	if err := ParseSystemResponse(&resp, MsgTimerResponse, &payload); err != nil {
		return nil, err
	}
	return payload.Timers, nil
}

func (c *TimerClient) ListMine(ctx context.Context, filter ListFilter) ([]TimerInfo, error) {
	if filter.OwnerL2Name == "" {
		filter.OwnerL2Name = c.sender.FullName()
	}
	return c.List(ctx, filter)
}

func validateAbsoluteScheduleTime(fireAt time.Time) error {
	if fireAt.IsZero() {
		return fmt.Errorf("fire_at must be non-zero")
	}
	if time.Until(fireAt) < MinTimerDuration {
		return &SystemResponseError{
			ResponseMsg: MsgTimerResponse,
			Verb:        "TIMER_SCHEDULE",
			Code:        "TIMER_BELOW_MINIMUM",
			Message:     fmt.Sprintf("minimum timer duration is %d seconds", int(MinTimerDuration.Seconds())),
		}
	}
	return nil
}

func validateRelativeScheduleDuration(d time.Duration) error {
	if d < MinTimerDuration {
		return &SystemResponseError{
			ResponseMsg: MsgTimerResponse,
			Verb:        "TIMER_SCHEDULE",
			Code:        "TIMER_BELOW_MINIMUM",
			Message:     fmt.Sprintf("minimum timer duration is %d seconds", int(MinTimerDuration.Seconds())),
		}
	}
	return nil
}

func validateMissedPolicy(policy MissedPolicy, withinMS *int64) error {
	if policy == "" {
		return nil
	}
	switch policy {
	case MissedPolicyFire, MissedPolicyDrop:
		return nil
	case MissedPolicyFireIfWithin:
		if withinMS == nil || *withinMS <= 0 {
			return fmt.Errorf("missed_within_ms must be set when missed_policy=fire_if_within")
		}
		return nil
	default:
		return fmt.Errorf("invalid missed_policy: %s", policy)
	}
}

func schedulePayload(base map[string]any, opts ScheduleOptions) (map[string]any, error) {
	if err := validateMissedPolicy(opts.MissedPolicy, opts.MissedWithinMS); err != nil {
		return nil, err
	}
	if opts.TargetL2Name != "" {
		base["target_l2_name"] = opts.TargetL2Name
	}
	if opts.ClientRef != "" {
		base["client_ref"] = opts.ClientRef
	}
	if opts.Payload != nil {
		base["payload"] = opts.Payload
	}
	if opts.Metadata != nil {
		base["metadata"] = opts.Metadata
	}
	if opts.MissedPolicy != "" {
		base["missed_policy"] = string(opts.MissedPolicy)
	}
	if opts.MissedWithinMS != nil {
		base["missed_within_ms"] = *opts.MissedWithinMS
	}
	return base, nil
}

func (c *TimerClient) schedule(ctx context.Context, verb string, payload map[string]any) (TimerID, error) {
	msg, err := c.buildRequest(verb, payload)
	if err != nil {
		return "", err
	}
	resp, err := RequestSystemRPC(ctx, c.sender, c.receiver, msg, MsgTimerResponse)
	if err != nil {
		return "", err
	}
	timerResp, err := ParseTimerResponse(&resp)
	if err != nil {
		return "", err
	}
	if respErr := timerResp.ResponseError(); respErr != nil {
		return "", respErr
	}
	if timerResp.TimerUUID == nil {
		return "", fmt.Errorf("timer response missing timer_uuid")
	}
	return TimerID(*timerResp.TimerUUID), nil
}

func (t *TimerInfo) UnmarshalJSON(data []byte) error {
	type alias TimerInfo
	var aux struct {
		alias
		Payload  json.RawMessage `json:"payload"`
		Metadata json.RawMessage `json:"metadata"`
	}
	if err := json.Unmarshal(data, &aux); err != nil {
		return err
	}
	*t = TimerInfo(aux.alias)
	t.Payload = aux.Payload
	t.Metadata = aux.Metadata
	return nil
}
