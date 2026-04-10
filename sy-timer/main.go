//go:build linux

package main

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"strings"
	"time"
	_ "time/tzdata"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
	"github.com/google/uuid"
	"github.com/robfig/cron/v3"
	_ "modernc.org/sqlite"
)

const (
	version             = "1.0"
	defaultNodeBaseName = "SY.timer"
	configDir           = "/etc/fluxbee"
	routerSockDir       = "/var/run/fluxbee/routers"
)

var uuidPersistenceDir = "/var/lib/fluxbee/state/nodes"

type messageSender interface {
	Send(fluxbeesdk.Message) error
	UUID() string
	FullName() string
}

type messageReceiver interface {
	Recv(context.Context) (fluxbeesdk.Message, error)
}

type Service struct {
	sender             messageSender
	receiver           messageReceiver
	nodeName           string
	instanceDir        string
	dbPath             string
	db                 *sql.DB
	uuidPersistenceDir string
	scheduler          *timerScheduler
}

type timerNowInRequest struct {
	TZ string `json:"tz"`
}

type timerConvertRequest struct {
	InstantUTCMS int64  `json:"instant_utc_ms"`
	ToTZ         string `json:"to_tz"`
}

type timerParseRequest struct {
	Input  string `json:"input"`
	Layout string `json:"layout"`
	TZ     string `json:"tz"`
}

type timerFormatRequest struct {
	InstantUTCMS int64  `json:"instant_utc_ms"`
	Layout       string `json:"layout"`
	TZ           string `json:"tz"`
}

type timerScheduleRequest struct {
	FireAtUTCMS    *int64          `json:"fire_at_utc_ms,omitempty"`
	FireInMS       *int64          `json:"fire_in_ms,omitempty"`
	TargetL2Name   *string         `json:"target_l2_name,omitempty"`
	ClientRef      *string         `json:"client_ref,omitempty"`
	Payload        json.RawMessage `json:"payload,omitempty"`
	MissedPolicy   *string         `json:"missed_policy,omitempty"`
	MissedWithinMS *int64          `json:"missed_within_ms,omitempty"`
	Metadata       json.RawMessage `json:"metadata,omitempty"`
}

type timerScheduleRecurringRequest struct {
	CronSpec       string          `json:"cron_spec"`
	CronTZ         string          `json:"cron_tz,omitempty"`
	TargetL2Name   *string         `json:"target_l2_name,omitempty"`
	ClientRef      *string         `json:"client_ref,omitempty"`
	Payload        json.RawMessage `json:"payload,omitempty"`
	MissedPolicy   *string         `json:"missed_policy,omitempty"`
	MissedWithinMS *int64          `json:"missed_within_ms,omitempty"`
	Metadata       json.RawMessage `json:"metadata,omitempty"`
}

type timerCancelRequest struct {
	TimerUUID string `json:"timer_uuid"`
}

type timerRescheduleRequest struct {
	TimerUUID      string `json:"timer_uuid"`
	NewFireAtUTCMS *int64 `json:"new_fire_at_utc_ms,omitempty"`
	NewFireInMS    *int64 `json:"new_fire_in_ms,omitempty"`
}

type timerListRequest struct {
	StatusFilter string `json:"status_filter,omitempty"`
	Limit        int    `json:"limit,omitempty"`
}

type timerPurgeOwnerRequest struct {
	OwnerL2Name string `json:"owner_l2_name"`
}

func main() {
	log.SetFlags(log.LstdFlags | log.Lmicroseconds)

	sender, receiver, err := fluxbeesdk.Connect(fluxbeesdk.NodeConfig{
		Name:               defaultNodeBaseName,
		RouterSocket:       routerSockDir,
		UUIDPersistenceDir: uuidPersistenceDir,
		UUIDMode:           fluxbeesdk.NodeUuidPersistent,
		ConfigDir:          configDir,
		Version:            version,
	})
	if err != nil {
		log.Fatalf("failed to connect node lifecycle: %v", err)
	}

	nodeName := sender.FullName()
	instanceDir, err := managedNodeInstanceDir(nodeName)
	if err != nil {
		log.Fatalf("failed to resolve managed node instance dir: %v", err)
	}
	if err := os.MkdirAll(instanceDir, 0o755); err != nil {
		log.Fatalf("failed to create instance dir: %v", err)
	}

	dbPath := filepath.Join(instanceDir, "timers.db")
	db, err := openTimerDB(dbPath)
	if err != nil {
		log.Fatalf("failed to open timers db: %v", err)
	}
	defer db.Close()

	service := &Service{
		sender:             sender,
		receiver:           receiver,
		nodeName:           nodeName,
		instanceDir:        instanceDir,
		dbPath:             dbPath,
		db:                 db,
		uuidPersistenceDir: uuidPersistenceDir,
	}
	service.scheduler = newTimerScheduler(service)
	deleted, err := gcHistoricalTimers(context.Background(), db, time.Now().UTC(), defaultHistoryRetention)
	if err != nil {
		log.Printf("historical timer gc failed: %v", err)
	}
	if err := service.scheduler.replayPending(context.Background(), time.Now().UTC()); err != nil {
		log.Fatalf("failed to replay pending timers: %v", err)
	}
	pendingCount, err := countPendingTimers(context.Background(), db)
	if err != nil {
		log.Printf("failed to count pending timers: %v", err)
	}
	log.Printf("SY.timer ready: node=%s db=%s pending=%d gc_deleted=%d", service.nodeName, service.dbPath, pendingCount, deleted)
	service.scheduler.start()
	service.run()
}

