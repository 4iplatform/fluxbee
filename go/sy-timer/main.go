//go:build linux

package main

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"
	_ "time/tzdata"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
	"github.com/google/uuid"
	_ "modernc.org/sqlite"
)

const (
	version             = "1.6"
	defaultNodeBaseName = "SY.timer"
	configDir           = "/etc/fluxbee"
	routerSockDir       = "/var/run/fluxbee/routers"
)

var uuidPersistenceDir = "/var/lib/fluxbee/state/nodes"

// Caps on what a node may ask of SY.timer (FINDINGS A-115).
const (
	// maxTimerDelay is how far ahead a one-shot may fire.
	maxTimerDelay = 366 * 24 * time.Hour
	// maxTimerDataBytes caps a timer's payload, and its metadata: a TIMER_FIRED with both fits in
	// the router's 128 KiB frame.
	maxTimerDataBytes = 32 * 1024
	// maxTimerListPageBytes is the most a TIMER_LIST page's timers take, encoded: with the rest of
	// the response it fits the router's 128 KiB frame (FINDINGS A-151).
	maxTimerListPageBytes = 96 * 1024
)

// maxPendingTimersPerOwner and maxPendingTimers cap the pending timers of one owner, and of the hive.
var (
	maxPendingTimersPerOwner = 10_000
	maxPendingTimers         = 100_000
)

type messageSender interface {
	Send(fluxbeesdk.Message) error
	UUID() string
	FullName() string
}

type Service struct {
	sender      messageSender
	incoming    <-chan fluxbeesdk.Message
	nodeName    string
	instanceDir string
	dbPath      string
	db          *sql.DB
	scheduler   *timerScheduler
	// refusals logs the requests SY.timer refuses, bounded per sender (refusalLog, FINDINGS A-258).
	refusals refusalLog
	// ignored logs the messages that are no request, bounded per sender in windows of their own
	// (ignoredLog, the review of A-258).
	ignored ignoredLog
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
	TimerUUID string `json:"timer_uuid,omitempty"`
	ClientRef string `json:"client_ref,omitempty"`
}

type timerRescheduleRequest struct {
	TimerUUID      string `json:"timer_uuid,omitempty"`
	ClientRef      string `json:"client_ref,omitempty"`
	NewFireAtUTCMS *int64 `json:"new_fire_at_utc_ms,omitempty"`
	NewFireInMS    *int64 `json:"new_fire_in_ms,omitempty"`
}

type timerListRequest struct {
	OwnerL2Name  string `json:"owner_l2_name,omitempty"`
	StatusFilter string `json:"status_filter,omitempty"`
	Limit        int    `json:"limit,omitempty"`
	// Cursor is the next_cursor of the page before, where this one starts.
	Cursor string `json:"cursor,omitempty"`
}

type timerPurgeOwnerRequest struct {
	OwnerL2Name string `json:"owner_l2_name"`
}

type timerReference struct {
	TimerUUID string
	ClientRef string
}

func main() {
	log.SetFlags(log.LstdFlags | log.Lmicroseconds)

	hiveID, err := fluxbeesdk.LoadHiveID(configDir)
	if err != nil {
		log.Fatalf("failed to load hive id: %v", err)
	}
	selfIlkID, err := fluxbeesdk.WaitForSelfSystemIlkID(
		hiveID,
		defaultNodeBaseName,
		30*time.Second,
		250*time.Millisecond,
	)
	if err != nil {
		log.Fatalf("failed to resolve self system ILK from identity SHM: %v", err)
	}
	log.Printf("resolved self system ILK from identity SHM: %s", selfIlkID)
	_ = selfIlkID // cached for future outgoing meta.src_ilk use

	profile, err := fluxbeesdk.NewOperationalRouteProfile().
		CommandChannel("incoming").
		PostPendingRule(
			fluxbeesdk.RouteAnyMsgOfType{MsgType: fluxbeesdk.SYSTEMKind},
			fluxbeesdk.RouteCommand{Channel: "incoming"},
		).
		Build()
	if err != nil {
		log.Fatalf("sy.timer rpc profile invalid: %v", err)
	}
	dispatcher, err := fluxbeesdk.ConnectWithRetry(fluxbeesdk.NodeConfig{
		Name:               defaultNodeBaseName,
		RouterSocket:       routerSockDir,
		UUIDPersistenceDir: uuidPersistenceDir,
		UUIDMode:           fluxbeesdk.NodeUuidPersistent,
		ConfigDir:          configDir,
		Version:            version,
	}, time.Second, profile)
	if err != nil {
		log.Fatalf("failed to connect node lifecycle: %v", err)
	}
	sender := dispatcher.SenderSnapshot()
	incoming, err := dispatcher.TakeCommandReceiver("incoming")
	if err != nil {
		log.Fatalf("failed to take incoming channel: %v", err)
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
		sender:      sender,
		incoming:    incoming,
		nodeName:    nodeName,
		instanceDir: instanceDir,
		dbPath:      dbPath,
		db:          db,
	}
	service.scheduler = newTimerScheduler(service)
	deleted, stamped, gcErr := gcHistoricalTimers(context.Background(), db, time.Now().UTC(), defaultHistoryRetention)
	if gcErr != nil {
		log.Printf("historical timer gc failed: %v", gcErr)
	}
	if err := service.scheduler.replayPending(context.Background(), time.Now().UTC()); err != nil {
		log.Fatalf("failed to replay pending timers: %v", err)
	}
	pendingCount, pendingErr := countPendingTimers(context.Background(), db)
	if pendingErr != nil {
		log.Printf("failed to count pending timers: %v", pendingErr)
	}
	startup := map[string]any{
		"node_name": service.nodeName,
		"db_path":   service.dbPath,
	}
	pending := startupPending(startup, pendingCount, pendingErr)
	gc := startupGC(startup, deleted, stamped, gcErr)
	logEvent("timer_startup", startup)
	log.Printf("SY.timer ready: node=%s db=%s %s %s", service.nodeName, service.dbPath, pending, gc)
	service.scheduler.start()
	go service.sweepLogsEvery(senderLogSweepInterval)
	service.run()
}

