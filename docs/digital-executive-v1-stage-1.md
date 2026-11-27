# The Digital Executive v1, stage 1: the base of identity and security

**Status:** the build design, 2026-11-25, agreed with the operator the same day (§6). Built and
live in 0.1.153 (release A) and 0.1.154 (release B).

**Withdrawn in part, 2026-11-26** (the operator): the security path went ahead of the flow it was to
protect, and its rule bent the next stage's design. What comes out of the code:

- §3, the router's binding of a person's `src_ilk`, whole (release B);
- in §2.1, the refusals of a channel added to a virtual ILK and of an IO's provision onto one (with
  them goes the `cloud` channel's own refusal: in this respect an ED is a person again);
- §4's io.api refusal of a virtual `by_ilk`;
- §7's first and third items.

What stays: the flag (§1), the virtual person's birth (§1.2), its merge refusals (§2.1), and
SY.admin's rules for IO.cloud (§2.2). Which node is an ED's, and what identity keeps of it, is to
be talked once the new path is built ([stage 2](digital-executive-v1-stage-2.md)).

**What it builds:** stage 1 of [digital-executive-v1-design](digital-executive-v1-design.md) §15.
- **The ED's ILK:** a person's ILK with the `virtual` flag.
- **What an ED refuses:** merges, a Cloud account, and Cloud's person actions (design §3.1). The
  Cloud account's refusal is withdrawn (2026-11-26).
- **The router binds a person's `src_ilk`** (design §7.2). Withdrawn (2026-11-26).

Nothing of SY.deploy, channels, the conscious or the profile comes in this stage: they are stages
2 and 4.

**What it leaves working:** every path that sends a person's ILK today keeps working. A stage-1 ED
has no channel and no conscious yet, so nothing legitimate speaks as one.

---

## 1. The `virtual` flag

### 1.1 Stored, replicated, projected

- **SY.identity's database:** migration v6 (`virtual_person`) adds `is_virtual BOOLEAN NOT NULL DEFAULT false` to
  `identity_ilks` (`src/bin/sy_identity.rs`; the last migration is v5,
  `drop_vocabulary_and_approved_by`).
- **The record:** `IlkRecord` gains `is_virtual: bool`, `virtual` on the wire, with
  `#[serde(default)]`, so records and replication deltas without it read as false.
- **The shared memory:** the projection sets a new bit in the entry's `flags` when the record is
  virtual.
  - **Its constant:** `ILK_FLAG_VIRTUAL`, for ILK entries only, chosen apart from the generic
    `FLAG_*` constants of the node and route tables.
  - **Its three copies:** `src/shm/mod.rs`, the Rust SDK's `crates/fluxbee_sdk/src/identity.rs`, and
    the Go SDK's `go/fluxbee-go-sdk/identity_shm.go`, each with an `is_virtual` accessor.
  - **What does not change:** the layout and `IDENTITY_VERSION` 6. Every reader masks
    `FLAG_ACTIVE`, so an old reader ignores the new bit.
- **A replica:** a replica-pushed record may neither set nor clear `virtual`, nor create a virtual
  ILK (the same guard `system` has, `src/bin/sy_identity.rs`).

### 1.2 Born only one way

- **A new verb, `ILK_CREATE_VIRTUAL`,** taken from exactly `SY.admin@motherbee` (as
  `ILK_RETIRE_HIVE` is).
- **What it creates:** an ILK of type `human`, `virtual` true, registration `complete`, charged to
  the tenant given. It takes `display_name` and `company_name`, and no channel. Never in the root
  tenant.
- **A new SY.admin action, `create_virtual_person`,** reachable by the operator (the HTTP API) and
  by Archi. Not exposed to IO.cloud in this stage: Cloud's create of an ED comes with SY.deploy, in
  stage 2.
- **The reads say it:** `GET .../identity/ilks/{id}`, and the list, show `virtual`.

---

## 2. What an ED refuses (design §3.1)

Each refusal answers a new code, **`ILK_VIRTUAL`**, with the action's name in its detail.

### 2.1 In SY.identity

- **No merge into a virtual ILK,** from any of the three paths:
  - `merge_registration`, when the holder of the vouched email is virtual: an `ILK_REGISTER` from
    the frontdesk, which is also Cloud's `register_human` path;
  - `ILK_ADD_CHANNEL` with `merge_from_ilk_id` whose target is virtual;
  - the provision of a `cloud` account channel, when its key would land on a virtual ILK
    (withdrawn, 2026-11-26).