func (s *Service) run() {
	for {
		msg, err := s.receiver.Recv(context.Background())
		if err != nil {
			log.Printf("receiver error: %v", err)
			time.Sleep(100 * time.Millisecond)
			continue
		}
		if err := s.handleMessage(msg); err != nil {
			log.Printf("handle message error: %v", err)
		}
	}
}

func (s *Service) handleMessage(msg fluxbeesdk.Message) error {
	if msg.Meta.MsgType != fluxbeesdk.SYSTEMKind {
		return nil
	}
	switch stringValue(msg.Meta.Msg) {
	case fluxbeesdk.MSGNodeStatusGet:
		response, err := fluxbeesdk.BuildDefaultNodeStatusResponse(&msg, s.sender.UUID(), "HEALTHY")
		if err != nil {
			return err
		}
		return s.sender.Send(response)
	case "TIMER_NOW":
		return s.respondTimerNow(msg)
	case "TIMER_NOW_IN":
		return s.respondTimerNowIn(msg)
	case "TIMER_CONVERT":
		return s.respondTimerConvert(msg)
	case "TIMER_PARSE":
		return s.respondTimerParse(msg)
	case "TIMER_FORMAT":
		return s.respondTimerFormat(msg)
	case "TIMER_SCHEDULE":
		return s.respondTimerSchedule(msg)
	case "TIMER_SCHEDULE_RECURRING":
		return s.respondTimerScheduleRecurring(msg)
	case "TIMER_GET":
		return s.respondTimerGet(msg)
	case "TIMER_LIST":
		return s.respondTimerList(msg)
	case "TIMER_CANCEL":
		return s.respondTimerCancel(msg)
	case "TIMER_RESCHEDULE":
		return s.respondTimerReschedule(msg)
	case "TIMER_PURGE_OWNER":
		return s.respondTimerPurgeOwner(msg)
	case fluxbeesdk.MsgTimerHelp:
		return s.respondTimerHelp(msg)
	default:
		return s.sendTimerError(msg, stringValue(msg.Meta.Msg), "TIMER_INTERNAL", fmt.Sprintf("verb not implemented in current build: %s", stringValue(msg.Meta.Msg)))
	}
}

func (s *Service) respondTimerNow(incoming fluxbeesdk.Message) error {
	now := time.Now().UTC()
	return s.sendTimerResponse(incoming, map[string]any{
		"ok":          true,
		"verb":        "TIMER_NOW",
		"now_utc_ms":  now.UnixMilli(),
		"now_utc_iso": formatTimeMillisUTC(now),
	})
}

