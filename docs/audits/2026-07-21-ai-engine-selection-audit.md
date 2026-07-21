# Auditoría — AI engine selection (OpenAI + Anthropic)

**Fecha:** 2026-07-21
**Objeto:** el trabajo de `docs/onworking COA/ai-engine-selection.md`, implementado en el commit `a8a9c54` ("feat(ai): add Anthropic provider support"), auditado contra HEAD `429a197`.
**Metodología:** 5 auditores independientes por dimensión (spec-conformance, adapter Anthropic, seguridad de credenciales, migración de consumidores SY, contrato ai-generic) + verificación adversarial de cada hallazgo (35 verificadores que intentan refutarlo). 33 hallazgos confirmados, 2 refutados.
**Contexto previo:** 1 bug de este trabajo ya fue encontrado y corregido antes de esta auditoría (wire form `open_ai` vs `openai` — blocker de fresh-install, fix `429a197`). No se re-reporta.

---

## Veredicto general

**La implementación central es sustancialmente real y correcta.** Los checkboxes cerrados del spec se sostienen en código: `AiProvider`/`HiveAiConfig` con parseo estricto y fallback `openai`+`gpt-5.5`; la sección `ai` integrada en TODOS los parsers de hive.yaml afectados (admin, architect, cognition, orchestrator, config compartido, frontdesk); los caminos legacy (`ai_providers.openai`, `semantic_tagger.provider/model`, `behavior.provider`, `openai_chat`) eliminados y RECHAZADOS activamente sin aliases; el crux del tool-loop (B3) genuinamente resuelto (`FunctionLoopItem::AssistantToolCalls` conserva el turno `tool_use` completo y la continuación reconstruye assistant(tool_use) + UN user message con todos los `tool_result` e `is_error`); headers/retries/error-envelope del adapter Anthropic conformes a la referencia fijada (SDK Go `0ce94bd`); el orchestrator propaga provider+model (incl. anthropic) al hive.yaml generado para workers; la resolución per-request de `AI.*` no cachea (una key revocada no puede servirse stale); la key nunca se persiste, nunca aparece en CONFIG_GET (redacted) y no se loguea; el interest de `VAULT_SECRET_CHANGED` es provider-aware en los 4 consumidores SY.

**Dónde está el riesgo real:** 2 hallazgos ALTOS (exfiltración de credencial vía `behavior.base_url`; truncamiento silencioso por `max_tokens=1024` sin detección de `stop_reason`), un cluster MEDIO de robustez del adapter (retry roto con body no-JSON, sin fallback de structured-output por capability, 400 determinístico por historia que empieza en assistant), y una banda de "el sistema miente bajo Anthropic" (clasificación de errores solo-OpenAI, strings hardcodeados) que viola el criterio de terminado §7 pero no rompe la selección en sí.

---

## Hallazgos confirmados

### ALTA

**A1. `behavior.base_url` convierte acceso de config en exfiltración de credencial en texto plano**
`nodes/ai/ai-generic/src/bin/ai_node_runner.rs:1254` (+ `llm.rs:1418-1440`)
`run_ai_chat` pasa `ai.base_url` sin validación a `create_llm_client`, que lo aplica incondicionalmente para ambos providers; luego el cliente manda `Bearer {api_key}` / `x-api-key` a esa URL. `base_url` NO está en el contrato cerrado de §3.5 (kind/vault_key/model) pero `EffectiveBehaviorSection` lo acepta y `CONFIG_GET` lo publica como campo opcional. Sin exigencia de https ni allowlist de dominio: `base_url=http://attacker/` recibe la key en claro. Derrota exactamente el confinamiento de plaintext que Model D' diseña — quien puede escribir config (no secretos) obtiene el secreto.
**Fix sugerido:** sacar `base_url` del contrato administrado (dejarlo solo para tests vía env/feature), o validarlo en CONFIG_SET: https obligatorio + allowlist anclada al dominio canónico del provider (`api.openai.com`/`api.anthropic.com`) salvo override explícito auditado. Relacionado: INFO-6 (base_url sobrevive una rotación de provider de la key).

