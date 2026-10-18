# Technical specification — `adapter ↔ IO.linkedhelper` protocol and mechanism

> Document updated to reflect the current mechanism of the `adapter ↔ IO.linkedhelper` channel.

---

# 1. Scope

This document covers:

- transport and minimal security;
- direction of the communication;
- polling/beacon;
- the result queue;
- batching;
- response families;
- configuration changes returned through the beacon;
- and the channel's minimal durable state.

It does not cover yet:

- the exact shape of all payloads;
- the final JSON schema;
- the final retries/timeouts policy;
- an alternative wire format;
- internal node ↔ router detail;
- or the final shape of `get-config` / `set-config`.

---

# 2. Transport and security

## 2.1. Transport
The `adapter ↔ IO.linkedhelper` channel is implemented over:

- HTTP
- protected by HTTPS

## 2.2. Reason for the choice
Ruled out, for now:

- TCP with a custom protocol;
- gRPC;
- protobuf.

Priority goes to:
- lower operational complexity;
- a simpler implementation;
- reasonable minimal security with a certificate.

## 2.3. Minimal authentication
In the current intermediate path, each adapter uses a unique **adapter secret**.

That secret:

- belongs to the adapter;
- is published in Vault;
- the node resolves it through `vault_ref` at startup or when applying config;
- and the adapter sends it as `Authorization: Bearer <adapter_secret>`.

## 2.4. Real security open items
Still open:
- local storage of the key;
- rotation/revocation;
- the exact HTTP authentication mechanism;
- possible additional future security.

---

# 3. Topology and initial deployment

## 3.1. Single node instance
In the current intermediate stage, the `IO.linkedhelper` node:

- is configured 1:1 with a single `managed_instance_id`;
- accepts a single adapter binding per instance;
- listens on a declarative address/port (`config.listen.address` + `config.listen.port`);
- and exposes temporary direct HTTP while there is no Edge.

## 3.2. Consequence
This simplifies:
- external routing to the node;
- coordination of the local state;
- initial persistence of the channel state;
- defensive validation of the `adapter_id ↔ managed_instance_id` binding;
- and returning results/config to the right adapter.

Canonical form of the current binding:

- `node_name = IO.linkedhelper.<managed_instance_id>@motherbee`
- `adapter.adapter_id = <adapter_id>`
- `adapter.local_instance_id = <local_instance_id>` optional
- `adapter.auth = { type: vault_ref, resource_type: linkedhelper_adapter, key: ... }`

---

# 4. Direction of the communication

## 4.1. The adapter always starts the communication
There are no webhooks and no direct push from Fluxbee to the adapter.

## 4.2. Polling / beacon
The adapter polls the node periodically.

In each exchange:
- if it has events, it sends them;
- if it has no events, it sends a heartbeat.

Current implemented state:

- the endpoint is `POST /v1/poll`;
- the `X-Fluxbee-Adapter-Id` header is mandatory;
- the body must include `adapter_id` and `managed_instance_id`;
- `local_instance_id` must also be sent when the node's binding knows it;
- the node accepts `mode = events | heartbeat`;
- and if `mode` is missing, the current runtime derives `heartbeat` when `items=[]` and `events` when there are items.

## 4.3. The heartbeat is not just a keepalive
It also serves to:

- pick up pending results;
- receive ILKs that are ready;
- receive configuration changes per ICH;
- keep the return channel alive.

---

# 5. Batch and result queue

## 5.1. Batch per request
A request may contain several events from:

- different profiles;
- different event types.

Note on the current state:

- the request may contain several events from different profiles;
- but they all belong to the same `managed_instance_id`, because the current node is 1:1 per approved instance;
- several simultaneous accounts inside the same node are outside the current model.

## 5.2. Node response
The response may return:

- `ack`
- `result`
- `heartbeat`

and mix pending elements from different previous events of the same adapter.

## 5.3. Queue per adapter
The node keeps a queue or set of pending results per adapter and delivers them in the next beacon/poll.

## 5.4. Deferred delivery
The responses to events sent in the same poll do not have to come back together.
They may arrive in later polls.

---

# 6. Nature of the events sent by the adapter

The items sent by the adapter are **events** toward Fluxbee, not already-resolved commands.

The adapter reports observed facts.
Fluxbee decides what to do with them according to:

- event type;
- metadata;
- system state;
- routing;
- internal services.

Examples:
- profile detected;
- conversational message;
- operational alert.

---

# 7. Message / event families that are current or on track

## 7.1. `heartbeat`
It is still a valid conceptual type at the mechanism level.

## 7.2. `profile_create`
Still current, but its semantics are now:

- report the discovery of the profile;
- let the node create the provisional ILK;
- without enabling automation in the adapter yet.

## 7.3. `conversation_message`
Still current, with a new restriction:
- it must not be emitted for a profile without a final/usable ILK and without LH automation enabled through its ICH.

## 7.4. `system_alert`
Still relevant for the channel's non-conversational states and alerts.

## 7.5. Future event: `profile_update`
It is recognized as a possible future event, but it is not part of the v1 minimum.

---

# 8. Gating per profile

## 8.1. General principle
There are blocks per profile.
One profile does not block another.

## 8.2. Clear cases already defined
- a new profile without a final ILK;
- a profile whose ILK is not `complete` yet;
- an LH ICH with automation disabled.

## 8.3. Consequence
While a profile is not ready:
- the adapter must not send conversational messages for that profile;
- the node must not return a provisional ILK to it;
- and the channel waits until promotion/enablement.

