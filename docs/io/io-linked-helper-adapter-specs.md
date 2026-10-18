# Technical specification v0 — `Linked Helper` adapter
## Basic definitions for the MVP

> Working document to set the first technical specification of the Linked Helper adapter.
>
> Status: v0 / basic definitions.
>
> Goal: fix the scope, responsibilities, assumptions and base decisions of the adapter, so that detailed design and implementation can move forward.
> 2026-05-15: Important note. This documentation was prepared before the changes to the LH monitor. Before moving forward with this document (whether to redefine it or implement it) it is **MANDATORY** to check whether all the concepts set here are still current

---

# 1. Scope

This document defines the Linked Helper adapter as a component that:

- runs on the same PC where Linked Helper is installed and running;
- observes Linked Helper's local technical structures;
- detects relevant instances, profiles and conversations;
- persists local state in embedded SQLite;
- communicates with the `IO.linkedhelper` node over HTTP/HTTPS;
- sends events to Fluxbee through the node;
- receives results/changes through the beacon;
- and executes the corresponding actions in Linked Helper.

This document does **not** settle yet:

- the exact SQLite schema;
- the final JSON contract of each payload;
- the fine-grained concurrency strategy in Rust;
- the full alert taxonomy;
- attachment support;
- support for editing/deleting messages already sent;
- or the exact mechanism to access Linked Helper.

---

# 2. Base technology

## 2.1. Language
The adapter will be implemented in **Rust**.

## 2.2. Local persistence
The adapter will use **embedded SQLite** as its local database.

## 2.3. Role of SQLite
SQLite will be used to:

- persist durable local state;
- hold the adapter's local identity and its operational configuration;
- record entities observed from Linked Helper;
- record outgoing events;
- record incoming results/actions;
- allow rebuilding the state after a restart;
- and support the adapter's basic idempotency.

---

# 3. Guiding principle

The adapter:

- **interprets the technical structure** of Linked Helper;
- **carries canonical events** to the node;
- but **does not resolve the canonical business identity**, the final tenant, capabilities, prompting or functional decisions of the system.

In other words:

- it understands **how to read and act** on Linked Helper;
- it does not decide **what that means** at the business level inside Fluxbee.

---

# 4. Responsibilities of the adapter

## 4.1. Local discovery
It must be able to:

- detect relevant Linked Helper instances;
- detect available profiles;
- detect relevant conversations/messages;
- and keep that state up to date locally.

## 4.2. Building canonical external ids
It must build the canonical external ids the channel needs, in particular:

- `external_profile_id`
- canonical contact ids, when applicable

These ids must reach the node ready to use.

## 4.3. Local persistence
It must persist the minimal state needed to:

- not resend events already sent;
- not lose track on restart;
- rebuild local queues;
- and support idempotency of events and actions.

## 4.4. Message consolidation
It must consolidate conversational messages locally before emitting `conversation_message`.

Consolidation belongs to the adapter because:

- Linked Helper already persists the messages in a way suited to consolidating them;
- the adapter knows LH's real data structure;
- and the node should not absorb logic specific to LH's internal storage.

## 4.5. Communication with the node
It must:

- always start the communication with the node;
- send events;
- do periodic polling/beacon;
- receive `ack`, `result` and `heartbeat`;
- and update its local state based on those responses.

## 4.6. Executing actions in LH
It must execute in Linked Helper the actions the system returns to it through the channel, within the MVP scope.

## 4.7. Operational monitoring
It must detect and record operational failures, its own or in the access to LH, and may emit alerts when appropriate.

---

# 5. What the adapter does NOT do

The adapter must not:

- define the profile's final identity;
- decide the represented tenant;
- resolve the profile's capabilities, prompting or role;
- expose the source of truth of the ICH state;
- be responsible for the global administration of ILKs/ICHs;
- or become a parallel business backend.

That belongs to the node and/or to core, as appropriate.

---

# 6. Relationship with the `IO.linkedhelper` node

The adapter relates to the node under these principles:

- the node is the translation point toward Fluxbee;
- the adapter does not consume provisional ILKs;
- the adapter does not automate for a profile until it receives its ready/usable state;
- automation is governed by the channel's ICH;
- the adapter reacts to `automation_enabled` / `automation_disabled`;
- and the node is still the one that observes the ILK promotion and the changes to its own ICHs.

This is consistent with the node's current documentation base. fileciteturn37file0turn37file1turn37file2turn37file3

---

# 7. Basic operating model

## 7.1. Observation
The adapter observes Linked Helper's local state.

## 7.2. Normalization
It turns what it observes into canonical channel events.

## 7.3. Persistence
It persists those events and their local state in SQLite.

## 7.4. Sending
It sends the events to the node in the next polling/beacon cycle.

## 7.5. Reception
It receives responses/results/changes from the node.

## 7.6. Application
It applies the needed actions in LH locally.

## 7.7. Local confirmation
It persists the result of the application, success or error.

---

# 8. Message consolidation

## 8.1. Decision
For the MVP, consolidation happens in the adapter.