func (s *Service) respondTimerNowIn(incoming fluxbeesdk.Message) error {
	var req timerNowInRequest
	if err := json.Unmarshal(incoming.Payload, &req); err != nil {
		return s.sendTimerError(incoming, "TIMER_NOW_IN", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	loc, err := loadLocation(req.TZ)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_NOW_IN", "TIMER_INVALID_TIMEZONE", err.Error())
	}
	nowUTC := time.Now().UTC()
	local := nowUTC.In(loc)
	abbr, offset := local.Zone()
	return s.sendTimerResponse(incoming, map[string]any{
		"ok":                true,
		"verb":              "TIMER_NOW_IN",
		"now_utc_ms":        nowUTC.UnixMilli(),
		"tz":                req.TZ,
		"now_local_iso":     formatTimeMillisLocal(local),
		"tz_offset_seconds": offset,
		"tz_abbrev":         abbr,
	})
}

func (s *Service) respondTimerConvert(incoming fluxbeesdk.Message) error {
	var req timerConvertRequest
	if err := json.Unmarshal(incoming.Payload, &req); err != nil {
		return s.sendTimerError(incoming, "TIMER_CONVERT", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	loc, err := loadLocation(req.ToTZ)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_CONVERT", "TIMER_INVALID_TIMEZONE", err.Error())
	}
	instant := time.UnixMilli(req.InstantUTCMS).UTC().In(loc)
	_, offset := instant.Zone()
	return s.sendTimerResponse(incoming, map[string]any{
		"ok":                true,
		"verb":              "TIMER_CONVERT",
		"instant_utc_ms":    req.InstantUTCMS,
		"to_tz":             req.ToTZ,
		"local_iso":         formatTimeMillisLocal(instant),
		"tz_offset_seconds": offset,
	})
}

func (s *Service) respondTimerParse(incoming fluxbeesdk.Message) error {
	var req timerParseRequest
	if err := json.Unmarshal(incoming.Payload, &req); err != nil {
		return s.sendTimerError(incoming, "TIMER_PARSE", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	if strings.TrimSpace(req.Input) == "" || strings.TrimSpace(req.Layout) == "" || strings.TrimSpace(req.TZ) == "" {
		return s.sendTimerError(incoming, "TIMER_PARSE", "TIMER_INVALID_PAYLOAD", "input, layout, and tz must be non-empty")
	}
	loc, err := loadLocation(req.TZ)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_PARSE", "TIMER_INVALID_TIMEZONE", err.Error())
	}
	parsed, err := time.ParseInLocation(req.Layout, req.Input, loc)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_PARSE", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	utc := parsed.UTC()
	return s.sendTimerResponse(incoming, map[string]any{
		"ok":               true,
		"verb":             "TIMER_PARSE",
		"instant_utc_ms":   utc.UnixMilli(),
		"resolved_iso_utc": formatTimeMillisUTC(utc),
	})
}

func (s *Service) respondTimerFormat(incoming fluxbeesdk.Message) error {
	var req timerFormatRequest
	if err := json.Unmarshal(incoming.Payload, &req); err != nil {
		return s.sendTimerError(incoming, "TIMER_FORMAT", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	if strings.TrimSpace(req.Layout) == "" || strings.TrimSpace(req.TZ) == "" {
		return s.sendTimerError(incoming, "TIMER_FORMAT", "TIMER_INVALID_PAYLOAD", "layout and tz must be non-empty")
	}
	loc, err := loadLocation(req.TZ)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_FORMAT", "TIMER_INVALID_TIMEZONE", err.Error())
	}
	formatted := time.UnixMilli(req.InstantUTCMS).UTC().In(loc).Format(req.Layout)
	return s.sendTimerResponse(incoming, map[string]any{
		"ok":        true,
		"verb":      "TIMER_FORMAT",
		"formatted": formatted,
	})
}

func (s *Service) respondTimerHelp(incoming fluxbeesdk.Message) error {
	payload := fluxbeesdk.BuildHelpDescriptor(
		fluxbeesdk.MsgTimerHelp,
		"SY",
		"SY.timer",
		version,
		"System node providing persistent timers and authoritative time/timezone services.",
		[]fluxbeesdk.HelpOperationDescriptor{
			{Verb: "TIMER_SCHEDULE", Description: "Create a one-shot timer.", RequiredFields: []string{"fire_at_utc_ms OR fire_in_ms"}, OptionalFields: []string{"target_l2_name", "client_ref", "payload", "missed_policy", "missed_within_ms", "metadata"}},
			{Verb: "TIMER_SCHEDULE_RECURRING", Description: "Create a recurring timer from a 5-field cron expression.", RequiredFields: []string{"cron_spec"}, OptionalFields: []string{"cron_tz", "target_l2_name", "client_ref", "payload", "missed_policy", "missed_within_ms", "metadata"}},
			{Verb: "TIMER_GET", Description: "Read a timer owned by the requester.", RequiredFields: []string{"timer_uuid"}},
			{Verb: "TIMER_LIST", Description: "List timers owned by the requester.", OptionalFields: []string{"status_filter", "limit"}},
			{Verb: "TIMER_CANCEL", Description: "Cancel a pending owned timer.", RequiredFields: []string{"timer_uuid"}},
			{Verb: "TIMER_RESCHEDULE", Description: "Reschedule a pending one-shot timer owned by the requester.", RequiredFields: []string{"timer_uuid", "new_fire_at_utc_ms OR new_fire_in_ms"}},
			{Verb: "TIMER_PURGE_OWNER", Description: "Delete every timer owned by a node. Reserved to SY.orchestrator.", RequiredFields: []string{"owner_l2_name"}},
			{Verb: "TIMER_NOW", Description: "Return current UTC time."},
			{Verb: "TIMER_NOW_IN", Description: "Return current time in a requested IANA timezone.", RequiredFields: []string{"tz"}},
			{Verb: "TIMER_CONVERT", Description: "Convert a UTC instant into a target timezone.", RequiredFields: []string{"instant_utc_ms", "to_tz"}},
			{Verb: "TIMER_PARSE", Description: "Parse a date/time string using a Go layout and timezone.", RequiredFields: []string{"input", "layout", "tz"}},
			{Verb: "TIMER_FORMAT", Description: "Format a UTC instant using a Go layout and timezone.", RequiredFields: []string{"instant_utc_ms", "layout", "tz"}},
			{Verb: "TIMER_HELP", Description: "Describe the currently implemented SY.timer capabilities."},
		},
		[]fluxbeesdk.HelpErrorDescriptor{
			{Code: "TIMER_BELOW_MINIMUM", Description: "Requested timer duration is below the 60 second minimum."},
			{Code: "TIMER_INVALID_PAYLOAD", Description: "Request payload is malformed or missing required fields."},
			{Code: "TIMER_INVALID_CRON", Description: "Recurring cron expression is invalid."},
			{Code: "TIMER_INVALID_TIMEZONE", Description: "Requested timezone is not a valid IANA timezone."},
			{Code: "TIMER_FORBIDDEN", Description: "Requester is not authorized for the requested operation."},
			{Code: "TIMER_NOT_FOUND", Description: "Requested timer does not exist."},
			{Code: "TIMER_NOT_OWNER", Description: "Requester does not own the timer."},
			{Code: "TIMER_ALREADY_FIRED", Description: "Timer already fired and can no longer be modified."},
			{Code: "TIMER_RECURRING_NOT_RESCHEDULABLE", Description: "Recurring timers cannot be rescheduled in v1."},
			{Code: "TIMER_INTERNAL", Description: "Verb is not implemented yet or an internal error occurred."},
		},
	)
	return s.sendTimerResponse(incoming, payload)
}

func (s *Service) respondTimerSchedule(incoming fluxbeesdk.Message) error {
	var req timerScheduleRequest
	if err := json.Unmarshal(incoming.Payload, &req); err != nil {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	ownerL2, err := s.resolveSourceL2Name(incoming.Routing.Src)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE", "TIMER_INTERNAL", err.Error())
	}
	fireAtUTCMS, err := scheduleFireAt(req)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE", timerErrorCode(err), err.Error())
	}
	targetL2 := ownerL2
	if req.TargetL2Name != nil && strings.TrimSpace(*req.TargetL2Name) != "" {
		targetL2 = strings.TrimSpace(*req.TargetL2Name)
	}
	if !isCanonicalL2Name(targetL2) {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE", "TIMER_INVALID_PAYLOAD", "target_l2_name must be a canonical L2 name")
	}
	missedPolicy := "fire"
	if req.MissedPolicy != nil && strings.TrimSpace(*req.MissedPolicy) != "" {
		missedPolicy = strings.TrimSpace(*req.MissedPolicy)
	}
	if err := validateMissedPolicyRaw(missedPolicy, req.MissedWithinMS); err != nil {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	payload := req.Payload
	if len(payload) == 0 {
		payload = json.RawMessage(`{}`)
	}
	metadata := req.Metadata
	if len(metadata) > 0 && !json.Valid(metadata) {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE", "TIMER_INVALID_PAYLOAD", "metadata must be valid JSON")
	}
	timerID := uuid.NewString()
	nowUTCMS := time.Now().UTC().UnixMilli()
	row := timerRow{
		UUID:         timerID,
		OwnerL2Name:  ownerL2,
		TargetL2Name: targetL2,
		Kind:         "oneshot",
		FireAtUTC:    fireAtUTCMS,
		MissedPolicy: missedPolicy,
		Payload:      string(payload),
		Status:       "pending",
		CreatedAtUTC: nowUTCMS,
		FireCount:    0,
	}
	if req.ClientRef != nil && strings.TrimSpace(*req.ClientRef) != "" {
		row.ClientRef = sql.NullString{String: strings.TrimSpace(*req.ClientRef), Valid: true}
	}
	if req.MissedWithinMS != nil {
		row.MissedWithinMS = sql.NullInt64{Int64: *req.MissedWithinMS, Valid: true}
	}
	if len(metadata) > 0 {
		row.Metadata = sql.NullString{String: string(metadata), Valid: true}
	}
	if err := insertTimer(context.Background(), s.db, row); err != nil {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE", "TIMER_STORAGE_ERROR", err.Error())
	}
	s.signalScheduler()
	return s.sendTimerResponse(incoming, map[string]any{
		"ok":             true,
		"verb":           "TIMER_SCHEDULE",
		"timer_uuid":     timerID,
		"fire_at_utc_ms": fireAtUTCMS,
	})
}

func (s *Service) respondTimerScheduleRecurring(incoming fluxbeesdk.Message) error {
	var req timerScheduleRecurringRequest
	if err := json.Unmarshal(incoming.Payload, &req); err != nil {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE_RECURRING", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	ownerL2, err := s.resolveSourceL2Name(incoming.Routing.Src)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE_RECURRING", "TIMER_INTERNAL", err.Error())
	}
	fireAtUTCMS, cronTZ, err := scheduleRecurringFireAt(req)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE_RECURRING", timerErrorCode(err), err.Error())
	}
	targetL2 := ownerL2
	if req.TargetL2Name != nil && strings.TrimSpace(*req.TargetL2Name) != "" {
		targetL2 = strings.TrimSpace(*req.TargetL2Name)
	}
	if !isCanonicalL2Name(targetL2) {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE_RECURRING", "TIMER_INVALID_PAYLOAD", "target_l2_name must be a canonical L2 name")
	}
	missedPolicy := "fire"
	if req.MissedPolicy != nil && strings.TrimSpace(*req.MissedPolicy) != "" {
		missedPolicy = strings.TrimSpace(*req.MissedPolicy)
	}
	if err := validateMissedPolicyRaw(missedPolicy, req.MissedWithinMS); err != nil {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE_RECURRING", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	payload := req.Payload
	if len(payload) == 0 {
		payload = json.RawMessage(`{}`)
	}
	if !json.Valid(payload) {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE_RECURRING", "TIMER_INVALID_PAYLOAD", "payload must be valid JSON")
	}
	metadata := req.Metadata
	if len(metadata) > 0 && !json.Valid(metadata) {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE_RECURRING", "TIMER_INVALID_PAYLOAD", "metadata must be valid JSON")
	}
	timerID := uuid.NewString()
	nowUTCMS := time.Now().UTC().UnixMilli()
	row := timerRow{
		UUID:         timerID,
		OwnerL2Name:  ownerL2,
		TargetL2Name: targetL2,
		Kind:         "recurring",
		FireAtUTC:    fireAtUTCMS,
		CronSpec:     sql.NullString{String: strings.TrimSpace(req.CronSpec), Valid: true},
		CronTZ:       sql.NullString{String: cronTZ, Valid: true},
		MissedPolicy: missedPolicy,
		Payload:      string(payload),
		Status:       "pending",
		CreatedAtUTC: nowUTCMS,
		FireCount:    0,
	}
	if req.ClientRef != nil && strings.TrimSpace(*req.ClientRef) != "" {
		row.ClientRef = sql.NullString{String: strings.TrimSpace(*req.ClientRef), Valid: true}
	}
	if req.MissedWithinMS != nil {
		row.MissedWithinMS = sql.NullInt64{Int64: *req.MissedWithinMS, Valid: true}
	}
	if len(metadata) > 0 {
		row.Metadata = sql.NullString{String: string(metadata), Valid: true}
	}
	if err := insertTimer(context.Background(), s.db, row); err != nil {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE_RECURRING", "TIMER_STORAGE_ERROR", err.Error())
	}
	s.signalScheduler()
	return s.sendTimerResponse(incoming, map[string]any{
		"ok":             true,
		"verb":           "TIMER_SCHEDULE_RECURRING",
		"timer_uuid":     timerID,
		"fire_at_utc_ms": fireAtUTCMS,
	})
}

func (s *Service) respondTimerGet(incoming fluxbeesdk.Message) error {
	var req timerCancelRequest
	if err := json.Unmarshal(incoming.Payload, &req); err != nil {
		return s.sendTimerError(incoming, "TIMER_GET", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	if strings.TrimSpace(req.TimerUUID) == "" {
		return s.sendTimerError(incoming, "TIMER_GET", "TIMER_INVALID_PAYLOAD", "timer_uuid must be non-empty")
	}
	ownerL2, err := s.resolveSourceL2Name(incoming.Routing.Src)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_GET", "TIMER_INTERNAL", err.Error())
	}
	row, err := getTimer(context.Background(), s.db, req.TimerUUID)
	if err == sql.ErrNoRows {
		return s.sendTimerError(incoming, "TIMER_GET", "TIMER_NOT_FOUND", "timer not found")
	}
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_GET", "TIMER_STORAGE_ERROR", err.Error())
	}
	if row.OwnerL2Name != ownerL2 {
		return s.sendTimerError(incoming, "TIMER_GET", "TIMER_NOT_OWNER", "timer does not belong to requester")
	}
	info, err := timerRowToInfo(*row)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_GET", "TIMER_INTERNAL", err.Error())
	}
	return s.sendTimerResponse(incoming, map[string]any{
		"ok":    true,
		"verb":  "TIMER_GET",
		"timer": info,
	})
}

func (s *Service) respondTimerList(incoming fluxbeesdk.Message) error {
	var req timerListRequest
	if len(incoming.Payload) > 0 {
		if err := json.Unmarshal(incoming.Payload, &req); err != nil {
			return s.sendTimerError(incoming, "TIMER_LIST", "TIMER_INVALID_PAYLOAD", err.Error())
		}
	}
	ownerL2, err := s.resolveSourceL2Name(incoming.Routing.Src)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_LIST", "TIMER_INTERNAL", err.Error())
	}
	statusFilter := strings.TrimSpace(req.StatusFilter)
	if statusFilter == "" {
		statusFilter = "pending"
	}
	if statusFilter != "pending" && statusFilter != "fired" && statusFilter != "canceled" && statusFilter != "all" {
		return s.sendTimerError(incoming, "TIMER_LIST", "TIMER_INVALID_PAYLOAD", "status_filter must be pending, fired, canceled, or all")
	}
	rows, err := listTimers(context.Background(), s.db, ownerL2, statusFilter, req.Limit)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_LIST", "TIMER_STORAGE_ERROR", err.Error())
	}
	items := make([]fluxbeesdk.TimerInfo, 0, len(rows))
	for _, row := range rows {
		info, err := timerRowToInfo(row)
		if err != nil {
			return s.sendTimerError(incoming, "TIMER_LIST", "TIMER_INTERNAL", err.Error())
		}
		items = append(items, info)
	}
	return s.sendTimerResponse(incoming, map[string]any{
		"ok":     true,
		"verb":   "TIMER_LIST",
		"count":  len(items),
		"timers": items,
	})
}

