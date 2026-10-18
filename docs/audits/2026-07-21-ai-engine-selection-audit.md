# Audit — AI engine selection (OpenAI + Anthropic)

> **✅ RESOLVED (2026-07-22).** All actionable findings (2 HIGH + 6 MEDIUM + LOW) were implemented, adversarially re-reviewed (the review of the fixes found 3 new defects — an **A1 validator bypass** via userinfo, a **secret leak in M2**, and an **attachment classification regression in M3** — all fixed) and covered by unit tests. See the **Resolution** section at the end. B4 (retry-after HTTP-date) accepted with the current safe fallback; the INFO/known-open items stay with the spec's open checkboxes (C3/C4/D6/F1-F4).

**Date:** 2026-07-21
**Subject:** the work in `docs/history/onworking-coa/ai-engine-selection.md`, implemented in commit `a8a9c54` ("feat(ai): add Anthropic provider support"), audited against HEAD `429a197`.
**Methodology:** 5 independent per-dimension auditors (spec-conformance, Anthropic adapter, credential security, SY consumer migration, ai-generic contract) + adversarial verification of each finding (35 verifiers that try to refute it). 33 findings confirmed, 2 refuted.
**Prior context:** 1 bug in this work was already found and fixed before this audit (wire form `open_ai` vs `openai` — a fresh-install blocker, fix `429a197`). It is not re-reported.

---

## Overall verdict

**The core implementation is substantially real and correct.** The spec's closed checkboxes hold up in the code: `AiProvider`/`HiveAiConfig` with strict parsing and an `openai`+`gpt-5.5` fallback; the `ai` section integrated into ALL the affected hive.yaml parsers (admin, architect, cognition, orchestrator, shared config, frontdesk); the legacy paths (`ai_providers.openai`, `semantic_tagger.provider/model`, `behavior.provider`, `openai_chat`) removed and actively REJECTED, with no aliases; the crux of the tool-loop (B3) genuinely resolved (`FunctionLoopItem::AssistantToolCalls` keeps the full `tool_use` turn, and the continuation rebuilds assistant(tool_use) + ONE user message with every `tool_result` and `is_error`); the Anthropic adapter's headers/retries/error-envelope conform to the pinned reference (Go SDK `0ce94bd`); the orchestrator propagates provider+model (incl. anthropic) to the hive.yaml generated for workers; the per-request resolution of `AI.*` does not cache (a revoked key cannot be served stale); the key is never persisted, never appears in CONFIG_GET (redacted) and is not logged; the `VAULT_SECRET_CHANGED` interest is provider-aware in the 4 SY consumers.

**Where the real risk is:** 2 HIGH findings (credential exfiltration via `behavior.base_url`; silent truncation from `max_tokens=1024` with no `stop_reason` detection), a MEDIUM adapter-robustness cluster (retry broken by a non-JSON body, no structured-output fallback by capability, a deterministic 400 from a history that starts with assistant), and a "the system lies under Anthropic" band (OpenAI-only error classification, hardcoded strings) that violates the §7 done criterion but does not break the selection itself.

---

## Confirmed findings

### HIGH

**A1. `behavior.base_url` turns config access into plaintext credential exfiltration**
`nodes/ai/ai-generic/src/bin/ai_node_runner.rs:1254` (+ `llm.rs:1418-1440`)
`run_ai_chat` passes `ai.base_url` unvalidated to `create_llm_client`, which applies it unconditionally for both providers; the client then sends `Bearer {api_key}` / `x-api-key` to that URL. `base_url` is NOT in the closed contract of §3.5 (kind/vault_key/model), but `EffectiveBehaviorSection` accepts it and `CONFIG_GET` publishes it as an optional field. There is no https requirement and no domain allowlist: `base_url=http://attacker/` receives the key in the clear. It defeats exactly the plaintext confinement that Model D' was designed for — whoever can write config (not secrets) gets the secret.
**Suggested fix:** take `base_url` out of the managed contract (keep it only for tests via env/feature), or validate it in CONFIG_SET: https required + an allowlist anchored to the provider's canonical domain (`api.openai.com`/`api.anthropic.com`), except for an explicit, audited override. Related: INFO-6 (base_url survives a provider rotation of the key).

