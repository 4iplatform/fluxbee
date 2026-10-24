# SY.frontdesk.gov

The identity frontdesk: it completes the registration of a person who arrived with a temporary ILK and
registers them in SY.identity (`ILK_REGISTER`).

## What it is

- A core **system node**, on the motherbee: `sy-frontdesk-gov.service`, binary
  `/usr/bin/sy-frontdesk-gov`, started with no arguments. It is **not** a runtime or an instance of
  `ai.generic`, and it is updated with the `.deb`.
- **Autonomous:**
  - it carries its prompt embedded;
  - the provider and the model come from the `ai` section of `hive.yaml`;
  - the key comes from SY.vault by `resource_type` (root tenant).
  - `CONFIG_SET` accepts no config fields: it only re-reads the key from the vault.
- **Two inputs:**
  - `frontdesk_handoff`: data that is already structured, for example `register_human` from io.cloud. It works
    without an LLM, even when the key is missing. It is a message its producer marks with
    `meta.msg = "frontdesk_handoff"`, never a payload shape alone, and it is taken only from an IO node (the
    sender the router stamps must start with `IO.`); any other sender gets `INVALID_REQUEST`.
  - conversational: the LLM gathers the data and calls the `ilk_register` tool.
- **Tenant:** the registration goes to the tenant of the case, the one SY.identity holds for the temporary ILK (the
  frontdesk reads it from the identity SHM). The `tenant_id` of a handoff only has to match. The
  frontdesk never creates tenants, and never takes one from the person or from the LLM; with no tenant it answers
  `TENANT_UNRESOLVED` and registers nothing.
- **Root tenant:** nobody registers there. If the case belongs to the root tenant, it answers
  `TENANT_NOT_REGISTRABLE` (`error_code` `TENANT_ROOT_NOT_REGISTRABLE`) without calling SY.identity
  or the LLM, and the person stays temporary. SY.identity rejects the same for any caller.
  Only io.cloud, io.blob and io.web run in the root tenant, and none of them provisions people there (operator
  decision 2026-10-02): this answer stays as defense in depth.
- **Vouched and claimed emails** (`docs/control-path-hardening-v1.md` §7): only a handoff's own
  `subject.email` is vouched, and it goes in `identification.email`; the case keeps it, out of the LLM's reach,
  until it registers. If it already belongs to another person of
  the tenant, SY.identity does the merge (the channel moves to that ILK and only the data it was missing is
  filled in) and the frontdesk answers `MERGED`. An email typed in the conversation, or one a handoff without an
  email takes from the thread state, goes in `identification.claimed_email`: SY.identity stores it but never
  merges on it, so the registration completes with the same answer whether or not the email is known.
- What is shared with other `.gov` components lives in `nodes/gov/common` (`gov-common`).
- The logs carry no personal data: from a registration it logs `ilk_id`, `tenant_id` and which fields
  came in, never their values.

## Build and test

From the repo root:

```bash
cargo test -p sy-frontdesk-gov -p gov-common
cargo build --release -p sy-frontdesk-gov --bin sy-frontdesk-gov
```

To run it you need a router and a `hive.yaml` (it takes its name from there,
`SY.frontdesk.gov@<hive>`).

## Pending

- A way to turn a claimed email into a vouched one (for example a one-time code). Until then a typed email
  never merges, and joining a claimant to the person who holds the email is the operator's explicit step,
  which SY.admin does not offer yet.
- The spec counters (§12) and the conversational retries.

The full contract is in `docs/ai-frontdesk-gov-spec.md`.