---

# 9. Configuration changes returned through the beacon

## 9.1. The beacon as a channel for incremental changes
Besides results, the beacon must be able to return to the adapter:

- the ILK of a profile that is ready;
- LH automation enabled/disabled for an ICH;
- other incremental changes the adapter needs to keep operating.

## 9.2. Important case: auto-detected LH ICH
Auto-detected Linked Helper ICHs are born with automation disabled.
When that flag changes, the node must return it to the adapter through the beacon.

## 9.3. Effect on the adapter
When it receives automation disabled for a profile's LH ICH:
- it stops looking for/reporting messages for AI for that profile;
- it may keep reporting states, alerts and non-conversational events.

## 9.4. Collapsing changes per ICH
For configuration/state changes per ICH that are pending delivery to the adapter, the node should keep the **last relevant state**.

Example:
- `disabled`
- then `enabled`

It is better not to return both changes if the second one already overrides the first.
Unless another scheme with versioning or timestamps is explicitly defined later, at this stage the last known state per ICH should prevail.

---

# 10. Node responses

## 10.1. `ack`
It indicates reception/acceptance for processing.

## 10.2. `result`
The conceptual model is kept:

- `status = success | error`
- `result_type`
- `payload` optional
- `error_code` optional
- `error_message` optional
- `retryable` optional

### Important rule
`error` is not a separate family; it is a `result` with `status = error`.

## 10.3. `heartbeat`
It is used when there is nothing else to return, or as the minimal response to an empty poll.

## 10.5. Intermediate HTTP envelope implemented today

Besides `items`, the node's current HTTP response includes:

- `ok`
- `accepted_at`
- `response_id`
- `adapter_id`
- `actions`

Current state:

- `actions` exists for compatibility with the intermediate path;
- today the node responds `actions: []`;
- the channel's useful responses still travel in `items`.

## 10.4. Beacon results already on track
The firmest cases today are:

- `profile_ready`
- `automation_enabled`
- `automation_disabled`
- resolution/promotion errors

---

# 11. Minimal durable state of the node

The node should durably persist the channel's minimal coordination state, including at least:

- mapping `adapter_id / managed_instance_id ↔ auth key ref` or an equivalent reference;
- mapping `adapter_id / managed_instance_id ↔ discovered profiles`;
- mapping `external_profile_id ↔ provisional/final ILK`;
- last poll observed per adapter;
- pending deliveries per adapter;
- last known automation state per own ICH.
- mapping `ILK/ICH ↔ adapter_id / managed_instance_id`;
- list of provisional ILKs pending promotion;
- last observed automation state per ICH for LH;
- changes pending delivery to the adapter;
- a durable or rebuildable result queue per adapter.

## 11.1. Reason
Persistence is not only to “not lose data”, but to be able to:

- keep monitoring pending ILKs;
- correctly associate changes with the right installation;
- and always return the information to the right adapter.

---

# 12. `get-config` / `set-config` (tentative status)

## 12.1. Current status
For now, `get-config` / `set-config` remain **tentative**.
Their final shape is not settled yet.

## 12.2. Agreed tentative criterion
If that path is taken:

- it is better not to drift too far from how config looks in other nodes;
- the node's common parts should stay at the top level;
- and the Linked Helper-specific parts should be added in their own keys/sections.

## 12.3. Conceptual distinction
Even if they travel through the same mechanism, it is better to distinguish internally between:

### The node's own config
Examples:
- port;
- installation keys / registered adapters;
- node limits or switches.

### Channel state / pending
Examples:
- pending provisional ILKs;
- pending changes.

## 12.4. Important note
The administrative exposure of:
- enabled/disabled ICHs
- and pending ILKs

is no longer considered a responsibility of the `IO.linkedhelper` node.
That must be handled by identity/core through other mechanisms.

## 12.5. `set-config`
If it were used in a first implementation:
- it should keep supporting the common behavior expected for the node's config;
- and it could accept a limited extension for node-specific changes.

But this is still tentative and pending a definition with the rest of the team.

---

# 13. What is still current and what changes

## 13.1. Still current
- HTTP + HTTPS
- installation key per adapter
- polling/beacon always started by the adapter
- heartbeat as the way to pick up pending items
- result queue per adapter
- `ack` / `result` / `heartbeat` families
- `error` as a `result` status

## 13.2. Changes or is reframed
- `profile_create` no longer implies immediate enablement;
- `conversation_message` is not sent until there is a final ILK and automation is enabled;
- the beacon now also carries incremental configuration changes;
- the adapter no longer consumes provisional ILKs;
- `profile_update` leaves the v1 minimum;
- `get-config` / `set-config` no longer carry responsibility for enabled/disabled ICHs and pending ILKs.

---

# 14. Real open items / black boxes

## 14.1. `get-config` / `set-config`
Not defined:
- the final shape;
- whether `set-config` will touch only common config or also some node extension;
- how much operational state it is worth returning from there.

## 14.2. Exact payloads
The exact schema of each message type under this new split is still pending.

## 14.3. Retries / timeouts
The fine-grained policy is still open and does not block this document.

---

# 15. Summary

The `adapter ↔ IO.linkedhelper` channel is currently modeled as:

- HTTP/HTTPS;
- a single node instance on a fixed port to be defined;
- polling always started by the adapter;
- heartbeat as the way to pick up pending items and as the channel for incremental changes;
- result queue per adapter;
- ICH changes collapsed to the last state;
- minimal durable channel state;
- automation controlled by the LH channel's ICH.