- **No channel added to a virtual ILK** by any caller, by `ILK_ADD_CHANNEL` or `ILK_UPDATE`. Its
  channels arrive in stage 2, through SY.deploy and SY.admin. **Withdrawn (2026-11-26):** a virtual
  ILK takes channels as any person does.
- **No `cloud` channel on a virtual ILK,** ever: an ED holds no Fluxbee Cloud account. **Withdrawn
  (2026-11-26)** with the one above.
- **No provision onto a virtual ILK** (§7). **Withdrawn (2026-11-26).**
- **No `set_ilk_tenant` of a virtual ILK** from Cloud's relay (§2.2).

### 2.2 In SY.admin's rules for IO.cloud

`delete_ilk`, `restore_ilk` and `set_ilk_tenant` coming from IO.cloud refuse a virtual ILK.
SY.admin reads the flag in the identity SHM. The rule sits next to the one that already makes Cloud
carry `ilk_type: "human"`. The operator, through the HTTP API and Archi, keeps these actions in
this stage, for tests and cleanup. In stage 2, an ED's life moves to SY.deploy.

---

## 3. The router binds a person's `src_ilk` (design §7.2)

**Withdrawn (2026-11-26), and out of the code.** Kept here as the record of what was built and why
it was taken out (the status above).

### 3.1 Who sends a person's ILK today

Read in the code on 2026-11-25. A rule that missed one of these senders would break a path that
works:

| Sender | What | Where |
| --- | --- | --- |
| `IO.*` | An inbound message carries its writer: io.slack, io.wapp, io.api (and its `by_ilk`), io.linkedhelper. IO.cloud's handoff carries the person it registers. | `nodes/io/common/src/inbound.rs`, `frontdesk_gate.rs`, `nodes/io/io-cloud` |
| `WF.*` | A WF's `send_message` to a node that is not an IO speaks for the instance's person: `src_ilk` is the person. A forward keeps the source's meta. | `go/nodes/wf/wf-generic/node/actions.go` |
| `SY.architect` | Archi's impersonation sends a user message as a chosen ILK, so the operator can test an agent as a person. | `src/bin/sy_architect.rs` |
| `AI.*` | Never a person: a reply carries the agent's own ILK (`stamp_reply`), pinned by a contract test. | `crates/fluxbee_ai_sdk/src/runtime.rs` |
| Other `SY.*` | Their own ILK, or none. SY.frontdesk.gov's replies carry the person as `dst_ilk`, not `src_ilk`; the Go system nodes (SY.timer, SY.wf-rules, SY.opa.rules) never set it. | `nodes/gov/ai-frontdesk-gov`, `go/sy-*` |

### 3.2 The rule

- **Where:** right after the router stamps the sender's name, on the three paths:
  - from a local node (`src/router/mod.rs`, the local message path);
  - from a sibling router of the same hive;
  - from a WAN peer, with the name already canonicalized to the authenticated hive.

  Every destination, not only a resolve.
- **When it applies:** when `meta.src_ilk` names an ILK whose type in the identity SHM is `human`.
- **For a person who is not virtual,** it is accepted from an `IO.*`, a `WF.*` or a `SY.*` sender.
  Every system node is Fluxbee's own code (the operator, 2026-11-25: *"¿los SY? por SY.frontdesk"*),
  and today only SY.architect sends a person's ILK.