func (s *Service) respondTimerCancel(incoming fluxbeesdk.Message) error {
	var req timerCancelRequest
	if err := json.Unmarshal(incoming.Payload, &req); err != nil {
		return s.sendTimerError(incoming, "TIMER_CANCEL", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	if strings.TrimSpace(req.TimerUUID) == "" {
		return s.sendTimerError(incoming, "TIMER_CANCEL", "TIMER_INVALID_PAYLOAD", "timer_uuid must be non-empty")
	}
	ownerL2, err := s.resolveSourceL2Name(incoming.Routing.Src)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_CANCEL", "TIMER_INTERNAL", err.Error())
	}
	row, err := getTimer(context.Background(), s.db, req.TimerUUID)
	if err == sql.ErrNoRows {
		return s.sendTimerError(incoming, "TIMER_CANCEL", "TIMER_NOT_FOUND", "timer not found")
	}
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_CANCEL", "TIMER_STORAGE_ERROR", err.Error())
	}
	if row.OwnerL2Name != ownerL2 {
		return s.sendTimerError(incoming, "TIMER_CANCEL", "TIMER_NOT_OWNER", "timer does not belong to requester")
	}
	if row.Status == "fired" {
		return s.sendTimerError(incoming, "TIMER_CANCEL", "TIMER_ALREADY_FIRED", "timer already fired")
	}
	row.Status = "canceled"
	if err := updateTimer(context.Background(), s.db, *row); err != nil {
		return s.sendTimerError(incoming, "TIMER_CANCEL", "TIMER_STORAGE_ERROR", err.Error())
	}
	s.signalScheduler()
	return s.sendTimerResponse(incoming, map[string]any{
		"ok":         true,
		"verb":       "TIMER_CANCEL",
		"timer_uuid": row.UUID,
		"status":     row.Status,
	})
}

