package sdk

import "context"

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
	var out TimerConvertResult
	if err := c.callTimeOperation(ctx, "TIMER_CONVERT", map[string]any{
		"instant_utc_ms": instantUTCMS,
		"to_tz":          toTZ,
	}, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

func (c *TimerClient) Parse(ctx context.Context, input, layout, tz string) (*TimerParseResult, error) {
	var out TimerParseResult
	if err := c.callTimeOperation(ctx, "TIMER_PARSE", map[string]any{
		"input":  input,
		"layout": layout,
		"tz":     tz,
	}, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

func (c *TimerClient) Format(ctx context.Context, instantUTCMS int64, layout, tz string) (*TimerFormatResult, error) {
	var out TimerFormatResult
	if err := c.callTimeOperation(ctx, "TIMER_FORMAT", map[string]any{
		"instant_utc_ms": instantUTCMS,
		"layout":         layout,
		"tz":             tz,
	}, &out); err != nil {
		return nil, err
	}
	return &out, nil
}