// startupPending puts the start's count of pending timers in the timer_startup event's fields
// (pending_count), and returns it for the ready line (pending=N). A count that failed is 0, which
// would read as a hive with no pending timer: it is pending=failed instead, with pending_error in the
// event (FINDINGS A-259).
func startupPending(fields map[string]any, count int64, err error) string {
	if err != nil {
		fields["pending_error"] = err.Error()
		return "pending=failed"
	}
	fields["pending_count"] = count
	return fmt.Sprintf("pending=%d", count)
}

// startupGC puts the start's history GC (gcHistoricalTimers) in the timer_startup event's fields, and
// returns it for the ready line: gc_deleted, and gc_stamped, the canceled timers it dated, canceled
// before 1.5 or by a SY.timer before it, which are kept 7 days from now (FINDINGS A-238). A GC that
// failed changed nothing, and its counts are 0, as for one that found nothing to delete or to date:
// it is gc=failed instead, with gc_error in the event.
func startupGC(fields map[string]any, deleted, stamped int64, err error) string {
	if err != nil {
		fields["gc_error"] = err.Error()
		return "gc=failed"
	}
	fields["gc_deleted"] = deleted
	fields["gc_stamped"] = stamped
	return fmt.Sprintf("gc_deleted=%d gc_stamped=%d", deleted, stamped)
}

func (s *Service) run() {
	for msg := range s.incoming {
		if err := s.handleMessage(msg); err != nil {
			// Who asked, as a refusal's line says (the review of A-258): an answer over the frame is
			// answered TIMER_INTERNAL here, not by sendTimerError.
			logEvent("timer_handler_error", map[string]any{
				"error":       err.Error(),
				"msg_type":    msg.Meta.MsgType,
				"verb":        stringValue(msg.Meta.Msg),
				"src_l2_name": msg.SourceL2Name(),
				"trace_id":    msg.Routing.TraceID,
			})
			log.Printf("handle message error: %v", err)
		}
	}
	log.Printf("sy.timer incoming channel closed; exiting run loop")
}

func (s *Service) handleMessage(msg fluxbeesdk.Message) error {
	if msg.Meta.MsgType != fluxbeesdk.SYSTEMKind {
		return nil
	}
	// A fire waits for no answer, so the router's UNREACHABLE for one comes here and not as an RPC
	// error: read with the SDK's UnreachablePayload, as the SDK reads one for a request (FINDINGS
	// A-228).
	if stringValue(msg.Meta.Msg) == fluxbeesdk.MSGUnreachable && s.scheduler != nil {
		var notice fluxbeesdk.UnreachablePayload
		_ = json.Unmarshal(msg.Payload, &notice)
		if s.scheduler.undelivered(msg.Routing.TraceID, notice.Reason) {
			return nil
		}
	}
	if isInfrastructureMessage(msg) {
		// One line, bounded per sender as refusals are (ignoredLog): before, a plain line each, and a
		// node that looped notices or replies at SY.timer wrote one per message (the review of A-258).
		s.ignored.note(time.Now(), msg.SourceL2Name(), stringValue(msg.Meta.Msg), msg.Routing.TraceID)
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
		return s.sendTimerError(msg, stringValue(msg.Meta.Msg), "TIMER_UNKNOWN_VERB", fmt.Sprintf("SY.timer does not know the verb %q", stringValue(msg.Meta.Msg)))
	}
}

