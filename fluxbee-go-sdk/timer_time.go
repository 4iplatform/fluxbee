package sdk

import (
	"context"
	"fmt"
	"strings"
)

type TimerConvertResult struct {
	InstantUTCMS    int64  `json:"instant_utc_ms"`
	ToTZ            string `json:"to_tz"`
	LocalISO        string `json:"local_iso"`
	TZOffsetSeconds int    `json:"tz_offset_seconds"`
}

type TimerParseResult struct {
	InstantUTCMS   int64  `json:"instant_utc_ms"`
	ResolvedISOUTC string `json:"resolved_iso_utc"`
}

type TimerFormatResult struct {
	Formatted string `json:"formatted"`
}

func (c *TimerClient) Convert(ctx context.Context, instantUTCMS int64, toTZ string) (*TimerConvertResult, error) {
	if strings.TrimSpace(toTZ) == "" {
		return nil, fmt.Errorf("to_tz must be non-empty")
	}
	var out TimerConvertResult
	if err := c.callTimeOperation(ctx, "TIMER_CONVERT", map[string]any{
		"instant_utc_ms": instantUTCMS,
		"to_tz":          strings.TrimSpace(toTZ),
	}, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

func (c *TimerClient) Parse(ctx context.Context, input, layout, tz string) (*TimerParseResult, error) {
	if strings.TrimSpace(input) == "" {
		return nil, fmt.Errorf("input must be non-empty")
	}
	if strings.TrimSpace(layout) == "" {
		return nil, fmt.Errorf("layout must be non-empty")
	}
	if strings.TrimSpace(tz) == "" {
		return nil, fmt.Errorf("tz must be non-empty")
	}
	var out TimerParseResult
	if err := c.callTimeOperation(ctx, "TIMER_PARSE", map[string]any{
		"input":  strings.TrimSpace(input),
		"layout": strings.TrimSpace(layout),
		"tz":     strings.TrimSpace(tz),
	}, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

func (c *TimerClient) Format(ctx context.Context, instantUTCMS int64, layout, tz string) (*TimerFormatResult, error) {
	if strings.TrimSpace(layout) == "" {
		return nil, fmt.Errorf("layout must be non-empty")
	}
	if strings.TrimSpace(tz) == "" {
		return nil, fmt.Errorf("tz must be non-empty")
	}
	var out TimerFormatResult
	if err := c.callTimeOperation(ctx, "TIMER_FORMAT", map[string]any{
		"instant_utc_ms": instantUTCMS,
		"layout":         strings.TrimSpace(layout),
		"tz":             strings.TrimSpace(tz),
	}, &out); err != nil {
		return nil, err
	}
	return &out, nil
}