**A2. Hardcoded `max_tokens=1024` + zero `stop_reason` detection = silent truncation by default**
`crates/fluxbee_ai_sdk/src/llm.rs:981, 1132-1140, 1275-1281`
`ANTHROPIC_DEFAULT_MAX_TOKENS=1024` is the fallback of `generate()` and of the function-calling model, which only reads its own `ModelSettings` — and `sy_admin.rs:933` + `sy_architect.rs:6133` build with `ModelSettings::default()` (`max_output_tokens: None`). On an Anthropic hive, **every turn of the admin executor and of architect (all subagents) is capped at 1024 output tokens**. `grep stop_reason llm.rs` returns nothing: the truncation is never detected, and it corrupts structured outputs and `tool_use` turns, showing up as "unrelated" parse errors.
**Suggested fix:** raise the default (4096-8192 per model) or make it configurable per hive; read `stop_reason` from the response and return an explicit truncation error (or at least warn + marker) when it is `max_tokens`, especially before validating structured output or dispatching tools.

### MEDIUM

**M1. The retry loop parses the body as JSON BEFORE deciding the retry**
`crates/fluxbee_ai_sdk/src/llm.rs:1050-1056`
`response.json().await?` runs before `anthropic_should_retry(...)`. A 502/503/529 from a proxy/LB with an HTML body (a common case) makes `.json()` fail, and the `?` propagates immediately — the WHOLE retry loop is skipped and the envelope with status/request-id is lost. The retry policy (which in itself conforms to the Go SDK) is defeated exactly in the case that needs it most.
**Fix:** read the body as text, decide the retry by status+headers first, parse JSON only in the success/final-error paths; treat a body-read failure on a retryable status as retryable.

**M2. The mandated FAILED_CONFIG almost never fires in the real upgrade**
`nodes/ai/ai-generic/src/bin/ai_node_runner.rs:5137` (+ `:249`, `:4559-4677`)
The old runner materialized `behavior.provider="openai"` in EVERY persisted config. The new `EffectiveBehaviorSection` is `deny_unknown_fields`, so those docs fail deserialization, and `load_persisted_dynamic_config` collapses them with `.ok()` into "no persisted config" → the node boots UNCONFIGURED (or re-adopts the spawn file, losing `config_version`) **silently, with no log and without the FAILED_CONFIG that §3.5/E3 mandates**. The operator loses the diagnostic signal needed to recover.
**Fix:** distinguish "file exists but fails the strict parse" from "file absent": lenient parse first (versions + raw config), and on a schema failure enter `FailedConfig` with the raw doc as `effective_config` and the stored `config_version` (mirroring the `:4587-4605` branch).

**M3. Every Anthropic HTTP error collapses to `provider_error retryable:false` (ai-generic + frontdesk-gov)**
`nodes/ai/ai-generic/src/bin/ai_node_runner.rs:5691` and `nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs:3902`
`parse_openai_status_error` matches only the literal marker `"openai error status="`; the Anthropic adapter emits `"anthropic error status=... type=... request_id=..."`. Under an Anthropic hive: a 401 is never `provider_auth_error`, a 429 after the retries are exhausted is never `provider_rate_limited retryable:true`, a 5xx is never `provider_unavailable`, and attachment rejections are never classified. The envelope's status/request-id/detail are lost.
**Fix:** have the SDK return a TYPED error (`ProviderStatusError { provider, status, request_id, detail }`) instead of having the consumers string-scrape `Protocol`; at a minimum, teach the parser both markers in the two runners.

**M4. B5 marked `[x]` but the structured-output fallback by capability does NOT exist**
`crates/fluxbee_ai_sdk/src/llm.rs:1162-1173`
`output_config.format=json_schema` is sent unconditionally for any Anthropic model. A model without the capability gets a raw 400 (an opaque `Protocol` string) — the helper `build_output_schema_fallback_instruction` exists (`llm.rs:211`) but is only wired to ai-generic's tools branch. The spec says "Anthropic structured output **or explicit fallback by capability**".
**Fix:** on a 400 that indicates output_config is not supported, retry once without `output_config`, with the fallback instruction in system (the local `validate_structured_output` validation already covers the check); or keep an explicit capability-check with a typed error.