// isInfrastructureMessage reports whether msg gets no answer: the router made it (it carries no
// routing.src_l2_name, which the router stamps on everything a node sends), or it is a notice or a
// reply, not a request: HELLO, ANNOUNCE, WITHDRAW, UNREACHABLE, TTL_EXCEEDED, a message without a
// name, or any *_RESPONSE (SY.timer awaits none). It is the AI SDK's is_infrastructure_message, name
// by name, and wf-generic's isNotice and neverAnswered (FINDINGS A-218). An answer to the router's
// UNREACHABLE would go to the router's own UUID, which no node has: the router would answer
// UNREACHABLE again, and the two would bounce it forever (a TIMER_FIRED for a node not connected yet
// starts it).
func isInfrastructureMessage(msg fluxbeesdk.Message) bool {
	if msg.SourceL2Name() == "" {
		return true
	}
	name := stringValue(msg.Meta.Msg)
	switch name {
	case "",
		fluxbeesdk.MSGHello,
		fluxbeesdk.MSGAnnounce,
		fluxbeesdk.MSGWithdraw,
		fluxbeesdk.MSGUnreachable,
		fluxbeesdk.MSGTTLExceeded:
		return true
	}
	return strings.HasSuffix(name, "_RESPONSE")
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
			{Verb: "TIMER_SCHEDULE", Description: "Create a one-shot timer, 60 seconds to 366 days ahead. Its fire goes to the requester; only a SY node may name another target_l2_name. If client_ref already matches an active owned timer, return the existing timer instead of creating a duplicate.", RequiredFields: []string{"fire_at_utc_ms OR fire_in_ms"}, OptionalFields: []string{"target_l2_name", "client_ref", "payload", "missed_policy", "missed_within_ms", "metadata"}},
			{Verb: "TIMER_SCHEDULE_RECURRING", Description: "Create a recurring timer from a 5-field cron expression. Its fires go to the requester; only a SY node may name another target_l2_name. If client_ref already matches an active owned timer, return the existing timer instead of creating a duplicate.", RequiredFields: []string{"cron_spec"}, OptionalFields: []string{"cron_tz", "target_l2_name", "client_ref", "payload", "missed_policy", "missed_within_ms", "metadata"}},
			{Verb: "TIMER_GET", Description: "Read one of the caller's timers by uuid, or its active timer by client_ref. SY.admin reads any timer by uuid. A timer's canceled_reason is present only when SY.timer canceled it itself, and says why: a one-shot its missed_policy dropped at a start, or a recurring timer whose next fire cannot be computed.", RequiredFields: []string{"timer_uuid OR client_ref"}},
			{Verb: "TIMER_LIST", Description: "List the caller's timers, by fire time, a page at a time: an answer with next_cursor has more, which the same request with cursor set to it lists. SY.admin lists every owner's, or one owner's with owner_l2_name. Each timer as TIMER_GET reads it: its canceled_reason is present only when SY.timer canceled it itself, and says why (a one-shot its missed_policy dropped, a recurring timer whose next fire cannot be computed).", OptionalFields: []string{"owner_l2_name", "status_filter", "limit", "cursor"}},
			{Verb: "TIMER_CANCEL", Description: "Cancel a pending owned timer by uuid or client_ref.", RequiredFields: []string{"timer_uuid OR client_ref"}},
			{Verb: "TIMER_RESCHEDULE", Description: "Reschedule a pending one-shot timer owned by the requester, by uuid or client_ref.", RequiredFields: []string{"timer_uuid OR client_ref", "new_fire_at_utc_ms OR new_fire_in_ms"}},
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
			{Code: "TIMER_ABOVE_MAXIMUM", Description: "Requested one-shot fires more than 366 days ahead."},
			{Code: "TIMER_LIMIT_EXCEEDED", Description: "A payload or metadata over 32 KiB, or the owner (10000) or the hive (100000) at its most pending timers."},
			{Code: "TIMER_INVALID_PAYLOAD", Description: "Request payload is malformed or missing required fields."},
			{Code: "TIMER_INVALID_CRON", Description: "Recurring cron expression is invalid, or starts with TZ=, CRON_TZ= or @: its zone goes in cron_tz, and v1 has no descriptors."},
			{Code: "TIMER_INVALID_TIMEZONE", Description: "Requested timezone is not a valid IANA timezone."},
			{Code: "TIMER_FORBIDDEN", Description: "Requester is not authorized for the requested operation: another node's target, another owner's list, or the purge."},
			{Code: "TIMER_NOT_FOUND", Description: "Requested timer does not exist."},
			{Code: "TIMER_NOT_OWNER", Description: "Requester does not own the timer it reads, cancels or reschedules."},
			{Code: "TIMER_ALREADY_FIRED", Description: "Timer already fired and can no longer be modified."},
			{Code: "TIMER_RECURRING_NOT_RESCHEDULABLE", Description: "Recurring timers cannot be rescheduled in v1."},
			{Code: "TIMER_UNKNOWN_VERB", Description: "SY.timer does not know the verb."},
			{Code: "TIMER_INTERNAL", Description: "An internal error occurred, such as an answer that does not fit in one frame."},
		},
	)
	return s.sendTimerResponse(incoming, payload)
}

