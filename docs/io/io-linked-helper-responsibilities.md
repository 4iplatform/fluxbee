# Technical specification — Responsibilities of the `IO.linkedhelper` node
## Adapter, node and core

> Document updated to reflect the current split of responsibilities of the `IO.linkedhelper` node.

---

# 1. Scope

This document only covers:

- responsibilities per component,
- system invariants,
- the limits of each layer,
- relevant black boxes,
- and new responsibilities that come from ICH/config/pending.

It does not cover in detail:

- the final HTTP protocol;
- the final shape of payloads;
- full wire contracts.

---

# 2. Guiding principle

The Linked Helper integration is organized in three layers:

- **adapter**: observes and acts on Linked Helper;
- **`IO.linkedhelper` node**: coordinates the channel's minimal state and translates toward Fluxbee;
- **Fluxbee/core**: resolves the canonical identity, configuration and the system's internal logic.

The central idea is that:

- Linked Helper stays encapsulated behind the adapter,
- the node does not become a general operational backend,
- and Fluxbee/core remains the source of truth for identity and configuration.

---

# 3. Responsibilities of the adapter

## 3.1. General role

The adapter is the component that:

- is installed on the PC where Linked Helper runs;
- accesses LH's technical world;
- monitors available folders/instances;
- detects new profiles and relevant events;
- communicates with `IO.linkedhelper`;
- and executes in LH the actions Fluxbee returns to it.

## 3.2. Clear responsibilities

### A. Automatic discovery of instances and profiles
It must be able to:

- monitor the Linked Helper folders/instances;
- detect new instances automatically;
- detect new available profiles.

### B. Communication with the node
It must:

- always start the communication with the node;
- send events;
- send a beacon/heartbeat when it has no events;
- receive results and configuration changes in the response.

### C. Executing actions on LH
It must carry out in Linked Helper actions such as:

- sending messages;
- updating pending messages;
- deleting/cancelling pending messages;
- other future operational actions on LH.

### D. Operational retry/alert policy
On an error result, it is up to the adapter to decide:

- whether to retry;
- when to retry;
- how many times to retry;
- when to alert.

### E. Building canonical external ids
It must build and send to the node the canonical external ids the channel needs to guarantee uniqueness.

In particular:

- `external_profile_id` must arrive already resolved by the adapter, as the profile's canonical identifier;
- if, to achieve uniqueness, the adapter needs to compose it from `adapter_id`, the Linked Helper instance or other components, that composition belongs to the adapter;
- the node should not depend on knowing or rebuilding that internal composition.

## 3.3. Clear restrictions

### A. It does not own the profile's final identity
It can discover and report a profile, but it does not define its final identity inside Fluxbee.

### B. It does not report messages or automation for a profile without a final ILK
When it detects a new profile:

- it reports it;
- but it does not report messages or start automation for that profile until it receives the usable ILK.

### C. It does not receive or use provisional ILKs
The adapter must not receive the profile's provisional ILK.

### D. Automation is governed by the LH channel's ICH
It must react to enablement/disablement per Linked Helper ICH, not per global ILK.

Important interpretation:
- that `ICH` is a channel/asset **owned by the Fluxbee system**, operated by the IO/adapter instance for Linked Helper;
- it does not represent the remote external contact.

### E. Auto-detected LH ICHs are born disabled
By default, when an LH profile/channel is auto-detected, that ICH's automation is born disabled.

## 3.4. Operational effect of disabled automation

When the adapter receives that a profile's LH ICH has automation disabled:

- it stops looking for/reporting messages for AI for that profile;
- but it may keep reporting states, alerts and non-conversational events.

---

# 4. Responsibilities of the `IO.linkedhelper` node

## 4.1. General role

The `IO.linkedhelper` node:

- receives events from the adapter;
- operates for a single `managed_instance_id` per node instance in the current intermediate path;
- creates or resolves provisional identities when needed;
- coordinates the channel's minimal needed state;
- translates to the Fluxbee model;
- observes relevant identity changes;
- keeps a result queue per adapter;
- and returns results and configuration information to the adapter.

### Operational note on lifecycle

`IO.linkedhelper` can stay alive for control-plane and observability even when it does not yet have enough material to operate a concrete LH profile/channel.

In particular:
- it can be alive with no ready profiles;
- it can be alive with profiles discovered but not promoted;
- it can be alive while it observes the states of its own ICHs.

But while a profile does not have enough identity/state, or its own ICH is not operational, the node must not enable real conversational traffic for that profile.

## 4.2. Clear responsibilities

### A. Reception and initial validation
Receive and minimally validate the events sent by the adapter.

Today this includes:

- `adapter_id` from the header and from the body;
- `managed_instance_id` from the body;
- `local_instance_id` when the node's binding knows it;
- and the adapter's bearer secret.

The secret must not live inline in config; the node resolves it from Vault and caches it in memory.

### B. Creating a provisional ILK for new profiles
When it detects a new profile:

- it registers an ILK of type `agent`;
- in a state other than `complete`;
- with the node's host/default tenant.

### C. Do not expose provisional ILKs to the adapter
The node must not send that provisional ILK to the adapter.

### D. Keep a list of provisional ILKs and track them
It must keep a list of the LH channel's provisional ILKs and track their state.

### E. Detect the ILK promotion by observing the identity SHM
It must detect that the ILK is ready by observing identity's shared memory.

### F. Return the final ILK to the adapter
When it detects that the ILK is ready, it returns it to the adapter through the beacon/poll.

### G. Result queue per adapter
It must keep a queue or set of pending results per adapter.