**M5. No role normalization: a history that starts with assistant → deterministic 400**
`crates/fluxbee_ai_sdk/src/llm.rs:1354-1416` (+ `function_calling.rs:298-312`)
`build_anthropic_function_messages` maps 1:1 without checking that the first message is `user` (a requirement of the Anthropic API). `normalize_recent_interactions` truncates the immediate-memory window with `saturating_sub` regardless of the role it starts on → roughly half of the parities of a long alternating conversation produce a request that gets a 400.
**Fix:** in the builder, drop the leading assistant messages (or turn them into a system note) so that `messages[0]` is always user; or truncate the window at a user-first boundary.

**M6. The architect chat response hardcodes `"provider":"openai"`**
`src/bin/sy_architect.rs:10295`
`handle_ai_chat` builds the model with `runtime.provider` (correct) but responds with `json!({... "provider": "openai" ...})` using the literal. On an Anthropic hive the contract lies: provider=openai with an Anthropic model. A direct violation of D5/§7 in a checkbox marked done.
**Fix:** `"provider": runtime.provider.to_string()`.

### LOW

**B1. Silent fallback to openai in the interest handler on an invalid hive.yaml** — `sy_admin.rs:2783-2794` and `sy_architect.rs:6778-6789`: `load_hive(...).ok()...unwrap_or_else(HiveAiConfig::fallback)` swallows both the load error and the `effective()` error. Boot does propagate the error (`?`); this only hits if hive.yaml becomes invalid after boot, but it is exactly the "silent fallback" that §3.3 forbids, and it desyncs the interest from the boot provider. **Fix:** cache the boot `EffectiveAiEngine` (as cognition does), or log the error and skip the event.

**B2. Cluster of OpenAI-only strings that lie under Anthropic** — static messages that name OpenAI unconditionally: the admin executor's not-configured error (`sy_admin.rs:2470`), architect's CONFIG_SET/rejection messages (`sy_architect.rs:11367`+), frontdesk-gov messages (`ai_node_runner.rs:1901`+), ai-generic's secrets-rejection message (`:1795`), and 2 runbook lines (`docs/ai-nodes-deploy-runbook.md:44,252` — can be folded into F1). **Fix:** interpolate the effective provider / use provider-neutral wording.

**B3. `image/gif` routed as Document → misleading error** — `text_payload.rs:466-468`: `is_openai_image_mime` (png/jpeg/webp) gates the Image branch of the shared builder; a gif falls through to Document, and the Anthropic adapter rejects it with "unsupported document" even though its image branch accepts gif. **Fix:** a provider-neutral image predicate that includes gif.

**B4. `Retry-After` in HTTP-date form not parsed** — `llm.rs:1101-1107`: only the numeric form is parsed; the date form (RFC 9110, handled by the reference Go SDK) is silently ignored (it falls back to exponential backoff — safe but not conformant). **Fix:** parse httpdate → delta-to-now, clamped to the 60s cap.

**B5. `ResolvedAiCredential` derives `Debug` with the key in the clear** — `ai_node_runner.rs:393`. No `{:?}` call-site today (a latent hazard), but inconsistent with the repo's patterns (the manual redacted Debug of `PublicationRuntime`; the admin/architect runtimes derive only Clone). **Fix:** a manual redacted Debug, or a newtype for the key.

**B6. The OpenAI error body (which on a 401 echoes the partially masked key) flows to logs and to the mesh** — `llm.rs:427` (+`:823`): the error embeds the full body; `ai_node_runner.rs` logs it (`error = %err`) and forwards up to 280 chars as `provider_detail` in the response envelope. The Anthropic path is cleaner (it extracts only type/request_id/message). **Fix:** on 401/403, extract only type/message (like Anthropic) or scrub `sk-…` tokens.