func (s *Service) respondTimerReschedule(incoming fluxbeesdk.Message) error {
	var req timerRescheduleRequest
	if err := json.Unmarshal(incoming.Payload, &req); err != nil {
		return s.sendTimerError(incoming, "TIMER_RESCHEDULE", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	if strings.TrimSpace(req.TimerUUID) == "" {
		return s.sendTimerError(incoming, "TIMER_RESCHEDULE", "TIMER_INVALID_PAYLOAD", "timer_uuid must be non-empty")
	}
	ownerL2, err := s.resolveSourceL2Name(incoming.Routing.Src)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_RESCHEDULE", "TIMER_INTERNAL", err.Error())
	}
	row, err := getTimer(context.Background(), s.db, req.TimerUUID)
	if err == sql.ErrNoRows {
		return s.sendTimerError(incoming, "TIMER_RESCHEDULE", "TIMER_NOT_FOUND", "timer not found")
	}
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_RESCHEDULE", "TIMER_STORAGE_ERROR", err.Error())
	}
	if row.OwnerL2Name != ownerL2 {
		return s.sendTimerError(incoming, "TIMER_RESCHEDULE", "TIMER_NOT_OWNER", "timer does not belong to requester")
	}
	if row.Kind == "recurring" {
		return s.sendTimerError(incoming, "TIMER_RESCHEDULE", "TIMER_RECURRING_NOT_RESCHEDULABLE", "recurring timers cannot be rescheduled in v1")
	}
	if row.Status != "pending" {
		return s.sendTimerError(incoming, "TIMER_RESCHEDULE", "TIMER_ALREADY_FIRED", "timer is not pending")
	}
	fireAtUTCMS, err := rescheduleFireAt(req)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_RESCHEDULE", timerErrorCode(err), err.Error())
	}
	row.FireAtUTC = fireAtUTCMS
	if err := updateTimer(context.Background(), s.db, *row); err != nil {
		return s.sendTimerError(incoming, "TIMER_RESCHEDULE", "TIMER_STORAGE_ERROR", err.Error())
	}
	s.signalScheduler()
	return s.sendTimerResponse(incoming, map[string]any{
		"ok":             true,
		"verb":           "TIMER_RESCHEDULE",
		"timer_uuid":     row.UUID,
		"fire_at_utc_ms": fireAtUTCMS,
	})
}