**A2. `max_tokens=1024` hardcodeado + cero detección de `stop_reason` = truncamiento silencioso por default**
`crates/fluxbee_ai_sdk/src/llm.rs:981, 1132-1140, 1275-1281`
`ANTHROPIC_DEFAULT_MAX_TOKENS=1024` es el fallback de `generate()` y del function-calling model, que solo lee su propio `ModelSettings` — y `sy_admin.rs:933` + `sy_architect.rs:6133` construyen con `ModelSettings::default()` (`max_output_tokens: None`). En un hive Anthropic, **cada turno del executor de admin y de architect (todos los subagentes) queda capado a 1024 tokens de salida**. `grep stop_reason llm.rs` devuelve nada: el truncamiento jamás se detecta, y corrompe structured outputs y turnos `tool_use` manifestándose como errores de parseo "no relacionados".
**Fix sugerido:** subir el default (4096-8192 por modelo) o hacerlo configurable por hive; leer `stop_reason` de la respuesta y devolver error explícito de truncamiento (o al menos warn + marker) cuando sea `max_tokens`, especialmente antes de validar structured output o despachar tools.

### MEDIA

**M1. El retry loop parsea el body como JSON ANTES de decidir el retry**
`crates/fluxbee_ai_sdk/src/llm.rs:1050-1056`
`response.json().await?` corre antes de `anthropic_should_retry(...)`. Un 502/503/529 de proxy/LB con body HTML (caso común) hace fallar el `.json()` y el `?` propaga de inmediato — se saltea TODO el retry loop y se pierde el envelope con status/request-id. La política de retries (que en sí es conforme al SDK Go) queda derrotada exactamente en el caso que más la necesita.
**Fix:** leer el body como texto, decidir retry por status+headers primero, parsear JSON solo en los paths de éxito/error-final; tratar un fallo de lectura de body en status retryable como retryable.

**M2. El FAILED_CONFIG mandatado casi nunca dispara en el upgrade real**
`nodes/ai/ai-generic/src/bin/ai_node_runner.rs:5137` (+ `:249`, `:4559-4677`)
El runner viejo materializaba `behavior.provider="openai"` en TODA config persistida. El `EffectiveBehaviorSection` nuevo es `deny_unknown_fields`, así que esos docs fallan la deserialización y `load_persisted_dynamic_config` los colapsa con `.ok()` a "no hay config persistida" → el nodo bootea UNCONFIGURED (o re-adopta el spawn file, perdiendo `config_version`) **en silencio, sin log y sin el FAILED_CONFIG que §3.5/E3 mandata**. El operador pierde la señal de diagnóstico para recuperar.
**Fix:** distinguir "archivo existe pero falla el parse estricto" de "archivo ausente": parseo leniente primero (versions + config cruda), y en fallo de schema entrar a `FailedConfig` con el doc crudo como `effective_config` y el `config_version` almacenado (espejo del branch `:4587-4605`).

**M3. Todo error HTTP de Anthropic colapsa a `provider_error retryable:false` (ai-generic + frontdesk-gov)**
`nodes/ai/ai-generic/src/bin/ai_node_runner.rs:5691` y `nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs:3902`
`parse_openai_status_error` matchea solo el marker literal `"openai error status="`; el adapter Anthropic emite `"anthropic error status=... type=... request_id=..."`. Bajo hive Anthropic: 401 nunca es `provider_auth_error`, 429 post-agotamiento nunca es `provider_rate_limited retryable:true`, 5xx nunca es `provider_unavailable`, y los rechazos de attachments nunca clasifican. Se pierden status/request-id/detail del envelope.
**Fix:** que el SDK devuelva un error TIPADO (`ProviderStatusError { provider, status, request_id, detail }`) en vez de que los consumidores string-scrapeen `Protocol`; como mínimo, enseñar ambos markers al parser en los dos runners.

**M4. B5 marcado `[x]` pero el fallback de structured-output por capability NO existe**
`crates/fluxbee_ai_sdk/src/llm.rs:1162-1173`
`output_config.format=json_schema` se manda incondicionalmente para cualquier modelo Anthropic. Un modelo sin la capability recibe un 400 crudo (string `Protocol` opaco) — el helper `build_output_schema_fallback_instruction` existe (`llm.rs:211`) pero solo está cableado a la rama con tools de ai-generic. El spec dice "structured output Anthropic **o fallback explícito por capability**".
**Fix:** ante 400 que indique output_config no soportado, reintentar una vez sin `output_config` con la instrucción de fallback en system (la validación local `validate_structured_output` ya cubre la verificación); o mantener un capability-check explícito con error tipado.