**B7. Cognition persists provider=openai/gpt-5.5 in its state file under an Anthropic hive** — `sy_cognition.rs:517` (+`:2632`): it persists the serde-skip defaults and only afterwards stamps the effective one in memory, without re-persisting. The live CONFIG_GET/STATUS is correct; the file and a log are misleading. **Fix:** stamp before persisting.

**B8. Every rejected CONFIG_SET responds `state=UNCONFIGURED`** — `ai_node_runner.rs:1905-1918`: a CONFIGURED node that keeps running its previous behavior (or one in FAILED_CONFIG) reports UNCONFIGURED in the rejection. STATUS/PING are not affected. **Fix:** respond with the real lifecycle.

**B9. A key with a non-AI `resource_type` / missing metadata / empty credential / vault down all collapse into the same `missing_ai_api_key`** — `ai_node_runner.rs:1598-1625`. Known-open (a restatement of AIENG-C4); recorded here so that C4 covers it with granularity (provider-mismatch vs transient outage, correct retryable).

### INFO

- **I1.** ai-generic's `CONFIG_GET` returns neither the derived provider nor the real availability of the key (§3.5 declares it part of the closed contract, but it is the open AIENG-C3 — known-open).
- **I2.** The admin executor's vault catalog/examples only list openai/postgres/slack shapes — no guidance for creating the anthropic key (known-open AIENG-F2).
- **I3.** `generate_stream` is a buffered pseudo-stream (the trait default, a single Delta) for both providers — deliberate and uniform; document it, or implement SSE only if incremental delivery is ever needed.
- **I4.** The `ai_local_probe` binaries (ai-generic and frontdesk-gov) are still OpenAI-only, with a direct `OpenAiResponsesClient` — a residual of B8; a `--provider` flag, or document them as OpenAI-only.
- **I5.** Refresh asymmetry on a hive flip: admin/architect re-read hive.yaml per event (live flip), cognition/frontdesk use the provider stamped at boot (a flip requires a restart). Each one is internally consistent; align or document.
- **I6.** `behavior.base_url` is provider-agnostic: rotating the key from openai→anthropic without clearing an old base_url sends the new key to the old host (a visible failure, but the key travels). Fold into the rotation E2E (AIENG-F3) and into the A1 fix.
- **I7.** The `open_ai` alias from fix `429a197` covers only serde deserialization; `FromStr` and the vault metadata comparison do not accept it — a key whose metadata was written as `open_ai` during the broken window is rejected with a clear log. Real but marginal (a window of hours, not deployed).

## Refuted (adversarial verification)

- `open-ai-alias-serde-fromstr-asymmetry` (as a design defect): the asymmetry is deliberate — the alias is read-only for configs written during the broken window; the marginal remainder stayed as I7.
- `vault-src-ilk-self-asserted`: the self-asserted `meta.src_ilk` that underpins the vault authorization is the PRE-EXISTING posture of Model D' (canonicalized by the router, not bound to the connection) — it was not introduced by this work. Note: the arbitrary `get(key)` path of `AI.*` makes it more load-bearing; it is worth keeping in mind when the identity plane's peer-auth is revisited.

## Relation to the spec's open backlog

The open checkboxes (B9 mock-HTTP tests, C3 redacted diagnostics, C4 credential edge cases, D6 per-consumer tests, E5 E2E for both providers, F1-F4 docs/E2E/gate) are still open, and this audit confirms it — several LOW/INFO findings are exactly the work those items would cover. Findings M1-M6 and A1-A2, by contrast, contradict checkboxes **marked done** (B4/B5/B3-adjacent, E3, D5) or the closed contract, and are not covered by any open item.

## Suggested resolution order

1. **A1** (base_url) — it is the only one with a direct security impact; small fix (validation/removal of the field).
2. **A2** (max_tokens + stop_reason) — the most likely production failure under Anthropic; small fix.
3. **M3** (error classification, ideally with a typed error in the SDK) — unblocks correct diagnostics for everything else.
4. **M1, M5** (retry body-parse; role normalization) — adapter robustness, small fixes.
5. **M2** (FAILED_CONFIG on upgrade) + **M6** (hardcoded provider) + **B1/B2** (lies under Anthropic).
6. **M4** (fallback by capability) — or un-check B5 and decide whether it is wanted.
7. The rest (LOW/INFO) can travel with the open checkboxes C3/C4/D6/F1-F3.

