package main

import (
	"database/sql"
	"errors"
	"fmt"
)

// timerError is an error and the TIMER_* code its answer carries (FINDINGS A-220). The code goes
// with the error, not in its text: a refusal quotes what the requester sent (a cron_spec, a zone),
// and what it quotes must not change its code.
type timerError struct {
	code string
	err  error
}

func (e *timerError) Error() string { return e.err.Error() }

func (e *timerError) Unwrap() error { return e.err }

// timerErrorf is an error that carries code, its text written as fmt.Errorf writes it (%w wraps a
// cause).
func timerErrorf(code, format string, args ...any) error {
	return &timerError{code: code, err: fmt.Errorf(format, args...)}
}

// errorCode is the code err carries, through any wrapping, or fallback when it carries none.
func errorCode(err error, fallback string) string {
	var coded *timerError
	if errors.As(err, &coded) {
		return coded.code
	}
	return fallback
}

// timerErrorCode is the code of a schedule or a reschedule SY.timer refuses: the one its error
// carries, TIMER_INVALID_PAYLOAD when it carries none, TIMER_INTERNAL for no error.
func timerErrorCode(err error) string {
	if err == nil {
		return "TIMER_INTERNAL"
	}
	return errorCode(err, "TIMER_INVALID_PAYLOAD")
}

// timerStorageOrInternalCode is the code of a cancel or a reschedule whose timer cannot be resolved:
// TIMER_NOT_FOUND for no timer, the code its error carries (TIMER_NOT_OWNER, TIMER_INTERNAL), and
// TIMER_STORAGE_ERROR for any other, the database's.
func timerStorageOrInternalCode(err error) string {
	if errors.Is(err, sql.ErrNoRows) {
		return "TIMER_NOT_FOUND"
	}
	return errorCode(err, "TIMER_STORAGE_ERROR")
}
