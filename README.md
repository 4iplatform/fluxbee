# Fluxbee

**Distributed intelligence infrastructure**

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

> *fluxbee.ai*

---z

## What is this?

Fluxbee is infrastructure for building systems where AI agents, humans, and automated workflows communicate seamlessly across any channel. It's the nervous system for organizations that want to operate with AI at the center, not at the edges.

Think of it as a phone system, but instead of connecting phone numbers, it connects:
- AI agents with specialized knowledge
- Human operators and customers
- Workflows and integrations
- Any communication channel (WhatsApp, Email, Slack, etc.)

Every message flows through a unified routing layer that knows who's talking, who they're talking to, what they're capable of, and where to send the conversation next.

---

## Why does this exist?

**The problem:** Today's AI integrations are point-to-point. You connect an AI to WhatsApp. Another to your CRM. Another to email. Each one is an hive. They don't share context. They can't hand off conversations. They can't be managed as a coherent system.

**The insight:** What if we treated AI agents the way organizations treat employees? Each one has a role, capabilities, and credentials. They're trained (prompted) for specific jobs. They work together, escalate to each other, and when they can't handle something, they bring in a human.

**The solution:** A routing layer that understands identity (who), capability (what they can do), and conversation flow (where things go next). Built for AI-first but works just as well for humans.

---

## Core Concepts

### Three Layers of Routing

| Layer | What it routes | Example |
|-------|----------------|---------|
| **L1 - Connection** | Raw sockets | Which process on which machine |
| **L2 - Node** | Named services | `AI.support.l1@production` |
| **L3 - Interlocutor** | Identities | Customer "John" or Agent "Support-L1" |

Most systems only have L1. Some have L2. Fluxbee has all three, which means you can route based on *who someone is* and *what they need*, not just where the bytes go.

### Hives

An **hive** is a deployment unit - a cluster of nodes that share memory and communicate via Unix sockets. Fast, local, zero serialization overhead.

Hives connect to each other over the network. A customer in São Paulo talks to an AI agent in the São Paulo hive. If that agent needs to escalate, the message routes to the Buenos Aires hive where the senior agents live. The customer doesn't know. The routing is automatic.

### The Identity System (ILK)

Every participant in the system has an **ILK** (Interlocutor Key) - a unique identifier that follows them everywhere:

```
ilk:550e8400-e29b-41d4-a716-446655440000
```

ILKs have types:
- **Tenant** - An organization (for billing, isolation, contracts)
- **Agent** - An AI with a specific degree (training)
- **Human/Internal** - An operator who can see inside the system
- **Human/External** - A customer who interacts from outside

The routing layer uses ILKs to make decisions: this customer belongs to this tenant, should talk to agents with these capabilities, and if things go wrong, escalate to this human.

### The University Model

AI agents don't just exist - they **graduate**.

1. **Modules** are fragments of knowledge: "You speak Spanish", "You know our product catalog", "You escalate after 3 failed attempts"

2. **Degrees** combine modules into a complete training: "Support-L1-Spanish" = Spanish + Product Knowledge + Basic Troubleshooting + Escalation Rules

3. **Graduation** assigns a degree to an agent with a cryptographic seal. The agent cannot operate without a valid degree. If someone tampers with the training, the hash breaks, and the agent refuses to run.

This means:
- You can audit exactly what an agent knows
- You can version and roll back training
- You can't accidentally deploy an untrained agent
- The AI manages the AI (humans write modules, but compilation and verification is automatic)

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                         Mother Hive                            │
│                                                                  │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐          │
│  │ PostgreSQL   │  │ SY.identity  │  │ SY.admin     │          │
│  │ (source of   │  │ (graduates   │  │ (human       │          │
│  │  truth)      │  │  agents)     │  │  interface)  │          │
│  └──────────────┘  └──────────────┘  └──────────────┘          │
│                                                                  │
└──────────────────────────┬──────────────────────────────────────┘
                           │
                           │ WAN (broadcast replication)
                           │