## Resolution (2026-07-22)

Implemented + unit-tested + adversarially re-reviewed. The review of the fixes (3 reviewers) found **3 new defects** in the implementation itself, all fixed before closing:
- **A1 bypass**: the host extractor split on `:`, letting `https://api.openai.com:x@evil.com` through (real host = evil.com) → **exfil anyway**. Fixed: any userinfo (`@`) is rejected, and the host is extracted before the `:port`.
- **M2 leak**: the rejected doc was surfaced raw in `effective_config`, and `redact_secrets` only masks the `api_key` key → an inline secret with another name leaked through CONFIG_GET. Fixed: the body is held back (withheld) if it contains a secret field; + a TOCTOU double-read that could panic the boot was removed.
- **M3 regression**: changing the OpenAI error from `body=` to `type/message` broke `extract_openai_error_param` (it parsed the JSON body) → the `provider_attachment_invalid_request` classification was left dead. Fixed: the error now carries an explicit `param=`, and the extractor reads it from the string; also applied to the function-calling path.

| Finding | Status | Fix |
|----------|--------|-----|
| A1 base_url exfil | ✅ | `validate_managed_base_url`: https + provider host allowlist + userinfo rejection; override only via the env `FLUXBEE_ALLOW_INSECURE_BASE_URL` (not the config-plane). Both build-paths. |
| A2 silent truncation | ✅ | Anthropic default 1024→4096 + `anthropic_check_truncation`(`stop_reason`) and `openai_check_truncation`(`incomplete_details`) in generate + function-calling. |
| M1 retry broken by a non-JSON body | ✅ | The retry decides by status+headers before parsing; tolerant body in the non-retry path. |
| M2 FAILED_CONFIG on upgrade | ✅ | `persisted_config_rejected` distinguishes a rejected doc from an absent one → FAILED_CONFIG with config_version (body redacted if it holds a secret). |
| M3 unclassified Anthropic errors | ✅ | `parse_openai_status_error` understands both markers (openai/anthropic) + `param=`; in ai-generic and frontdesk. |
| M4 structured-output fallback by capability | ✅ | `generate` retries 1× without `output_config`, with a prompt instruction, on a capability 400. |
| M5 first-message ≠ user | ✅ | `build_anthropic_function_messages` drops leading assistant messages + orphaned tool_result blocks. |
| M6 architect hardcoded provider | ✅ | Returns `runtime.provider.to_string()`. |
| B1 silent fallback | ✅ | Admin/architect interest handlers: warn on error, quiet only on "no ai section". |
| B3 gif as Document | ✅ | Provider-neutral `is_supported_image_mime` (includes gif). |
| B5 Debug with key | ✅ | `ResolvedAiCredential`: manual redacted Debug. |
| B6 OpenAI key body-echo | ✅ | OpenAI error without the raw body + `scrub_secret_like` (redacts `sk-…` at a token boundary). |
| B2 "OpenAI" strings under Anthropic | ⚠️ partial | Fixed the admin executor's not-configured error (the most misleading one); the remaining cosmetic strings stay with the F1/F2 docs pass. |
| B4 retry-after HTTP-date | ⏸️ accepted | The fallback (bounded exponential backoff) is safe; a hand-written date parser is more fragile. |
| INFO / known-open (I1-I7, B7-B9) | ⏸️ | Diagnostic/cosmetic items left to the spec's open checkboxes (C3/C4/D6/F1-F4). |

Commits: (see the `fix(ai)` commit that references this doc). New tests: SDK (`openai_status_error` without key-echo, `scrub_secret_like`, truncation detectors, `output_config_unsupported`), ai-generic (`validate_managed_base_url` incl. the userinfo bypass, `parse_provider_status_error` for both providers, `extract_openai_error_param` + attachment). Suites: SDK 65, ai-generic 39, frontdesk 26 — all green.