**M5. Sin normalización de roles: historia que empieza en assistant → 400 determinístico**
`crates/fluxbee_ai_sdk/src/llm.rs:1354-1416` (+ `function_calling.rs:298-312`)
`build_anthropic_function_messages` mapea 1:1 sin chequear que el primer mensaje sea `user` (requisito de la API de Anthropic). `normalize_recent_interactions` trunca la ventana de immediate-memory con `saturating_sub` sin importar en qué rol arranca → aproximadamente la mitad de las paridades de una conversación larga alternada produce un request que 400ea.
**Fix:** en el builder, dropear (o convertir a nota de system) los assistant iniciales para que `messages[0]` sea siempre user; o truncar la ventana a un boundary user-first.

**M6. La respuesta de chat de architect hardcodea `"provider":"openai"`**
`src/bin/sy_architect.rs:10295`
`handle_ai_chat` construye el modelo con `runtime.provider` (correcto) pero responde `json!({... "provider": "openai" ...})` con el literal. En hive Anthropic el contrato miente: provider=openai con modelo Anthropic. Violación directa de D5/§7 en un checkbox marcado hecho.
**Fix:** `"provider": runtime.provider.to_string()`.

### BAJA

**B1. Fallback silencioso a openai en el interest handler ante hive.yaml inválido** — `sy_admin.rs:2783-2794` y `sy_architect.rs:6778-6789`: `load_hive(...).ok()...unwrap_or_else(HiveAiConfig::fallback)` traga tanto el error de load como el de `effective()`. El boot sí propaga error (`?`); esto solo pega si hive.yaml se invalida post-boot, pero es exactamente el "fallback silencioso" que §3.3 prohíbe y desincroniza el interest del provider de boot. **Fix:** cachear el `EffectiveAiEngine` de boot (como hace cognition) o loguear error y saltear el evento.

**B2. Cluster de strings solo-OpenAI que mienten bajo Anthropic** — mensajes estáticos que nombran OpenAI incondicionalmente: error not-configured del executor de admin (`sy_admin.rs:2470`), mensajes de CONFIG_SET/rechazo de architect (`sy_architect.rs:11367`+), mensajes de frontdesk-gov (`ai_node_runner.rs:1901`+), mensaje de rechazo de secretos de ai-generic (`:1795`), y 2 líneas del runbook (`docs/ai-nodes-deploy-runbook.md:44,252` — subsumible en F1). **Fix:** interpolar el provider efectivo / redactar provider-neutral.

**B3. `image/gif` ruteado como Document → error engañoso** — `text_payload.rs:466-468`: `is_openai_image_mime` (png/jpeg/webp) gatea la rama Image del builder compartido; un gif cae a Document y el adapter Anthropic lo rechaza con "unsupported document" pese a que su rama de imagen acepta gif. **Fix:** predicado de imagen provider-neutral que incluya gif.

**B4. `Retry-After` en forma HTTP-date no parseado** — `llm.rs:1101-1107`: solo se parsea la forma numérica; la forma fecha (RFC 9110, manejada por el SDK Go de referencia) se ignora en silencio (cae a backoff exponencial — seguro pero no conforme). **Fix:** parse httpdate → delta-a-now, clampeado al cap de 60s.

**B5. `ResolvedAiCredential` deriva `Debug` con la key en claro** — `ai_node_runner.rs:393`. Sin call-site `{:?}` hoy (hazard latente), pero inconsistente con los patrones del repo (Debug manual redactado de `PublicationRuntime`; los runtimes de admin/architect derivan solo Clone). **Fix:** Debug manual redactado o newtype para la key.

**B6. El body de error de OpenAI (que en 401 ecoa la key parcialmente enmascarada) fluye a logs y al mesh** — `llm.rs:427` (+`:823`): el error embebe el body completo; `ai_node_runner.rs` lo loguea (`error = %err`) y forwardea hasta 280 chars como `provider_detail` en el envelope de respuesta. El path Anthropic es más limpio (extrae solo type/request_id/message). **Fix:** en 401/403 extraer solo type/message (como Anthropic) o scrubbear tokens `sk-…`.

**B7. Cognition persiste provider=openai/gpt-5.5 en su state file bajo hive Anthropic** — `sy_cognition.rs:517` (+`:2632`): persiste los defaults serde-skip y recién después estampa en memoria el efectivo, sin re-persistir. El CONFIG_GET/STATUS vivo es correcto; el archivo y un log engañan. **Fix:** estampar antes de persistir.

