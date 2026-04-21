package node

import (
	"context"
	"fmt"
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

type receiver interface {
	Recv(context.Context) (fluxbeesdk.Message, error)
}

type Service struct {
	cfg          NodeConfig
	store        *Store
	sender       sender
	receiver     receiver
	mux          *messageMux
	admin        adminClient
	orchestrator orchestratorClient
	wfNodes      wfNodeClient
	clock        ClockFunc
}

func NewService(cfg NodeConfig, snd sender, rcv receiver, clock ClockFunc) *Service {
	if clock == nil {
		clock = func() time.Time { return time.Now().UTC() }
	}
	mux := newMessageMux(rcv)
	return &Service{
		cfg:          cfg,
		store:        NewStore(cfg.StateDir),
		sender:       snd,
		receiver:     rcv,
		mux:          mux,
		admin:        newAdminClient(cfg, snd, mux),
		orchestrator: newOrchestratorClient(snd, mux),
		wfNodes:      newWFNodeClient(snd, mux),
		clock:        clock,
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

	sender, receiver, err := fluxbeesdk.Connect(fluxbeesdk.NodeConfig{
		Name:               runtimeCfg.NodeBaseName,
		RouterSocket:       runtimeCfg.RouterSocketDir,
		UUIDPersistenceDir: runtimeCfg.UUIDPersistenceDir,
		UUIDMode:           fluxbeesdk.NodeUuidPersistent,
		ConfigDir:          runtimeCfg.ConfigDir,
		Version:            "0.1.0",
	})
	if err != nil {
		return err
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
	return NewService(cfg, sender, receiver, nil).RunWithContext(ctx)
}

func (s *Service) Run() error {
	return s.RunWithContext(context.Background())
}

func (s *Service) RunWithContext(ctx context.Context) error {
	if s.mux == nil {
		return fmt.Errorf("message mux is required")
	}
	go s.mux.Run(ctx)
	for {
		msg, err := s.mux.NextMessage(ctx)
		if err != nil {
			if ctx.Err() != nil {
				return nil
			}
			return err
		}
		s.handleMessage(msg)
	}
}

func (s *Service) handleMessage(msg fluxbeesdk.Message) {
	switch msg.Meta.MsgType {
	case fluxbeesdk.SYSTEMKind:
		s.handleSystemMessage(msg)
	case "command":
		s.handleCommand(msg)
	case "query":
		s.handleQuery(msg)
	}
}

func (s *Service) nodeName() string {
	if s.sender != nil && strings.TrimSpace(s.sender.FullName()) != "" {
		return s.sender.FullName()
	}
	return s.cfg.NodeName
}
