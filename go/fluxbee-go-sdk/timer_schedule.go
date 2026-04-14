package sdk

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"time"
)

const MinTimerDuration = time.Minute
const MaxTimerListLimit = 1000

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
	if err := validateRecurringOptions(opts); err != nil {
		return "", err
	}
	payload := map[string]any{
		"cron_spec": strings.TrimSpace(opts.CronSpec),
	}
	if cronTZ := strings.TrimSpace(opts.CronTZ); cronTZ != "" {
		payload["cron_tz"] = cronTZ
	}
	if target := strings.TrimSpace(opts.TargetL2Name); target != "" {
		payload["target_l2_name"] = target
	}
	if clientRef := strings.TrimSpace(opts.ClientRef); clientRef != "" {
		payload["client_ref"] = clientRef
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
	if err := validateTimerID(id); err != nil {
		return err
	}
	msg, err := c.buildRequest("TIMER_CANCEL", map[string]any{"timer_uuid": string(id)})
	if err != nil {
		return err
	}
	return c.cancelWithMessage(ctx, msg)
}

func (c *TimerClient) CancelByClientRef(ctx context.Context, clientRef string) error {
	clientRef, err := validateClientRef(clientRef)
	if err != nil {
		return err
	}
	msg, err := c.buildRequest("TIMER_CANCEL", map[string]any{"client_ref": clientRef})
	if err != nil {
		return err
	}
	return c.cancelWithMessage(ctx, msg)
}

func (c *TimerClient) cancelWithMessage(ctx context.Context, msg Message) error {
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
	if err := validateTimerID(id); err != nil {
		return err
	}
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
	return c.rescheduleWithMessage(ctx, msg)
}

func (c *TimerClient) RescheduleByClientRef(ctx context.Context, clientRef string, newFireAt time.Time) error {
	clientRef, err := validateClientRef(clientRef)
	if err != nil {
		return err
	}
	if err := validateAbsoluteScheduleTime(newFireAt); err != nil {
		return err
	}
	msg, err := c.buildRequest("TIMER_RESCHEDULE", map[string]any{
		"client_ref":         clientRef,
		"new_fire_at_utc_ms": newFireAt.UTC().UnixMilli(),
	})
	if err != nil {
		return err
	}
	return c.rescheduleWithMessage(ctx, msg)
}

func (c *TimerClient) rescheduleWithMessage(ctx context.Context, msg Message) error {
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
	if err := validateTimerID(id); err != nil {
		return nil, err
	}
	msg, err := c.buildRequest("TIMER_GET", map[string]any{"timer_uuid": string(id)})
	if err != nil {
		return nil, err
	}
	return c.getWithMessage(ctx, msg)
}

func (c *TimerClient) GetByClientRef(ctx context.Context, clientRef string) (*TimerInfo, error) {
	clientRef, err := validateClientRef(clientRef)
	if err != nil {
		return nil, err
	}
	msg, err := c.buildRequest("TIMER_GET", map[string]any{"client_ref": clientRef})
	if err != nil {
		return nil, err
	}
	return c.getWithMessage(ctx, msg)
}

func (c *TimerClient) getWithMessage(ctx context.Context, msg Message) (*TimerInfo, error) {
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
	if err := validateListFilter(filter); err != nil {
		return nil, err
	}
	payloadMap := map[string]any{}
	if owner := strings.TrimSpace(filter.OwnerL2Name); owner != "" {
		payloadMap["owner_l2_name"] = owner
	}
	if status := strings.TrimSpace(filter.StatusFilter); status != "" {
		payloadMap["status_filter"] = status
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
		if withinMS != nil {
			return fmt.Errorf("missed_within_ms requires missed_policy=fire_if_within")
		}
		return nil
	}
	switch policy {
	case MissedPolicyFire, MissedPolicyDrop:
		if withinMS != nil {
			return fmt.Errorf("missed_within_ms is only valid when missed_policy=fire_if_within")
		}
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
	if target := strings.TrimSpace(opts.TargetL2Name); target != "" {
		base["target_l2_name"] = target
	}
	if clientRef := strings.TrimSpace(opts.ClientRef); clientRef != "" {
		if _, err := validateClientRef(clientRef); err != nil {
			return nil, err
		}
		base["client_ref"] = clientRef
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

func validateRecurringOptions(opts RecurringOptions) error {
	cronSpec := strings.TrimSpace(opts.CronSpec)
	if cronSpec == "" {
		return fmt.Errorf("cron_spec must be non-empty")
	}
	if len(strings.Fields(cronSpec)) != 5 {
		return fmt.Errorf("cron_spec must use standard 5-field cron syntax")
	}
	if clientRef := strings.TrimSpace(opts.ClientRef); clientRef != "" {
		if _, err := validateClientRef(clientRef); err != nil {
			return err
		}
	}
	return validateMissedPolicy(opts.MissedPolicy, opts.MissedWithinMS)
}

func validateTimerID(id TimerID) error {
	if strings.TrimSpace(string(id)) == "" {
		return fmt.Errorf("timer_id must be non-empty")
	}
	return nil
}

func validateClientRef(clientRef string) (string, error) {
	clientRef = strings.TrimSpace(clientRef)
	if clientRef == "" {
		return "", fmt.Errorf("client_ref must be non-empty")
	}
	if len(clientRef) > 255 {
		return "", fmt.Errorf("client_ref must be at most 255 characters")
	}
	return clientRef, nil
}

func validateListFilter(filter ListFilter) error {
	if filter.Limit < 0 {
		return fmt.Errorf("limit must be >= 0")
	}
	if filter.Limit > MaxTimerListLimit {
		return fmt.Errorf("limit must be <= %d", MaxTimerListLimit)
	}
	switch strings.TrimSpace(filter.StatusFilter) {
	case "", "pending", "fired", "canceled", "all":
		return nil
	default:
		return fmt.Errorf("invalid status_filter: %s", filter.StatusFilter)
	}
}
