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
	_ "modernc.org/sqlite"
)

const (
	version             = "1.0"
	defaultNodeBaseName = "SY.timer"
	configDir           = "/etc/fluxbee"
	nodesDir            = "/var/lib/fluxbee/state/nodes"
	routerSockDir       = "/var/run/fluxbee/routers"
	nodeRootDir         = "/var/lib/fluxbee/nodes"
)

type Service struct {
	sender      *fluxbeesdk.NodeSender
	receiver    *fluxbeesdk.NodeReceiver
	nodeName    string
	instanceDir string
	dbPath      string
	db          *sql.DB
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

func main() {
	log.SetFlags(log.LstdFlags | log.Lmicroseconds)

	sender, receiver, err := fluxbeesdk.Connect(fluxbeesdk.NodeConfig{
		Name:               defaultNodeBaseName,
		RouterSocket:       routerSockDir,
		UUIDPersistenceDir: nodesDir,
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
		sender:      sender,
		receiver:    receiver,
		nodeName:    nodeName,
		instanceDir: instanceDir,
		dbPath:      dbPath,
		db:          db,
	}
	log.Printf("SY.timer ready: node=%s db=%s", service.nodeName, service.dbPath)
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
			{Verb: "TIMER_NOW", Description: "Return current UTC time."},
			{Verb: "TIMER_NOW_IN", Description: "Return current time in a requested IANA timezone.", RequiredFields: []string{"tz"}},
			{Verb: "TIMER_CONVERT", Description: "Convert a UTC instant into a target timezone.", RequiredFields: []string{"instant_utc_ms", "to_tz"}},
			{Verb: "TIMER_PARSE", Description: "Parse a date/time string using a Go layout and timezone.", RequiredFields: []string{"input", "layout", "tz"}},
			{Verb: "TIMER_FORMAT", Description: "Format a UTC instant using a Go layout and timezone.", RequiredFields: []string{"instant_utc_ms", "layout", "tz"}},
			{Verb: "TIMER_HELP", Description: "Describe the currently implemented SY.timer capabilities."},
		},
		[]fluxbeesdk.HelpErrorDescriptor{
			{Code: "TIMER_INVALID_PAYLOAD", Description: "Request payload is malformed or missing required fields."},
			{Code: "TIMER_INVALID_TIMEZONE", Description: "Requested timezone is not a valid IANA timezone."},
			{Code: "TIMER_INTERNAL", Description: "Verb is not implemented yet or an internal error occurred."},
		},
	)
	return s.sendTimerResponse(incoming, payload)
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

func openTimerDB(path string) (*sql.DB, error) {
	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, err
	}
	if _, err := db.Exec("PRAGMA journal_mode=WAL;"); err != nil {
		_ = db.Close()
		return nil, err
	}
	if _, err := db.Exec("PRAGMA busy_timeout=5000;"); err != nil {
		_ = db.Close()
		return nil, err
	}
	return db, nil
}

func managedNodeInstanceDir(nodeName string) (string, error) {
	nodeName = strings.TrimSpace(nodeName)
	if nodeName == "" {
		return "", fmt.Errorf("node_name must be non-empty")
	}
	localName, _, ok := strings.Cut(nodeName, "@")
	if !ok || strings.TrimSpace(localName) == "" {
		return "", fmt.Errorf("node_name must include hive suffix")
	}
	kind, _, ok := strings.Cut(localName, ".")
	if !ok || strings.TrimSpace(kind) == "" {
		return "", fmt.Errorf("node_name must include kind prefix")
	}
	return filepath.Join(nodeRootDir, kind, nodeName), nil
}

func loadLocation(raw string) (*time.Location, error) {
	if strings.TrimSpace(raw) == "" {
		return nil, fmt.Errorf("timezone must be non-empty")
	}
	return time.LoadLocation(raw)
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