## 8.2. Reason
The real source of messages is Linked Helper, and the adapter knows how they are stored.

## 8.3. Type of consolidation
Consolidation will initially be:

- mechanical;
- deterministic;
- based on the technical structure observed in LH;
- not “intelligent”, and not business semantics.

## 8.4. Expected result
The adapter emits a consolidated `conversation_message` as a conversational unit the node can use.

---

# 9. Idempotency and local deduplication

## 9.1. Principle
The deduplication that matters in the adapter is not the deduplication of LH's raw data, but that of:

- outgoing events;
- incoming actions/results.

## 9.2. Minimal rules
The adapter must avoid:

- sending the same `profile_create` twice;
- sending the same consolidated `conversation_message` block twice;
- executing the same logical action received from the node twice.

## 9.3. Consequence
Local persistence must make it possible to know:

- which event was already emitted;
- which event was acknowledged;
- which action was already applied;
- and what is still pending.

---

# 10. Message edits and deletions

## 10.1. MVP scope
The adapter MVP covers **detection of new messages**, not editing/deleting messages already sent.

## 10.2. Reason
There is not yet enough technical certainty about:

- real and consistent LinkedIn support for post-send editing/deleting;
- reliable visibility of those changes from Linked Helper;
- or enough value to take on that complexity in the first version.

## 10.3. Future
It is left as a possible later extension, if it is verified that LH exposes it in a consistent and useful way.

---

# 11. The adapter's own alerts

## 11.1. Principle
The adapter may emit its own alerts, but in a limited way.

## 11.2. Kinds of expected alerts
Examples:

- inability to access needed LH structures;
- strong inconsistency in observed data;
- inability to persist to SQLite;
- prolonged inability to communicate with the node;
- inability to execute a local action in LH;
- other technical failures of the adapter itself.

## 11.3. Limit
The adapter must not become a complex monitoring system or emit arbitrary alerts.

---

# 12. Profiles and automation

## 12.1. New profiles
When the adapter detects a new profile:

- it reports it;
- but it does not start automation for that profile;
- and it does not use any provisional ILK.

## 12.2. Activation
It only starts automating when it receives, through the channel, that the profile is ready and its automation is enabled.

## 12.3. Deactivation
When it receives `automation_disabled`:

- it stops looking for/reporting messages for that profile's automation;
- although it may keep observing and reporting non-conversational states/alerts, if appropriate.

---

# 13. Local persistence in SQLite

## 13.1. Decision
Embedded SQLite will be the adapter's official local persistence.

## 13.2. Minimal functions it must cover
It must be able to hold:

- the adapter's local configuration;
- the state of detected instances;
- the state of detected profiles;
- the mapping of external ids;
- outgoing events;
- incoming results/actions;
- local execution of actions;
- basic errors/retries;
- and idempotency marks.

## 13.3. Status
The exact table/entity model is pending a detailed definition in the next iteration.

---

# 14. Polling and beacon

## 14.1. Principle
The adapter always starts the communication with the node.

## 14.2. Behavior
In each cycle:

- if it has events, it sends them;
- if it has no events, it sends a heartbeat;
- and in both cases it receives pending results/changes from the node.

## 14.3. Status
The frequency, batch size and priorities are left for the detailed design.

---

# 15. Minimal MVP actions

For the MVP, the adapter must be designed to support at least:

- profile detection;
- sending `profile_create`;
- consolidating and sending `conversation_message`;
- receiving `profile_ready`;
- receiving `automation_enabled` / `automation_disabled`;
- basic execution of message sending in LH;
- local persistence of the result of those actions;
- and basic error handling.

---

# 16. Local security

## 16.1. Current status
Local security is left as a topic to detail later.

## 16.2. Minimal baseline
At least the following must be covered:

- a secure location for the installation key;
- location and permissions of the SQLite database;
- location and permissions of the logs;
- and basic behavior when the adapter is copied or reinstalled.

---

# 17. Minimal observability

The adapter should offer, at a minimum:

- structured logs;
- visibility of the polling state;
- visibility of the local backlog;
- visibility of execution errors;
- and the last successful/failed communication with the node.

The concrete mechanism is left for the detailed design.

---

# 18. Topics left for the next iteration

1. SQLite data model
2. the adapter's exact internal pipeline
3. polling/batch frequency and policy
4. concrete execution of actions on LH
5. error categories and retries
6. internal concurrency in Rust
7. exact mechanism to access LH
8. minimal `system_alert` taxonomy
9. concrete observability
10. detailed local security

---

# 19. Summary

The Linked Helper adapter, in this v0, is defined as a Rust component with embedded SQLite that:

- observes Linked Helper locally;
- interprets its technical structure;
- consolidates conversational messages;
- builds canonical external ids;
- persists durable local state;
- emits events to the `IO.linkedhelper` node;
- receives results/changes through the beacon;
- executes local actions in LH;
- and keeps idempotency and basic error handling.

This base is already enough to move on to a more detailed specification focused on:
- the SQLite data model,
- the internal pipeline,
- and the execution of actions.