func (s *Service) respondTimerPurgeOwner(incoming fluxbeesdk.Message) error {
	var req timerPurgeOwnerRequest
	if err := json.Unmarshal(incoming.Payload, &req); err != nil {
		return s.sendTimerError(incoming, "TIMER_PURGE_OWNER", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	requesterL2, err := s.resolveSourceL2Name(incoming.Routing.Src)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_PURGE_OWNER", "TIMER_INTERNAL", err.Error())
	}
	localOrchestrator := s.localOrchestratorL2Name()
	if requesterL2 != localOrchestrator {
		return s.sendTimerError(incoming, "TIMER_PURGE_OWNER", "TIMER_FORBIDDEN", "operation is reserved to local SY.orchestrator")
	}
	ownerL2Name := strings.TrimSpace(req.OwnerL2Name)
	if !isCanonicalL2Name(ownerL2Name) {
		return s.sendTimerError(incoming, "TIMER_PURGE_OWNER", "TIMER_INVALID_PAYLOAD", "owner_l2_name must be a canonical L2 name")
	}
	deletedCount, err := deleteTimersByOwner(context.Background(), s.db, ownerL2Name)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_PURGE_OWNER", "TIMER_STORAGE_ERROR", err.Error())
	}
	s.signalScheduler()
	return s.sendTimerResponse(incoming, map[string]any{
		"ok":            true,
		"verb":          "TIMER_PURGE_OWNER",
		"deleted_count": deletedCount,
	})
}