┌──────────────────────────┼──────────────────────────────────────┐
│                          ▼                                       │
│                    Production Hive                             │
│                                                                  │
│  ┌─────────┐    ┌─────────┐    ┌─────────┐    ┌─────────┐      │
│  │ Router  │    │ Router  │    │ Gateway │    │ SY.*    │      │
│  │ (RT)    │    │ (RT)    │    │ (to WAN)│    │ (system)│      │
│  └────┬────┘    └────┬────┘    └─────────┘    └─────────┘      │
│       │              │                                          │
│       │   Unix Sockets (fast, local)                           │
│       │              │                                          │
│  ┌────┴────┐    ┌────┴────┐    ┌─────────┐    ┌─────────┐      │
│  │ AI.     │    │ AI.     │    │ IO.     │    │ WF.     │      │
│  │ support │    │ sales   │    │ whatsapp│    │ crm     │      │
│  │ (agent) │    │ (agent) │    │ (edge)  │    │ (flow)  │      │
│  └─────────┘    └─────────┘    └─────────┘    └─────────┘      │
│                                      │                          │
└──────────────────────────────────────┼──────────────────────────┘
                                       │
                                       │ HTTPS (WhatsApp API)
                                       │
                                  ┌────┴────┐
                                  │ Customer│
                                  │ (phone) │
                                  └─────────┘
```

### Node Types

| Prefix | Purpose | Examples |
|--------|---------|----------|
| **RT** | Router - moves messages | `RT.main@production` |
| **SY** | System - configuration, identity, admin | `SY.identity@mother` |
| **IO** | Edge - connects to external channels | `IO.whatsapp@production` |
| **AI** | Agent - processes conversations | `AI.support.l1@production` |
| **WF** | Workflow - orchestrates processes | `WF.onboarding@production` |

### Shared Memory

Within an hive, nodes communicate through shared memory regions:
- **Node table** - Who's connected right now
- **Config** - Routes and VPNs
- **Identity** - ILKs, degrees, capabilities
- **OPA** - Compiled routing policies

No serialization. No network calls. Just memory reads. This is why it's fast.

### OPA Policies

Routing decisions are made by [OPA](https://www.openpolicyagent.org/) (Open Policy Agent). You write rules like:

```rego
# Route to agent with required capability
target = node {
    required := input.meta.context.required_capability
    some ilk
    data.identity[ilk].type == "agent"
    required in data.identity[ilk].capabilities
    node := data.identity[ilk].handler_node
}