In the current state of the repo:

- the node already persists `pending_deliveries` per adapter;
- it drains that queue during poll/heartbeat;
- and it collapses state changes per `ich_id`.

### H. Gating per profile
It must prevent a profile that is not ready from moving on to automation.
The block is per profile; one profile does not block another.

### I. Configuration changes per ICH
It must be able to return to the adapter changes such as:
- ILK ready;
- LH automation enabled/disabled per ICH;
- other future configuration changes.

### J. Record of pending changes per ICH
Besides monitoring pending ILKs, the node must keep a record of the pending changes per ICH that must be reported to the adapter.

### K. Collapsing to the last state per ICH
If there is already a pending change for an ICH and then a new change of the same ICH happens, the last relevant state must prevail instead of sending both, unless another scheme with versioning/timestamps is explicitly defined in the future.

## 4.2.1. Current minimal node configuration

In the current intermediate path, the node must be considered configured with:

- `managed_instance_id`
- `tenant_id`
- `listen.address`
- `listen.port`
- `adapter.adapter_id`
- `adapter.local_instance_id` optional
- `adapter.auth = { type: vault_ref, resource_type: linkedhelper_adapter, key: ... }`
- `mode = direct_http_intermediate`

Consequences:

- the node is 1:1 with an approved instance;
- it does not accept several adapters configured in parallel;
- and it stays non-operational if it cannot resolve the adapter's secret from Vault.

## 4.3. Legitimate monitoring by the node

The node should indeed monitor:

- the state of provisional/ready profiles;
- the state of its own ICHs;
- automation changes per ICH;
- pending results per adapter;
- identity changes needed to release the channel.

### Important
This monitoring of the states of its own ICHs is not a Linked Helper-specific oddity.
It is considered a responsibility that generalizes to IO nodes: each IO node should be able to observe the states of its own ICHs and react operationally to relevant changes.

Important interpretation:
- the ICHs monitored by the node are **local/systemic** ICHs of the LH channel;
- external contacts and their handles are resolved through identity/contact mechanisms, not as ICHs.

## 4.4. What the node should not absorb

It should not centralize:

- sophisticated retry policies;
- complex operational alerting;
- configurable long-running retries;
- general recovery workflows;
- business decisions after an error;
- administrative exposure of enabled/disabled ICHs or pending ILKs, when that responsibility sits on the identity/core side.

## 4.5. Minimal durable persistence

The node should durably persist, at a minimum:

- adapter/managed_instance ↔ profile ↔ ILK/ICH mappings;
- pending provisional ILKs;
- the last observed automation state per ICH for LH;
- changes pending delivery to the adapter;
- the result queue per adapter, or a way to rebuild it.

This is needed not only to avoid losing track, but to always be able to answer the right adapter.

---

# 5. Responsibilities of Fluxbee / core / internal services

## 5.1. General role

Fluxbee/core is responsible for:

- holding the source of truth for identity;
- completing the profile's missing data;
- promoting provisional ILKs to complete/usable;
- managing configuration per ICH;
- and running the system's routing/AI/workflows.

## 5.2. Clear responsibilities

### A. Source of truth for identity
The ILKs and the canonical truth live inside Fluxbee/core.

### B. Complete the profile's missing data
It must complete or confirm, as appropriate:

- the represented tenant;
- business email;
- language;
- time zone;
- role/capabilities;
- future prompting/configuration.

### C. Promotion of the profile ILK
It must promote the ILK from the provisional state to complete/usable.

### D. Configuration per ICH
It must allow enabling/disabling automation per ICH.

### E. Administrative exposure
The administrative exposure of:
- enabled/disabled ICHs
- pending ILKs
- and other identity states

is no longer considered a responsibility of the `IO.linkedhelper` node, but of identity/core and the mechanisms that team defines.

---

# 6. Clear invariants

1. A profile can exist inside Fluxbee in a provisional state.
2. The adapter does not receive or use the provisional ILK.
3. The adapter does not automate until it receives confirmation that the profile is ready.
4. Automation is governed by the LH channel's ICH.
5. Auto-detected LH ICHs are born with automation disabled.
6. One profile does not block another.
7. The node keeps the result queue per adapter.
8. The node detects the profile promotion by observing the identity SHM.
9. The host tenant is not necessarily the represented tenant.
10. Pending changes per ICH must reach the right adapter.
11. For ICH changes, the last relevant state must prevail.

---

# 7. Black boxes / real open items

## 7.1. Profile promotion
Not defined:

- which exact component triggers it;
- how the minimal data set is completed;
- what human intervention happens exactly;
- how the represented tenant is resolved internally.

## 7.2. `get-config` / `set-config`
Still open:

- whether they are finally used, and for which of the node's own config;
- how they are extended without distorting the current model;
- and what their final shape will be.

## 7.3. SDK
It is not clear whether the SDK will have to change to support these extra states/data.

---

# 8. Summary

## Adapter
Sees and executes:
- discovers;
- reports;
- polls;
- executes in LH;
- decides retries/alerts.

## Node
Coordinates and translates:
- registers provisional ILKs;
- observes identity;
- monitors pending ILKs and the states of its own ICHs;
- keeps the queue;
- releases/blocks per profile;
- records pending changes per ICH;
- returns results/config to the right adapter.

## Core
Completes and promotes:
- maintains identity;
- completes data;
- promotes ILKs;
- governs configuration per ICH;
- exposes the ICH/ILK state administratively through its own mechanisms;
- runs routing/AI/workflows.