func (s *Service) respondTimerSchedule(incoming fluxbeesdk.Message) error {
	var req timerScheduleRequest
	if err := json.Unmarshal(incoming.Payload, &req); err != nil {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	ownerL2, err := s.resolveSourceL2NameFromMsg(incoming)
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
	if err := checkTimerTarget(ownerL2, targetL2); err != nil {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE", "TIMER_FORBIDDEN", err.Error())
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
	if err := checkTimerData(payload, metadata); err != nil {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE", "TIMER_LIMIT_EXCEEDED", err.Error())
	}
	nowUTCMS := time.Now().UTC().UnixMilli()
	clientRef, err := normalizeClientRef(req.ClientRef)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	if clientRef != "" {
		existing, err := findPendingTimerByClientRef(context.Background(), s.db, ownerL2, clientRef)
		if err != nil && err != sql.ErrNoRows {
			return s.sendTimerError(incoming, "TIMER_SCHEDULE", "TIMER_STORAGE_ERROR", err.Error())
		}
		if err == nil && existing != nil {
			return s.sendTimerScheduleExistingResponse(incoming, "TIMER_SCHEDULE", *existing, true)
		}
	}
	if code, err := checkPendingLimits(context.Background(), s.db, ownerL2); err != nil {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE", code, err.Error())
	}
	timerID := uuid.NewString()
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
	if clientRef != "" {
		row.ClientRef = sql.NullString{String: clientRef, Valid: true}
	}
	if req.MissedWithinMS != nil {
		row.MissedWithinMS = sql.NullInt64{Int64: *req.MissedWithinMS, Valid: true}
	}
	if len(metadata) > 0 {
		row.Metadata = sql.NullString{String: string(metadata), Valid: true}
	}
	if err := insertTimer(context.Background(), s.db, row); err != nil {
		if clientRef != "" && isPendingClientRefUniqueViolation(err) {
			existing, lookupErr := findPendingTimerByClientRef(context.Background(), s.db, ownerL2, clientRef)
			if lookupErr == nil && existing != nil {
				return s.sendTimerScheduleExistingResponse(incoming, "TIMER_SCHEDULE", *existing, true)
			}
		}
		return s.sendTimerError(incoming, "TIMER_SCHEDULE", "TIMER_STORAGE_ERROR", err.Error())
	}
	s.enqueueTimer(timerID, fireAtUTCMS)
	logEvent("timer_scheduled", map[string]any{
		"timer_uuid":     timerID,
		"owner_l2_name":  ownerL2,
		"target_l2_name": targetL2,
		"kind":           "oneshot",
		"fire_at_utc_ms": fireAtUTCMS,
		"trace_id":       incoming.Routing.TraceID,
		"missed_policy":  missedPolicy,
		"has_client_ref": row.ClientRef.Valid,
		"has_metadata":   row.Metadata.Valid,
	})
	return s.sendTimerResponse(incoming, map[string]any{
		"ok":              true,
		"verb":            "TIMER_SCHEDULE",
		"timer_uuid":      timerID,
		"fire_at_utc_ms":  fireAtUTCMS,
		"already_existed": false,
	})
}

func (s *Service) respondTimerScheduleRecurring(incoming fluxbeesdk.Message) error {
	var req timerScheduleRecurringRequest
	if err := json.Unmarshal(incoming.Payload, &req); err != nil {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE_RECURRING", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	ownerL2, err := s.resolveSourceL2NameFromMsg(incoming)
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
	if err := checkTimerTarget(ownerL2, targetL2); err != nil {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE_RECURRING", "TIMER_FORBIDDEN", err.Error())
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
	if err := checkTimerData(payload, metadata); err != nil {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE_RECURRING", "TIMER_LIMIT_EXCEEDED", err.Error())
	}
	nowUTCMS := time.Now().UTC().UnixMilli()
	clientRef, err := normalizeClientRef(req.ClientRef)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE_RECURRING", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	if clientRef != "" {
		existing, err := findPendingTimerByClientRef(context.Background(), s.db, ownerL2, clientRef)
		if err != nil && err != sql.ErrNoRows {
			return s.sendTimerError(incoming, "TIMER_SCHEDULE_RECURRING", "TIMER_STORAGE_ERROR", err.Error())
		}
		if err == nil && existing != nil {
			return s.sendTimerScheduleExistingResponse(incoming, "TIMER_SCHEDULE_RECURRING", *existing, true)
		}
	}
	if code, err := checkPendingLimits(context.Background(), s.db, ownerL2); err != nil {
		return s.sendTimerError(incoming, "TIMER_SCHEDULE_RECURRING", code, err.Error())
	}
	timerID := uuid.NewString()
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
	if clientRef != "" {
		row.ClientRef = sql.NullString{String: clientRef, Valid: true}
	}
	if req.MissedWithinMS != nil {
		row.MissedWithinMS = sql.NullInt64{Int64: *req.MissedWithinMS, Valid: true}
	}
	if len(metadata) > 0 {
		row.Metadata = sql.NullString{String: string(metadata), Valid: true}
	}
	if err := insertTimer(context.Background(), s.db, row); err != nil {
		if clientRef != "" && isPendingClientRefUniqueViolation(err) {
			existing, lookupErr := findPendingTimerByClientRef(context.Background(), s.db, ownerL2, clientRef)
			if lookupErr == nil && existing != nil {
				return s.sendTimerScheduleExistingResponse(incoming, "TIMER_SCHEDULE_RECURRING", *existing, true)
			}
		}
		return s.sendTimerError(incoming, "TIMER_SCHEDULE_RECURRING", "TIMER_STORAGE_ERROR", err.Error())
	}
	s.enqueueTimer(timerID, fireAtUTCMS)
	logEvent("timer_scheduled", map[string]any{
		"timer_uuid":     timerID,
		"owner_l2_name":  ownerL2,
		"target_l2_name": targetL2,
		"kind":           "recurring",
		"fire_at_utc_ms": fireAtUTCMS,
		"cron_tz":        cronTZ,
		"trace_id":       incoming.Routing.TraceID,
		"missed_policy":  missedPolicy,
		"has_client_ref": row.ClientRef.Valid,
		"has_metadata":   row.Metadata.Valid,
	})
	return s.sendTimerResponse(incoming, map[string]any{
		"ok":              true,
		"verb":            "TIMER_SCHEDULE_RECURRING",
		"timer_uuid":      timerID,
		"fire_at_utc_ms":  fireAtUTCMS,
		"already_existed": false,
	})
}

func (s *Service) respondTimerGet(incoming fluxbeesdk.Message) error {
	var req timerCancelRequest
	if err := json.Unmarshal(incoming.Payload, &req); err != nil {
		return s.sendTimerError(incoming, "TIMER_GET", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	ref, err := timerReferenceFromRequest(req.TimerUUID, req.ClientRef)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_GET", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	row, err := s.resolveTimerForGet(context.Background(), incoming, ref)
	if err == sql.ErrNoRows {
		return s.sendTimerError(incoming, "TIMER_GET", "TIMER_NOT_FOUND", "timer not found")
	}
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_GET", "TIMER_STORAGE_ERROR", err.Error())
	}
	if !mayRead(incoming, row.OwnerL2Name) {
		return s.sendTimerError(incoming, "TIMER_GET", "TIMER_NOT_OWNER", "a timer is read by its owner and by SY.admin only")
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
	statusFilter := strings.TrimSpace(req.StatusFilter)
	if statusFilter == "" {
		statusFilter = "pending"
	}
	if statusFilter != "pending" && statusFilter != "fired" && statusFilter != "canceled" && statusFilter != "all" {
		return s.sendTimerError(incoming, "TIMER_LIST", "TIMER_INVALID_PAYLOAD", "status_filter must be pending, fired, canceled, or all")
	}
	ownerFilter := strings.TrimSpace(req.OwnerL2Name)
	if ownerFilter != "" && !isCanonicalL2Name(ownerFilter) {
		return s.sendTimerError(incoming, "TIMER_LIST", "TIMER_INVALID_PAYLOAD", "owner_l2_name must be a canonical L2 name")
	}
	if incoming.SourceL2Name() != fluxbeesdk.PrimaryAdminNode {
		if ownerFilter == "" {
			ownerFilter = incoming.SourceL2Name()
		}
		if !mayRead(incoming, ownerFilter) {
			return s.sendTimerError(incoming, "TIMER_LIST", "TIMER_FORBIDDEN", "a node lists its own timers; another owner's are listed by SY.admin only")
		}
	}
	after, err := parseTimerListCursor(req.Cursor)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_LIST", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	rows, err := listTimers(context.Background(), s.db, ownerFilter, statusFilter, req.Limit, after)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_LIST", "TIMER_STORAGE_ERROR", err.Error())
	}
	// A page fits one frame (FINDINGS A-151): the timers past maxTimerListPageBytes go in the next
	// page, after the cursor of the last one this page lists.
	items := make([]fluxbeesdk.TimerInfo, 0, len(rows))
	pageBytes := 0
	nextCursor := ""
	for i, row := range rows {
		info, err := timerRowToInfo(row)
		if err != nil {
			return s.sendTimerError(incoming, "TIMER_LIST", "TIMER_INTERNAL", err.Error())
		}
		encoded, err := json.Marshal(info)
		if err != nil {
			return s.sendTimerError(incoming, "TIMER_LIST", "TIMER_INTERNAL", err.Error())
		}
		if i > 0 && pageBytes+len(encoded)+1 > maxTimerListPageBytes {
			nextCursor = formatTimerListCursor(rows[i-1])
			break
		}
		pageBytes += len(encoded) + 1
		items = append(items, info)
	}
	response := map[string]any{
		"ok":            true,
		"verb":          "TIMER_LIST",
		"owner_l2_name": nullableJSONValue(ownerFilter),
		"count":         len(items),
		"timers":        items,
	}
	if nextCursor != "" {
		response["next_cursor"] = nextCursor
	}
	return s.sendTimerResponse(incoming, response)
}

// formatTimerListCursor is the cursor after row: its fire time, in ms, and its uuid.
func formatTimerListCursor(row timerRow) string {
	return fmt.Sprintf("%d:%s", row.FireAtUTC, row.UUID)
}

// parseTimerListCursor reads a cursor formatTimerListCursor wrote, nil for none.
func parseTimerListCursor(cursor string) (*timerListCursor, error) {
	cursor = strings.TrimSpace(cursor)
	if cursor == "" {
		return nil, nil
	}
	fireAt, uuid, ok := strings.Cut(cursor, ":")
	fireAtUTC, err := strconv.ParseInt(fireAt, 10, 64)
	if !ok || err != nil || strings.TrimSpace(uuid) == "" {
		// A-232: a refusal names the field, never the value it was sent.
		return nil, errors.New("cursor must be a next_cursor of TIMER_LIST")
	}
	return &timerListCursor{fireAtUTC: fireAtUTC, uuid: uuid}, nil
}

func (s *Service) respondTimerCancel(incoming fluxbeesdk.Message) error {
	var req timerCancelRequest
	if err := json.Unmarshal(incoming.Payload, &req); err != nil {
		return s.sendTimerError(incoming, "TIMER_CANCEL", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	ref, err := timerReferenceFromRequest(req.TimerUUID, req.ClientRef)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_CANCEL", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	row, foundByUUID, unlock, err := s.lockOwnedTimerForMutation(context.Background(), incoming, ref)
	defer unlock()
	if err == sql.ErrNoRows && !foundByUUID {
		return s.sendTimerResponse(incoming, map[string]any{
			"ok":    true,
			"verb":  "TIMER_CANCEL",
			"found": false,
		})
	}
	if err == sql.ErrNoRows {
		return s.sendTimerError(incoming, "TIMER_CANCEL", "TIMER_NOT_FOUND", "timer not found")
	}
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_CANCEL", timerStorageOrInternalCode(err), err.Error())
	}
	if row.Status == "fired" {
		return s.sendTimerError(incoming, "TIMER_CANCEL", "TIMER_ALREADY_FIRED", "timer already fired")
	}
	if row.Status == "pending" {
		// Its history is kept 7 days from now (canceled_at_utc, FINDINGS A-238). A timer canceled
		// already keeps its time: a cancel again does not lengthen it.
		row.CanceledAtUTC = nullableNow(time.Now())
	}
	row.Status = "canceled"
	if err := updateTimer(context.Background(), s.db, *row); err != nil {
		return s.sendTimerError(incoming, "TIMER_CANCEL", "TIMER_STORAGE_ERROR", err.Error())
	}
	s.dequeueTimers(row.UUID)
	logEvent("timer_canceled", map[string]any{
		"timer_uuid":     row.UUID,
		"owner_l2_name":  row.OwnerL2Name,
		"target_l2_name": row.TargetL2Name,
		"trace_id":       incoming.Routing.TraceID,
	})
	return s.sendTimerResponse(incoming, map[string]any{
		"ok":         true,
		"verb":       "TIMER_CANCEL",
		"timer_uuid": row.UUID,
		"status":     row.Status,
		"found":      true,
	})
}

func (s *Service) respondTimerReschedule(incoming fluxbeesdk.Message) error {
	var req timerRescheduleRequest
	if err := json.Unmarshal(incoming.Payload, &req); err != nil {
		return s.sendTimerError(incoming, "TIMER_RESCHEDULE", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	ref, err := timerReferenceFromRequest(req.TimerUUID, req.ClientRef)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_RESCHEDULE", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	row, foundByUUID, unlock, err := s.lockOwnedTimerForMutation(context.Background(), incoming, ref)
	defer unlock()
	if err == sql.ErrNoRows {
		if !foundByUUID {
			return s.sendTimerError(incoming, "TIMER_RESCHEDULE", "TIMER_NOT_FOUND", "timer not found")
		}
		return s.sendTimerError(incoming, "TIMER_RESCHEDULE", "TIMER_NOT_FOUND", "timer not found")
	}
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_RESCHEDULE", timerStorageOrInternalCode(err), err.Error())
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
	s.enqueueTimer(row.UUID, fireAtUTCMS)
	logEvent("timer_rescheduled", map[string]any{
		"timer_uuid":     row.UUID,
		"owner_l2_name":  row.OwnerL2Name,
		"target_l2_name": row.TargetL2Name,
		"fire_at_utc_ms": fireAtUTCMS,
		"trace_id":       incoming.Routing.TraceID,
	})
	return s.sendTimerResponse(incoming, map[string]any{
		"ok":             true,
		"verb":           "TIMER_RESCHEDULE",
		"timer_uuid":     row.UUID,
		"fire_at_utc_ms": fireAtUTCMS,
		"found":          true,
	})
}

func (s *Service) respondTimerPurgeOwner(incoming fluxbeesdk.Message) error {
	var req timerPurgeOwnerRequest
	if err := json.Unmarshal(incoming.Payload, &req); err != nil {
		return s.sendTimerError(incoming, "TIMER_PURGE_OWNER", "TIMER_INVALID_PAYLOAD", err.Error())
	}
	requesterL2, err := s.resolveSourceL2NameFromMsg(incoming)
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
	deletedCount, pending, err := deleteTimersByOwner(context.Background(), s.db, ownerL2Name)
	if err != nil {
		return s.sendTimerError(incoming, "TIMER_PURGE_OWNER", "TIMER_STORAGE_ERROR", err.Error())
	}
	s.dequeueTimers(pending...)
	logEvent("timer_owner_purged", map[string]any{
		"owner_l2_name": ownerL2Name,
		"deleted_count": deletedCount,
		"requester_l2":  requesterL2,
		"trace_id":      incoming.Routing.TraceID,
	})
	return s.sendTimerResponse(incoming, map[string]any{
		"ok":            true,
		"verb":          "TIMER_PURGE_OWNER",
		"deleted_count": deletedCount,
	})
}

// sendTimerResponse answers incoming with TIMER_RESPONSE. A response that cannot go (it does not
// build, or is over the frame limit, say a TIMER_LIST of many timers) is answered with a
// TIMER_INTERNAL error instead, so the requester does not wait out its timeout. Any other send
// failure (the send queue full during a reconnect, say) gets no error reply: the work may be done,
// and the requester's timeout reads "unknown, not failed". Every failure is returned, for the run
// loop to log.
func (s *Service) sendTimerResponse(incoming fluxbeesdk.Message, payload any) error {
	refused, err := s.sendTimerResponseOnce(incoming, payload)
	if err == nil {
		return nil
	}
	err = fmt.Errorf("%s not sent: %w", fluxbeesdk.MsgTimerResponse, err)
	if !refused {
		return err
	}
	if _, replyErr := s.sendTimerResponseOnce(incoming, timerErrorPayload(stringValue(incoming.Meta.Msg), "TIMER_INTERNAL", err.Error())); replyErr != nil {
		return fmt.Errorf("%w (the error reply was not sent either: %v)", err, replyErr)
	}
	return err
}

// sendTimerResponseOnce sends one TIMER_RESPONSE; refused says the response itself cannot go: it
// does not build, or is over the frame limit.
func (s *Service) sendTimerResponseOnce(incoming fluxbeesdk.Message, payload any) (refused bool, err error) {
	response, err := fluxbeesdk.BuildSystemResponse(
		&incoming,
		s.sender.UUID(),
		fluxbeesdk.MsgTimerResponse,
		payload,
		fluxbeesdk.SystemEnvelopeOptions{},
	)
	if err != nil {
		return true, err
	}
	err = s.sender.Send(response)
	var tooLarge *fluxbeesdk.MessageTooLargeError
	return errors.As(err, &tooLarge), err
}

// sendTimerError answers incoming with a refusal, and logs it first, bounded per sender (refusalLog,
// FINDINGS A-258): its verb, its code and its sender, not its message, but for a TIMER_STORAGE_ERROR,
// whose message is the database's. Before, only the sender heard of it.
func (s *Service) sendTimerError(incoming fluxbeesdk.Message, verb, code, message string) error {
	s.refusals.note(time.Now(), incoming.SourceL2Name(), verb, code, incoming.Routing.TraceID, message)
	return s.sendTimerResponse(incoming, timerErrorPayload(verb, code, message))
}

func timerErrorPayload(verb, code, message string) map[string]any {
	return map[string]any{
		"ok":   false,
		"verb": verb,
		"error": map[string]any{
			"code":    code,
			"message": message,
		},
	}
}

// enqueueTimer queues a timer scheduled or moved (TIMER_RESCHEDULE) for its fire at fireAtUTCMS: a
// timer queued already moves there (timerScheduler.push).
func (s *Service) enqueueTimer(timerUUID string, fireAtUTCMS int64) {
	if s.scheduler != nil {
		s.scheduler.push(timerUUID, fireAtUTCMS)
	}
}

// dequeueTimers takes canceled or purged timers out of the scheduler's heap: they wait for no fire
// (FINDINGS A-259).
func (s *Service) dequeueTimers(timerUUIDs ...string) {
	if s.scheduler != nil {
		s.scheduler.remove(timerUUIDs...)
	}
}

func loadLocation(raw string) (*time.Location, error) {
	if strings.TrimSpace(raw) == "" {
		return nil, fmt.Errorf("timezone must be non-empty")
	}
	return time.LoadLocation(raw)
}

func nullableJSONValue(value string) any {
	value = strings.TrimSpace(value)
	if value == "" {
		return nil
	}
	return value
}

// resolveSourceL2NameFromMsg returns the canonical L2 name of the sender.
// It reads routing.src_l2_name stamped authoritatively by the router on delivery.
// Returns an error if the stamp is absent — all messages from live routers must
// carry this field.
func (s *Service) resolveSourceL2NameFromMsg(incoming fluxbeesdk.Message) (string, error) {
	if name := incoming.SourceL2Name(); name != "" {
		return name, nil
	}
	return "", timerErrorf("TIMER_INTERNAL", "routing.src_l2_name missing from incoming message (src=%s)", incoming.Routing.Src)
}

// mayRead reports whether the sender of incoming may read the timers of owner: a node reads its own,
// and SY.admin every one (FINDINGS A-115).
func mayRead(incoming fluxbeesdk.Message, owner string) bool {
	requester := incoming.SourceL2Name()
	return requester != "" && (requester == owner || requester == fluxbeesdk.PrimaryAdminNode)
}

// checkTimerTarget refuses a timer whose fires go to another node than its owner, unless the owner is
// a SY node: a node schedules for itself (FINDINGS A-115).
func checkTimerTarget(ownerL2, targetL2 string) error {
	if targetL2 == ownerL2 || strings.HasPrefix(ownerL2, "SY.") {
		return nil
	}
	return fmt.Errorf("a node schedules timers for itself only: target_l2_name %q is not %q", targetL2, ownerL2)
}

// checkTimerData refuses a payload or metadata over maxTimerDataBytes.
func checkTimerData(payload, metadata json.RawMessage) error {
	if len(payload) > maxTimerDataBytes {
		return fmt.Errorf("payload is %d bytes, at most %d", len(payload), maxTimerDataBytes)
	}
	if len(metadata) > maxTimerDataBytes {
		return fmt.Errorf("metadata is %d bytes, at most %d", len(metadata), maxTimerDataBytes)
	}
	return nil
}

// checkPendingLimits refuses a new timer once its owner, or the hive, has as many pending timers as
// SY.timer keeps (TIMER_LIMIT_EXCEEDED); the code is TIMER_STORAGE_ERROR when it cannot count.
func checkPendingLimits(ctx context.Context, db *sql.DB, ownerL2 string) (string, error) {
	owned, err := countPendingTimersByOwner(ctx, db, ownerL2)
	if err != nil {
		return "TIMER_STORAGE_ERROR", err
	}
	if owned >= int64(maxPendingTimersPerOwner) {
		return "TIMER_LIMIT_EXCEEDED", fmt.Errorf("%s has %d pending timers, the most one owner may have", ownerL2, owned)
	}
	total, err := countPendingTimers(ctx, db)
	if err != nil {
		return "TIMER_STORAGE_ERROR", err
	}
	if total >= int64(maxPendingTimers) {
		return "TIMER_LIMIT_EXCEEDED", fmt.Errorf("the hive has %d pending timers, the most SY.timer keeps", total)
	}
	return "", nil
}

func (s *Service) localOrchestratorL2Name() string {
	_, hive, _ := strings.Cut(s.nodeName, "@")
	return fmt.Sprintf("SY.orchestrator@%s", hive)
}

func timerReferenceFromRequest(timerUUID, clientRef string) (timerReference, error) {
	ref := timerReference{
		TimerUUID: strings.TrimSpace(timerUUID),
		ClientRef: strings.TrimSpace(clientRef),
	}
	switch {
	case ref.TimerUUID != "":
		return ref, nil
	case ref.ClientRef != "":
		if len(ref.ClientRef) > 255 {
			return timerReference{}, fmt.Errorf("client_ref must be at most 255 characters")
		}
		return ref, nil
	default:
		return timerReference{}, fmt.Errorf("either timer_uuid or client_ref must be provided")
	}
}

func normalizeClientRef(clientRef *string) (string, error) {
	if clientRef == nil {
		return "", nil
	}
	trimmed := strings.TrimSpace(*clientRef)
	if trimmed == "" {
		return "", nil
	}
	if len(trimmed) > 255 {
		return "", fmt.Errorf("client_ref must be at most 255 characters")
	}
	return trimmed, nil
}

func (s *Service) resolveTimerForGet(ctx context.Context, incoming fluxbeesdk.Message, ref timerReference) (*timerRow, error) {
	if ref.TimerUUID != "" {
		return getTimer(ctx, s.db, ref.TimerUUID)
	}
	ownerL2, err := s.resolveSourceL2NameFromMsg(incoming)
	if err != nil {
		return nil, err
	}
	return findPendingTimerByClientRef(ctx, s.db, ownerL2, ref.ClientRef)
}

func (s *Service) resolveOwnedTimerForMutation(ctx context.Context, incoming fluxbeesdk.Message, ref timerReference) (*timerRow, bool, error) {
	ownerL2, err := s.resolveSourceL2NameFromMsg(incoming)
	if err != nil {
		return nil, ref.TimerUUID != "", err
	}
	var row *timerRow
	if ref.TimerUUID != "" {
		row, err = getTimer(ctx, s.db, ref.TimerUUID)
	} else {
		row, err = findPendingTimerByClientRef(ctx, s.db, ownerL2, ref.ClientRef)
	}
	if err != nil {
		return nil, ref.TimerUUID != "", err
	}
	if row.OwnerL2Name != ownerL2 {
		return nil, ref.TimerUUID != "", timerErrorf("TIMER_NOT_OWNER", "timer does not belong to requester")
	}
	return row, ref.TimerUUID != "", nil
}

// lockOwnedTimerForMutation resolves the requester's timer as resolveOwnedTimerForMutation does,
// takes its lock, and reads it again under it (FINDINGS A-220's review). A fire holds that lock from
// its read of the row to its save: a cancel or a reschedule that lands meanwhile waits, and then
// sees what the fire wrote, instead of being written over by it. unlock releases the lock; it is
// never nil, and holds nothing when err is set.
func (s *Service) lockOwnedTimerForMutation(ctx context.Context, incoming fluxbeesdk.Message, ref timerReference) (row *timerRow, foundByUUID bool, unlock func(), err error) {
	row, foundByUUID, err = s.resolveOwnedTimerForMutation(ctx, incoming, ref)
	if err != nil {
		return nil, foundByUUID, func() {}, err
	}
	unlock = s.lockTimer(row.UUID)
	if row, err = getTimer(ctx, s.db, row.UUID); err != nil {
		unlock()
		return nil, foundByUUID, func() {}, err
	}
	return row, foundByUUID, unlock, nil
}

// lockTimer takes a timer's lock, the one its fires take (timerScheduler.lockTimer), and returns what
// releases it. With no scheduler (some tests) no fire runs, and there is nothing to take.
func (s *Service) lockTimer(timerUUID string) func() {
	if s.scheduler == nil {
		return func() {}
	}
	return s.scheduler.lockTimer(timerUUID)
}

func (s *Service) sendTimerScheduleExistingResponse(incoming fluxbeesdk.Message, verb string, row timerRow, alreadyExisted bool) error {
	return s.sendTimerResponse(incoming, map[string]any{
		"ok":              true,
		"verb":            verb,
		"timer_uuid":      row.UUID,
		"fire_at_utc_ms":  row.FireAtUTC,
		"already_existed": alreadyExisted,
	})
}

func scheduleFireAt(req timerScheduleRequest) (int64, error) {
	return oneShotFireAt(req.FireAtUTCMS, req.FireInMS, "fire_at_utc_ms", "fire_in_ms")
}

// oneShotFireAt is the instant a one-shot fires, from exactly one of an absolute instant or a delay:
// at least MinTimerDuration ahead, and at most maxTimerDelay.
func oneShotFireAt(atUTCMS, inMS *int64, atField, inField string) (int64, error) {
	if (atUTCMS != nil) == (inMS != nil) {
		return 0, timerErrorf("TIMER_INVALID_PAYLOAD", "exactly one of %s or %s must be set", atField, inField)
	}
	nowUTCMS := time.Now().UTC().UnixMilli()
	fireAt := nowUTCMS
	if atUTCMS != nil {
		fireAt = *atUTCMS
	} else {
		if *inMS > maxTimerDelay.Milliseconds() {
			return 0, errAboveMaximum()
		}
		fireAt += *inMS
	}
	switch ahead := fireAt - nowUTCMS; {
	case ahead < fluxbeesdk.MinTimerDuration.Milliseconds():
		return 0, errBelowMinimum()
	case ahead > maxTimerDelay.Milliseconds():
		return 0, errAboveMaximum()
	}
	return fireAt, nil
}

func scheduleRecurringFireAt(req timerScheduleRecurringRequest) (int64, string, error) {
	cronSpec := strings.TrimSpace(req.CronSpec)
	if cronSpec == "" {
		return 0, "", timerErrorf("TIMER_INVALID_PAYLOAD", "cron_spec must be non-empty")
	}
	cronTZ := "UTC"
	if strings.TrimSpace(req.CronTZ) != "" {
		cronTZ = strings.TrimSpace(req.CronTZ)
	}
	loc, err := loadLocation(cronTZ)
	if err != nil {
		return 0, "", timerErrorf("TIMER_INVALID_TIMEZONE", "invalid timezone: %w", err)
	}
	schedule, err := parseCronSpec(cronSpec)
	if err != nil {
		return 0, "", err
	}
	now := time.Now().UTC()
	first := schedule.Next(now.In(loc))
	if first.IsZero() {
		return 0, "", timerErrorf("TIMER_INVALID_CRON", "invalid cron: no future fire time")
	}
	next := schedule.Next(first.Add(time.Second).In(loc))
	if !next.IsZero() && next.Sub(first) < fluxbeesdk.MinTimerDuration {
		return 0, "", errBelowMinimum()
	}
	return first.UTC().UnixMilli(), cronTZ, nil
}

func rescheduleFireAt(req timerRescheduleRequest) (int64, error) {
	return oneShotFireAt(req.NewFireAtUTCMS, req.NewFireInMS, "new_fire_at_utc_ms", "new_fire_in_ms")
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
	return timerErrorf("TIMER_BELOW_MINIMUM", "minimum timer duration is %d seconds", int(fluxbeesdk.MinTimerDuration.Seconds()))
}

func errAboveMaximum() error {
	return timerErrorf("TIMER_ABOVE_MAXIMUM", "maximum timer delay is %d days", int(maxTimerDelay.Hours()/24))
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
