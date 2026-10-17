package node

import (
	"context"
	"fmt"
	"log"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

const (
	timeRFC3339 = time.RFC3339
	lockPath    = "/var/run/fluxbee/sy-wf-rules.lock"
)

type sender interface {
	Send(fluxbeesdk.Message) error
	UUID() string
	FullName() string
}

type Service struct {
	cfg        NodeConfig
	store      *Store
	sender     sender
	dispatcher *fluxbeesdk.RouterDispatcher
	incoming   <-chan fluxbeesdk.Message
	admin      adminClient
	nodes      nodeClient
	wfNodes    wfNodeClient
	clock      ClockFunc
}

func NewService(
	cfg NodeConfig,
	dispatcher *fluxbeesdk.RouterDispatcher,
	incoming <-chan fluxbeesdk.Message,
	clock ClockFunc,
) *Service {
	if clock == nil {
		clock = func() time.Time { return time.Now().UTC() }
	}
	var snd sender
	if dispatcher != nil {
		snd = dispatcher.SenderSnapshot()
	}
	return &Service{
		cfg:        cfg,
		store:      NewStore(cfg.StateDir),
		sender:     snd,
		dispatcher: dispatcher,
		incoming:   incoming,
		admin:      newAdminClient(cfg, dispatcher),
		nodes:      newNodeClient(dispatcher, cfg.HiveID),
		wfNodes:    newWFNodeClient(dispatcher),
		clock:      clock,
	}
}

func Run(runtimeCfg RuntimeConfig) error {
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()

	if err := os.MkdirAll("/var/run/fluxbee", 0o755); err != nil {
		return fmt.Errorf("create lock dir: %w", err)
	}
	lockFile, err := os.OpenFile(lockPath, os.O_CREATE|os.O_RDWR, 0o644)
	if err != nil {
		return fmt.Errorf("open lock file: %w", err)
	}
	defer lockFile.Close()
	if err := syscall.Flock(int(lockFile.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		return fmt.Errorf("sy-wf-rules already running (lock held): %w", err)
	}

	hiveID, err := fluxbeesdk.LoadHiveID(runtimeCfg.ConfigDir)
	if err != nil {
		return fmt.Errorf("load hive id: %w", err)
	}
	selfIlkID, err := fluxbeesdk.WaitForSelfSystemIlkID(
		hiveID,
		runtimeCfg.NodeBaseName,
		30*time.Second,
		250*time.Millisecond,
	)
	if err != nil {
		return fmt.Errorf("resolve self system ILK: %w", err)
	}
	log.Printf("resolved self system ILK from identity SHM: %s", selfIlkID)
	_ = selfIlkID // cached for future outgoing meta.src_ilk use

	profile, err := fluxbeesdk.NewOperationalRouteProfile().
		CommandChannel("incoming").
		PostPendingRule(fluxbeesdk.RouteAny{}, fluxbeesdk.RouteCommand{Channel: "incoming"}).
		Build()
	if err != nil {
		return fmt.Errorf("sy-wf-rules rpc profile invalid: %w", err)
	}
	dispatcher, err := fluxbeesdk.ConnectWithRetry(fluxbeesdk.NodeConfig{
		Name:               runtimeCfg.NodeBaseName,
		RouterSocket:       runtimeCfg.RouterSocketDir,
		UUIDPersistenceDir: runtimeCfg.UUIDPersistenceDir,
		UUIDMode:           fluxbeesdk.NodeUuidPersistent,
		ConfigDir:          runtimeCfg.ConfigDir,
		Version:            "0.1.0",
	}, time.Second, profile)
	if err != nil {
		return err
	}
	defer func() { _ = dispatcher.Close() }()
	sender := dispatcher.SenderSnapshot()
	incoming, err := dispatcher.TakeCommandReceiver("incoming")
	if err != nil {
		return fmt.Errorf("take incoming receiver: %w", err)
	}
	cfg, err := BuildNodeConfig(sender.FullName(), runtimeCfg.StateDir, runtimeCfg.DistRuntimeRoot)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(cfg.StateDir, 0o755); err != nil {
		return err
	}
	if err := os.MkdirAll(cfg.DistRuntimeRoot, 0o755); err != nil {
		return err
	}
	_ = sender
	return NewService(cfg, dispatcher, incoming, nil).RunWithContext(ctx)
}

func (s *Service) Run() error {
	return s.RunWithContext(context.Background())
}

func (s *Service) RunWithContext(ctx context.Context) error {
	if s.incoming == nil {
		return fmt.Errorf("incoming channel is required")
	}
	for {
		select {
		case <-ctx.Done():
			return nil
		case msg, ok := <-s.incoming:
			if !ok {
				return nil
			}
			s.handleMessage(msg)
		}
	}
}

func (s *Service) handleMessage(msg fluxbeesdk.Message) {
	switch msg.Meta.MsgType {
	case fluxbeesdk.SYSTEMKind:
		s.handleSystemMessage(msg)
	case "command":
		if !fromAdmin(msg) {
			s.sendCommandResponse(msg, stringValue(msg.Meta.Action), notFromAdmin(msg))
			return
		}
		s.handleCommand(msg)
	case "query":
		if !fromAdmin(msg) {
			s.sendQueryResponse(msg, stringValue(msg.Meta.Action), notFromAdmin(msg))
			return
		}
		s.handleQuery(msg)
	}
}

// fromAdmin reports whether a request comes from SY.admin. Workflow operations are user actions,
// and every user action goes through the admin; the router stamps the sender, so no other node can
// claim to be it. Without this, any node could have SY.wf-rules deploy or delete WF nodes, which it
// does through the admin under its own authority (A-68).
func fromAdmin(msg fluxbeesdk.Message) bool {
	return msg.SourceL2Name() == fluxbeesdk.PrimaryAdminNode
}

func notFromAdmin(msg fluxbeesdk.Message) map[string]any {
	return map[string]any{
		"status":       "error",
		"error_code":   "UNAUTHORIZED",
		"error_detail": fmt.Sprintf("SY.wf-rules takes workflow operations from %s only, not %q", fluxbeesdk.PrimaryAdminNode, msg.SourceL2Name()),
	}
}

func (s *Service) nodeName() string {
	if s.sender != nil && strings.TrimSpace(s.sender.FullName()) != "" {
		return s.sender.FullName()
	}
	return s.cfg.NodeName
}
