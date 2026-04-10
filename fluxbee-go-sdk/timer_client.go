package sdk

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"time"

	"github.com/google/uuid"
)

const (
	MsgTimerFired = "TIMER_FIRED"
)

type TimerID string

type MissedPolicy string

const (
	MissedPolicyFire         MissedPolicy = "fire"
	MissedPolicyDrop         MissedPolicy = "drop"
	MissedPolicyFireIfWithin MissedPolicy = "fire_if_within"
)

type ListFilter struct {
	OwnerL2Name  string `json:"owner_l2_name,omitempty"`
	StatusFilter string `json:"status_filter,omitempty"`
	Limit        int    `json:"limit,omitempty"`
}

type TimerInfo struct {
	UUID             string          `json:"uuid"`
	OwnerL2Name      string          `json:"owner_l2_name"`
	TargetL2Name     string          `json:"target_l2_name"`
	ClientRef        *string         `json:"client_ref,omitempty"`
	Kind             string          `json:"kind"`
	FireAtUTCMS      int64           `json:"fire_at_utc_ms"`
	CronSpec         *string         `json:"cron_spec,omitempty"`
	CronTZ           *string         `json:"cron_tz,omitempty"`
	MissedPolicy     MissedPolicy    `json:"missed_policy"`
	MissedWithinMS   *int64          `json:"missed_within_ms,omitempty"`
	Status           string          `json:"status"`
	CreatedAtUTCMS   int64           `json:"created_at_utc_ms"`
	FireCount        int64           `json:"fire_count"`
	LastFiredAtUTCMS *int64          `json:"last_fired_at_utc_ms,omitempty"`
	Payload          json.RawMessage `json:"payload,omitempty"`
	Metadata         json.RawMessage `json:"metadata,omitempty"`
}

type FiredEvent struct {
	TimerUUID            string          `json:"timer_uuid"`
	OwnerL2Name          string          `json:"owner_l2_name"`
	ClientRef            *string         `json:"client_ref,omitempty"`
	Kind                 string          `json:"kind"`
	ScheduledFireAtUTCMS int64           `json:"scheduled_fire_at_utc_ms"`
	ActualFireAtUTCMS    int64           `json:"actual_fire_at_utc_ms"`
	FireCount            int64           `json:"fire_count"`
	IsLastFire           bool            `json:"is_last_fire"`
	UserPayload          json.RawMessage `json:"user_payload,omitempty"`
	Metadata             json.RawMessage `json:"metadata,omitempty"`
}

type TimerClientConfig struct {
	TimerNode         string
	TimeRetrySchedule []time.Duration
}

type TimerClient struct {
	sender            *NodeSender
	receiver          *NodeReceiver
	timerNode         string
	timeRetrySchedule []time.Duration
}

type TimerNowResult struct {
	NowUTCMS  int64  `json:"now_utc_ms"`
	NowUTCISO string `json:"now_utc_iso"`
}

type TimerNowInResult struct {
	NowUTCMS        int64  `json:"now_utc_ms"`
	TZ              string `json:"tz"`
	NowLocalISO     string `json:"now_local_iso"`
	TZOffsetSeconds int    `json:"tz_offset_seconds"`
	TZAbbrev        string `json:"tz_abbrev"`
}

func NewTimerClient(sender *NodeSender, receiver *NodeReceiver, cfg TimerClientConfig) (*TimerClient, error) {
	if sender == nil {
		return nil, fmt.Errorf("sender must be non-nil")
	}
	if receiver == nil {
		return nil, fmt.Errorf("receiver must be non-nil")
	}
	timerNode := strings.TrimSpace(cfg.TimerNode)
	if timerNode == "" {
		var err error
		timerNode, err = localTimerNodeName(sender.FullName())
		if err != nil {
			return nil, err
		}
	}
	retries := cfg.TimeRetrySchedule
	if len(retries) == 0 {
		retries = []time.Duration{100 * time.Millisecond, 300 * time.Millisecond, time.Second}
	}
	return &TimerClient{
		sender:            sender,
		receiver:          receiver,
		timerNode:         timerNode,
		timeRetrySchedule: retries,
	}, nil
}

func (c *TimerClient) Now(ctx context.Context) (*TimerNowResult, error) {
	var out TimerNowResult
	if err := c.callTimeOperation(ctx, "TIMER_NOW", map[string]any{}, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

func (c *TimerClient) NowIn(ctx context.Context, tz string) (*TimerNowInResult, error) {
	var out TimerNowInResult
	if err := c.callTimeOperation(ctx, "TIMER_NOW_IN", map[string]any{"tz": tz}, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

func (c *TimerClient) Help(ctx context.Context) (*HelpDescriptor, error) {
	msg, err := c.buildRequest("TIMER_HELP", map[string]any{})
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
	var payload HelpDescriptor
	if err := ParseSystemResponse(&resp, MsgTimerResponse, &payload); err != nil {
		return nil, err
	}
	return &payload, nil
}

func ParseFiredEvent(msg Message) (*FiredEvent, error) {
	if msg.Meta.MsgType != SYSTEMKind || stringValue(msg.Meta.Msg) != MsgTimerFired {
		return nil, fmt.Errorf("message is not TIMER_FIRED")
	}
	var event FiredEvent
	if err := json.Unmarshal(msg.Payload, &event); err != nil {
		return nil, err
	}
	return &event, nil
}

func (c *TimerClient) callTimeOperation(ctx context.Context, verb string, payload any, out any) error {
	var lastErr error
	attempts := len(c.timeRetrySchedule)
	if attempts == 0 {
		attempts = 1
	}
	for attempt := 0; attempt < attempts; attempt++ {
		msg, err := c.buildRequest(verb, payload)
		if err != nil {
			return err
		}
		attemptCtx, cancel := context.WithTimeout(ctx, c.timeRetrySchedule[attempt])
		resp, err := RequestSystemRPC(attemptCtx, c.sender, c.receiver, msg, MsgTimerResponse)
		cancel()
		if err == nil {
			timerResp, parseErr := ParseTimerResponse(&resp)
			if parseErr != nil {
				return parseErr
			}
			if respErr := timerResp.ResponseError(); respErr != nil {
				return respErr
			}
			return ParseSystemResponse(&resp, MsgTimerResponse, out)
		}
		lastErr = err
		if ctx.Err() != nil {
			return ctx.Err()
		}
	}
	return &SystemResponseError{
		ResponseMsg: MsgTimerResponse,
		Verb:        verb,
		Code:        "TIMER_UNREACHABLE",
		Message:     fmt.Sprintf("SY.timer did not respond after %d attempts: %v", attempts, lastErr),
	}
}

func (c *TimerClient) buildRequest(verb string, payload any) (Message, error) {
	return BuildSystemRequest(
		c.sender.UUID(),
		c.timerNode,
		verb,
		payload,
		uuid.NewString(),
		SystemEnvelopeOptions{},
	)
}

func localTimerNodeName(fullName string) (string, error) {
	fullName = strings.TrimSpace(fullName)
	if fullName == "" {
		return "", fmt.Errorf("cannot derive local timer node name from empty sender full name")
	}
	_, hive, ok := strings.Cut(fullName, "@")
	if !ok || strings.TrimSpace(hive) == "" {
		return "", fmt.Errorf("sender full name must include hive suffix")
	}
	return "SY.timer@" + hive, nil
}
