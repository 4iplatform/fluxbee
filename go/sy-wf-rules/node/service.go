package node

import (
	"context"
	"fmt"
	"os"
	"strings"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

const timeRFC3339 = time.RFC3339

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
	orchestrator orchestratorClient
	clock        ClockFunc
}

func NewService(cfg NodeConfig, snd sender, rcv receiver, clock ClockFunc) *Service {
	if clock == nil {
		clock = func() time.Time { return time.Now().UTC() }
	}
	return &Service{
		cfg:          cfg,
		store:        NewStore(cfg.StateDir),
		sender:       snd,
		receiver:     rcv,
		orchestrator: newOrchestratorClient(snd, rcv),
		clock:        clock,
	}
}

func Run(runtimeCfg RuntimeConfig) error {
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
	return NewService(cfg, sender, receiver, nil).Run()
}

func (s *Service) Run() error {
	if s.receiver == nil {
		return fmt.Errorf("receiver is required")
	}
	for {
		msg, err := s.receiver.Recv(context.Background())
		if err != nil {
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