# Only internal humans can see system status
allow {
    input.meta.action == "system_status"
    data.identity[input.meta.src_ilk].human_subtype == "internal"
}
```

Policies are compiled to WASM and distributed to all hives. Changes propagate in seconds.

---

## What can you build with this?

### Multi-channel Customer Support
- Customer writes on WhatsApp → AI agent responds
- Same customer emails later → Same context, same agent knowledge
- Agent can't solve it → Escalates to senior AI → Escalates to human
- Human resolves → AI learns for next time

### AI-Native Sales Team
- Lead comes in → AI qualifier assesses fit
- Qualified → AI sales rep handles objections
- Ready to close → Senior AI or human closer takes over
- All in the same conversation thread, all with full context

### Operations Dashboard
- Internal operators (human/internal) can query system status
- See which agents are handling what
- Monitor escalation rates
- Adjust routing in real-time

### Multi-tenant SaaS
- Each tenant gets isolated agents, routing, and data
- Billing per tenant
- Custom training per tenant
- Shared infrastructure, separated concerns

---

## Design Principles

### 1. AI-Native, Human-Compatible
The system assumes AI is the primary operator. Humans are escalation points, not the main workforce. But when humans are needed, they have full visibility.

### 2. Identity is Everything
Every message carries who sent it, who it's for, and what conversation it belongs to. You can't lose context. You can't have orphan messages.

### 3. Verified Knowledge
Agents can't operate without valid credentials. Training is versioned, hashed, and auditable. No "oops, we deployed the wrong prompt."

### 4. Local Speed, Global Reach
Within an hive: shared memory, microsecond latency.
Between hives: async replication, eventual consistency.
Best of both worlds.

### 5. Policy-Driven Routing
Business rules live in OPA policies, not in code. Change who handles what without deploying code. Audit routing decisions after the fact.

### 6. The System Doesn't Self-Modify
Configuration comes from outside (admins, APIs). The system executes but doesn't decide its own rules. This is intentional. AI managing AI is powerful, but there's always a human-controlled layer at the top.

---

## Current Status

This is a working specification with partial implementation. The core router, shared memory regions, and node communication are functional. The identity system and university model are specified but not yet implemented.

### Implemented
- Core router with FIB and shared memory
- Node library with split sender/receiver model
- Inter-hive gateway communication
- OPA policy compilation and distribution
- Configuration broadcast and replication

### Specified, Not Yet Implemented
- SY.identity service
- Module/Degree/Graduation system
- IO nodes (WhatsApp, Email, etc.)
- AI agent framework
- Workflow engine

---

## Getting Started

See the [Technical Specification](./docs/) for complete details.

### Development Guide

This README explains the system and concepts. For how to run, build, and develop locally, see `DEVELOPMENT.md`.

### Node Development Template (Rust)

If you want to build a node in another repo, copy the client library and use it as a path dependency.

**What to copy**
```
json-router/crates/jsr_client/
```

**Suggested structure**
```
my-node/
├── Cargo.toml
├── src/
│   └── main.rs
└── jsr_client/        # copied from json-router/crates/jsr_client
```

**Cargo.toml**
```toml
[package]
name = "my-node"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1.37", features = ["full"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
uuid = { version = "1.7", features = ["v4"] }
jsr-client = { path = "./jsr_client" }
```

**Minimal node example**
```rust
use jsr_client::{connect, NodeConfig};
use jsr_client::protocol::{Destination, Message, Meta, Routing};
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = NodeConfig {
        name: "WF.test".to_string(),
        router_socket: "/var/run/json-router/routers".into(),
        uuid_persistence_dir: "/var/lib/json-router/state/nodes".into(),
        config_dir: "/etc/json-router".into(),
        version: "1.0".to_string(),
    };

    let (sender, mut receiver) = connect(&config).await?;

    let msg = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            dst: Destination::Broadcast,
            ttl: 16,
            trace_id: Uuid::new_v4().to_string(),
        },
        meta: Meta {
            msg_type: "user".to_string(),
            msg: Some("HELLO".to_string()),
            scope: None,
            target: None,
            action: None,
            priority: None,
            context: None,
        },
        payload: serde_json::json!({"hello":"world"}),
    };
    sender.send(msg).await?;

    loop {
        let msg = receiver.recv().await?;
        println!("received: {:?}", msg);
    }
}
```

Key documents:
- `01-arquitectura.md` - Architecture and concepts
- `02-protocolo.md` - Message protocol and node library
- `03-shm.md` - Shared memory structures
- `04-routing.md` - FIB, VPNs, OPA integration
- `10-identity-layer3.md` - Identity system and L3 routing

### Functional Specification (Docs)

The functional specification lives in `docs/`. There is no cross-navigation between files yet, so here is the full index:
- `01-arquitectura.md` - Architecture overview
- `02-protocolo.md` - Protocol and node library behavior
- `03-shm.md` - Shared memory regions and layout
- `04-routing.md` - Routing, FIB, VPNs, OPA integration
- `05-conectividad.md` - WAN connectivity and gateway behavior
- `06-regiones.md` - Config/LSA regions and update flows
- `07-operaciones.md` - Ops, deployment, and admin workflows
- `08-apendices.md` - Appendix and reference notes
- `09-router-status.md` - Router implementation status checklist
- `10-identity-layer3.md` - Identity system and L3 routing
- `SY_nodes_spec.md` - System nodes specification

---

## License

MIT License - see [LICENSE](LICENSE) for details.

---

## Contributing

Contributions welcome. Please read the technical specifications first.

---

**Fluxbee** — *Where AI agents work together*