- **For a virtual ILK,** it is accepted only from a `SY.*` sender (Archi, for the operator's tests). Stage 2
  adds the owner of one of its channels (each channel's `owner_l2_name`, in the SHM) and its
  registered hand.
- **Anything else is refused.** The sender gets an `UNREACHABLE` with a new reason,
  `SRC_ILK_NOT_BOUND`, and the router logs a WARN naming the sender, the ILK and the destination.
- **Not covered here:** an ILK the SHM does not know (a temporary not yet replicated, an alias)
  falls to today's alias canonicalization. A `src_ilk` of type `agent` or `system` is not touched in
  this stage.

### 3.3 Report first, then enforce

The rule touches every message, so it comes in two steps:

1. **Report:** the router evaluates the rule and only logs `src_ilk_bind would_refuse`, with the
   sender, the ILK and the destination. It delivers as today.
2. **Enforce:** once a run of the gates on 8.x shows no refusal that matters, a release turns the
   refusals on.

The mode is the router's code, not a setting: release B ships `report`, and release C switches it
to `enforce`. Each change of mode is a release, explicit and recorded, with no new key in
`hive.yaml` or in the installation file.

### 3.4 What stays open after stage 1

- **io.api's `by_ilk`.** An integrator with a tenant's bearer can name any person of that tenant.
  The rule lets it through, since io.api is an `IO.*`. Before an ED acts on who is talking, it must
  be decided whether a message by `by_ilk` counts as that person or as "the API on behalf of".
  `by_ilk` refusing a virtual ILK comes in stage 1 (§4).
- **The tenant.** A `WF.*` or `IO.*` of one tenant naming a person of another passes this rule.
  IO.cloud, in the root tenant, hands over people of every tenant, so a tenant check needs its
  exception. Left for when the report shows what crosses.

---

## 4. Smaller items in this stage

- **Withdrawn (2026-11-26):** io.api's `by_ilk` refuses a virtual ILK with `ilk_virtual` (io.api's codes are lowercase),
  after its tenant check (`validate_explicit_subject_ilk`, `nodes/io/io-api/src/main.rs`): an ILK
  the tenant does not know answers `subject_not_found`, virtual or not, so another tenant learns
  nothing.
- **The help** of the touched actions (`create_virtual_person`, `delete_ilk`, `restore_ilk`,
  `set_ilk_tenant`, `register_human`) names the virtual case.
- **Archi's handbook:** a line on what a virtual person is, and that the operator creates one only
  for tests in this stage.

---

## 5. How it ships (METHOD §9)

Two releases, each with its gate:

- **Release A, the flag and the refusals.**
  - **Branches:**
    - `coa/identity-ed1`: SY.identity, the SHM constant in `src/shm/mod.rs`, and the two SDKs'
      copies. This is the shared code, so it is alone on its branch;
    - `coa/admin-ed1`: SY.admin's action and Cloud's refusals;
    - `coa/ioapi-ed1`: io.api.
  - **Its gate:**
    - create a virtual person, and read it back as virtual;
    - Cloud's `register_human` with the ED's email answers `ILK_VIRTUAL`, and no merge happens;
    - an IO's `ILK_ADD_CHANNEL` to it, and Cloud's `delete_ilk` and `set_ilk_tenant`, answer
      `ILK_VIRTUAL`;
    - the operator's `delete_ilk`, then purge, clean it;
    - the smoke (a new throwaway node, a stopped one read);
    - the timings.
- **Release B, the router's rule in `report`.**
  - **Branch:** `coa/router-ed1`.
  - **Its gate:**
    - a throwaway mesh client named `AI.gate…` sends with a person's `src_ilk`, and the log shows
      `would_refuse`;
    - an IO's inbound, a WF's `send_message` (the WF cases' tool) and Archi's impersonation show
      none;
    - a full run of the usual gates, after which the router logs are read for every
      `would_refuse`.
- **Later, release C:** `enforce`, after the report reads clean. The operator's call.

8.x may be reinstalled if a step needs it (the operator, 2026-11-25). Nothing here changes the
SHM's layout, so none should.

---

## 6. The operator's calls (2026-11-25)

1. **The allowed senders:** `IO.*`, `WF.*` and every `SY.*` (*"¿los SY? por SY.frontdesk"*).
2. **Report first, then enforce:** *"primero que ande, después que sea seguro"*; if something
   clashes, talk before.
3. **The operator keeps the person actions on an ED in this stage:** yes, and *"rompe todo sin
   problemas, ahora es avanzar lo más importante"*.
4. **The releases:** their order and number are mine to choose.

## 7. Carried forward, from the review round (2026-11-25)

- **Withdrawn (2026-11-26): to stage 2, an ED's channels in the IOs' own lookup.** An IO resolves a sender's address in the
  identity SHM first, and provisions only on a miss. SY.identity's refusal to provision onto a
  virtual person is the fallback. Once an ED holds channels, the IOs' lookup must leave them out,
  or check `ILK_FLAG_VIRTUAL`: otherwise mail *from* an ED's address would carry the ED's ILK.
- **To stage 2, an idempotency key on a virtual person's creation.** In this stage only the operator
  creates one, and a retry after a timeout makes a second one, cleaned by hand. Cloud's path
  (design §4.1) carries its own id for the ED.
- **Withdrawn (2026-11-26) with the router's rule: before `enforce` (release C), two ways around
  it:**
  - a bare UUID with no `ilk:` prefix, which some consumers also resolve to the person;
  - an active alias, which the pre-resolve turns back into its person.