func (s *Service) sendTimerResponse(incoming fluxbeesdk.Message, payload any) error {
	response, err := fluxbeesdk.BuildSystemResponse(
		&incoming,
		s.sender.UUID(),
		fluxbeesdk.MsgTimerResponse,
		payload,
		fluxbeesdk.SystemEnvelopeOptions{},
	)
	if err != nil {
		return err
	}
	return s.sender.Send(response)
}

func (s *Service) sendTimerError(incoming fluxbeesdk.Message, verb, code, message string) error {
	return s.sendTimerResponse(incoming, map[string]any{
		"ok":   false,
		"verb": verb,
		"error": map[string]any{
			"code":    code,
			"message": message,
		},
	})
}

func (s *Service) signalScheduler() {
	if s.scheduler != nil {
		s.scheduler.signal()
	}
}

func loadLocation(raw string) (*time.Location, error) {
	if strings.TrimSpace(raw) == "" {
		return nil, fmt.Errorf("timezone must be non-empty")
	}
	return time.LoadLocation(raw)
}

func (s *Service) resolveSourceL2Name(sourceUUID string) (string, error) {
	_, hive, ok := strings.Cut(s.nodeName, "@")
	if !ok || strings.TrimSpace(hive) == "" {
		return "", fmt.Errorf("service node name missing hive suffix")
	}
	return fluxbeesdk.ResolvePeerL2Name(sourceUUID, fluxbeesdk.PeerIdentityResolver{
		UUIDPersistenceDir: s.uuidPersistenceDir,
		HiveID:             hive,
	})
}

func (s *Service) localOrchestratorL2Name() string {
	_, hive, _ := strings.Cut(s.nodeName, "@")
	return fmt.Sprintf("SY.orchestrator@%s", hive)
}

