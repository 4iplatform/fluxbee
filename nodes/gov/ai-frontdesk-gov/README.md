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
    without an LLM, even when the key is missing.
  - conversational: the LLM gathers the data and calls the `ilk_register` tool.
- **Tenant:** the registration goes to the tenant of the case, the one SY.identity holds for the temporary ILK (the
  frontdesk reads it from the identity SHM). The `tenant_id` of a handoff only has to match. The
  frontdesk never creates tenants, and never takes one from the person or from the LLM; with no tenant it answers
  `TENANT_UNRESOLVED` and registers nothing.
- **Root tenant:** nobody registers there. If the case belongs to the root tenant, it answers
  `TENANT_NOT_REGISTRABLE` (`error_code` `TENANT_ROOT_NOT_REGISTRABLE`) without calling SY.identity
  or the LLM, and the person stays temporary. SY.identity rejects the same for any caller.
  Only io.cloud and io.blob run in the root tenant, and neither of them provisions people there (operator
  decision 2026-10-02): this answer stays as defense in depth.
- **Email already registered:** if the email already belongs to another person of the tenant, SY.identity does the merge
  (the channel moves to that ILK and only the data it was missing is filled in) and the frontdesk answers
  `MERGED`.
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

- Prove that the person owns the email (for example with a one-time code) before the merge:
  today whoever types someone else's email links their own channel to that person's ILK.
- The spec counters (§12) and the conversational retries.

The full contract is in `docs/ai-frontdesk-gov-spec.md`.