**B8. Todo CONFIG_SET rechazado responde `state=UNCONFIGURED`** — `ai_node_runner.rs:1905-1918`: un nodo CONFIGURED que sigue corriendo su behavior previo (o uno en FAILED_CONFIG) reporta UNCONFIGURED en el rechazo. STATUS/PING no se afectan. **Fix:** responder el lifecycle real.

**B9. Key con `resource_type` no-AI / metadata ausente / credencial vacía / vault caído colapsan al mismo `missing_ai_api_key`** — `ai_node_runner.rs:1598-1625`. Known-open (restatement de AIENG-C4); registrado acá para que C4 lo cubra con granularidad (provider-mismatch vs outage transitorio, retryable correcto).

### INFO

- **I1.** `CONFIG_GET` de ai-generic no devuelve provider derivado ni disponibilidad real de la key (§3.5 lo declara parte del contrato cerrado, pero es AIENG-C3 abierto — known-open).
- **I2.** El catálogo/ejemplos de vault del executor de admin solo enumera shapes openai/postgres/slack — sin guía para crear la key anthropic (known-open AIENG-F2).
- **I3.** `generate_stream` es pseudo-stream buffered (default del trait, un solo Delta) para ambos providers — deliberado y uniforme; documentar o implementar SSE solo si alguna vez se necesita entrega incremental.
- **I4.** Los binarios `ai_local_probe` (ai-generic y frontdesk-gov) siguen siendo OpenAI-only con `OpenAiResponsesClient` directo — residual de B8; flag `--provider` o documentarlos como OpenAI-only.
- **I5.** Asimetría de refresh ante flip del hive: admin/architect re-leen hive.yaml por evento (flip en vivo), cognition/frontdesk usan el provider estampado en boot (flip requiere restart). Cada uno es internamente consistente; alinear o documentar.
- **I6.** `behavior.base_url` es provider-agnóstico: rotar la key de openai→anthropic sin limpiar un base_url viejo manda la key nueva al host viejo (falla visible, pero la key viaja). Subsumir en el E2E de rotación (AIENG-F3) y en el fix de A1.
- **I7.** El alias `open_ai` del fix `429a197` cubre solo deserialización serde; `FromStr` y la comparación de metadata de vault no lo aceptan — una key cuya metadata se escribió como `open_ai` durante la ventana rota se rechaza con log claro. Real pero marginal (ventana de horas, no deployada).

## Refutados (verificación adversarial)

- `open-ai-alias-serde-fromstr-asymmetry` (como defecto de diseño): la asimetría es deliberada — el alias es read-only para configs escritas durante la ventana rota; el remanente marginal quedó como I7.
- `vault-src-ilk-self-asserted`: el `meta.src_ilk` auto-aserto que sustenta la autorización del vault es la postura PRE-EXISTENTE de Model D' (canonicalizado por el router, no ligado a la conexión) — no fue introducido por este trabajo. Nota: el path `get(key)` arbitrario de `AI.*` lo vuelve más load-bearing; vale tenerlo presente cuando se revisite peer-auth del plano de identidad.

## Relación con el backlog abierto del spec

Los checkboxes abiertos (B9 mock-HTTP tests, C3 diagnóstico redacted, C4 edge cases de credencial, D6 tests por consumidor, E5 E2E ambos providers, F1-F4 docs/E2E/gate) siguen abiertos y esta auditoría lo confirma — varios hallazgos BAJA/INFO son exactamente el trabajo que esos ítems cubrirían. Los hallazgos M1-M6 y A1-A2, en cambio, contradicen checkboxes **marcados hechos** (B4/B5/B3-adjacente, E3, D5) o el contrato cerrado, y no están cubiertos por ningún ítem abierto.

## Orden de resolución sugerido

1. **A1** (base_url) — es el único con impacto de seguridad directo; fix chico (validación/remoción del campo).
2. **A2** (max_tokens + stop_reason) — el fallo de producción más probable bajo Anthropic; fix chico.
3. **M3** (clasificación de errores, ideal con error tipado en el SDK) — desbloquea diagnóstico correcto para todo lo demás.
4. **M1, M5** (retry body-parse; normalización de roles) — robustez del adapter, fixes chicos.
5. **M2** (FAILED_CONFIG en upgrade) + **M6** (provider hardcodeado) + **B1/B2** (mentiras bajo Anthropic).
6. **M4** (fallback por capability) — o des-marcar B5 y decidir si se quiere.
7. El resto (BAJA/INFO) puede viajar con los checkboxes abiertos C3/C4/D6/F1-F3.