func scheduleFireAt(req timerScheduleRequest) (int64, error) {
	hasAbsolute := req.FireAtUTCMS != nil
	hasRelative := req.FireInMS != nil
	if hasAbsolute == hasRelative {
		return 0, fmt.Errorf("exactly one of fire_at_utc_ms or fire_in_ms must be set")
	}
	if hasAbsolute {
		fireAt := *req.FireAtUTCMS
		if fireAt-time.Now().UTC().UnixMilli() < int64(fluxbeesdk.MinTimerDuration/time.Millisecond) {
			return 0, errBelowMinimum()
		}
		return fireAt, nil
	}
	if *req.FireInMS < int64(fluxbeesdk.MinTimerDuration/time.Millisecond) {
		return 0, errBelowMinimum()
	}
	return time.Now().UTC().Add(time.Duration(*req.FireInMS) * time.Millisecond).UnixMilli(), nil
}

func scheduleRecurringFireAt(req timerScheduleRecurringRequest) (int64, string, error) {
	cronSpec := strings.TrimSpace(req.CronSpec)
	if cronSpec == "" {
		return 0, "", fmt.Errorf("cron_spec must be non-empty")
	}
	cronTZ := "UTC"
	if strings.TrimSpace(req.CronTZ) != "" {
		cronTZ = strings.TrimSpace(req.CronTZ)
	}
	loc, err := loadLocation(cronTZ)
	if err != nil {
		return 0, "", fmt.Errorf("invalid timezone: %w", err)
	}
	parser := cron.NewParser(cron.Minute | cron.Hour | cron.Dom | cron.Month | cron.Dow)
	schedule, err := parser.Parse(cronSpec)
	if err != nil {
		return 0, "", fmt.Errorf("invalid cron: %w", err)
	}
	now := time.Now().UTC()
	first := schedule.Next(now.In(loc))
	if first.IsZero() {
		return 0, "", fmt.Errorf("invalid cron: no future fire time")
	}
	next := schedule.Next(first.Add(time.Second).In(loc))
	if !next.IsZero() && next.Sub(first) < fluxbeesdk.MinTimerDuration {
		return 0, "", errBelowMinimum()
	}
	return first.UTC().UnixMilli(), cronTZ, nil
}

func rescheduleFireAt(req timerRescheduleRequest) (int64, error) {
	hasAbsolute := req.NewFireAtUTCMS != nil
	hasRelative := req.NewFireInMS != nil
	if hasAbsolute == hasRelative {
		return 0, fmt.Errorf("exactly one of new_fire_at_utc_ms or new_fire_in_ms must be set")
	}
	if hasAbsolute {
		fireAt := *req.NewFireAtUTCMS
		if fireAt-time.Now().UTC().UnixMilli() < int64(fluxbeesdk.MinTimerDuration/time.Millisecond) {
			return 0, errBelowMinimum()
		}
		return fireAt, nil
	}
	if *req.NewFireInMS < int64(fluxbeesdk.MinTimerDuration/time.Millisecond) {
		return 0, errBelowMinimum()
	}
	return time.Now().UTC().Add(time.Duration(*req.NewFireInMS) * time.Millisecond).UnixMilli(), nil
}

func validateMissedPolicyRaw(policy string, withinMS *int64) error {
	switch policy {
	case "fire", "drop":
		return nil
	case "fire_if_within":
		if withinMS == nil || *withinMS <= 0 {
			return fmt.Errorf("missed_within_ms must be set when missed_policy=fire_if_within")
		}
		return nil
	default:
		return fmt.Errorf("invalid missed_policy: %s", policy)
	}
}

func isCanonicalL2Name(value string) bool {
	value = strings.TrimSpace(value)
	if value == "" {
		return false
	}
	local, hive, ok := strings.Cut(value, "@")
	if !ok || strings.TrimSpace(local) == "" || strings.TrimSpace(hive) == "" {
		return false
	}
	return strings.Contains(local, ".")
}

func errBelowMinimum() error {
	return fmt.Errorf("minimum timer duration is %d seconds", int(fluxbeesdk.MinTimerDuration.Seconds()))
}

func timerErrorCode(err error) string {
	if err == nil {
		return "TIMER_INTERNAL"
	}
	if strings.Contains(err.Error(), "minimum timer duration") {
		return "TIMER_BELOW_MINIMUM"
	}
	if strings.Contains(err.Error(), "invalid timezone") {
		return "TIMER_INVALID_TIMEZONE"
	}
	if strings.Contains(err.Error(), "invalid cron") {
		return "TIMER_INVALID_CRON"
	}
	return "TIMER_INVALID_PAYLOAD"
}

func formatTimeMillisUTC(t time.Time) string {
	return t.UTC().Truncate(time.Millisecond).Format("2006-01-02T15:04:05.000Z07:00")
}

func formatTimeMillisLocal(t time.Time) string {
	return t.Truncate(time.Millisecond).Format("2006-01-02T15:04:05.000Z07:00")
}

func stringValue(value *string) string {
	if value == nil {
		return ""
	}
	return *value
}
