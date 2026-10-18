# Node Secret Config - Implementation Tasks

Status: active  
Reference spec: `docs/onworking COA/node-secret-config-spec.md`

Objetivo:
- sacar secrets/keys de `hive.yaml`
- mantener el ingreso solo por `archi` / `SCMD`
- transportar por `SY.admin` + L2 unicast usando `CONFIG_SET`
- persistir localmente por nodo en `secrets.json`
- dejar el mecanismo común en `fluxbee_sdk`

## 1. Decisiones ya cerradas

- [x] No introducir vault global en v1.
- [x] No introducir cifrado obligatorio en v1.
- [x] Mantener `CONFIG_GET` / `CONFIG_SET` como verbs del contrato.
- [x] Mantener `CONFIG_RESPONSE` como envelope de respuesta.
- [x] Persistir secrets en archivo separado de `config.json` y `state.json`.
- [x] Usar permisos `0700` para directorio y `0600` para `secrets.json`.
- [x] Mantener redaction obligatoria en lecturas, respuestas y logs.
- [x] Usar metadatos de auditoría con `updated_by_ilk` y `trace_id`.

## 2. Fase S1 - `fluxbee_sdk`

Meta:
- dejar listo el mecanismo base para que los nodos no reinventen storage, redaction ni permisos

Checklist:
- [x] S1-T1. Crear módulo canónico de secrets en `crates/fluxbee_sdk`.
- [x] S1-T2. Definir helper de path canónico para `secrets.json` bajo `/var/lib/fluxbee/nodes/<TYPE>/<node@hive>/`.
- [x] S1-T3. Implementar helper de creación de directorio con `0700`.
- [x] S1-T4. Implementar helper de creación/actualización de archivo con `0600`.
- [x] S1-T5. Implementar escritura atómica (`tmp` + rename).
- [x] S1-T6. Implementar lectura JSON tipada del contenedor de secrets.
- [x] S1-T7. Definir struct común del contenedor:
  - `schema_version`
  - `updated_at`
  - `updated_by_ilk`
  - `updated_by_label`
  - `trace_id`
  - `secrets`
- [x] S1-T8. Implementar helper de redaction para lecturas/respuestas.
- [x] S1-T9. Definir token por defecto de redaction (`***REDACTED***` o equivalente).
- [x] S1-T10. Agregar pruebas unitarias:
  - create path
  - enforce perms
  - atomic write
  - roundtrip load/save
  - redaction output

Implementado en:
- `crates/fluxbee_sdk/src/node_secret.rs`
- exportado por `crates/fluxbee_sdk/src/lib.rs`
- exportado por `crates/fluxbee_sdk/src/prelude.rs`

Verificación:
- `cargo test -p fluxbee-sdk node_secret --lib`

## 3. Fase S2 - `SY.admin` / `archi`

Meta:
- mantener el entrypoint único por `archi`
- documentar y endurecer el uso del transporte actual para secrets

Checklist:
- [x] S2-T1. Documentar ejemplos canónicos de `SCMD` para secrets vía `CONFIG_SET`.
- [x] S2-T2. Documentar ejemplo canónico de `CONFIG_GET` para descubrir faltantes de secrets.
- [x] S2-T3. Revisar logging de `SY.admin` para asegurar que payloads secretos no se impriman en claro.
- [x] S2-T4. Revisar logging/UX de `archi` para asegurar que no refleje secrets en respuestas o historial visible.
- [x] S2-T5. Alinear ayuda de `archi` para que entienda que keys/secrets entran por `CONFIG_SET` y no por `hive.yaml`.
- [ ] S2-T6. Definir wording estándar para respuestas de `archi` cuando un nodo reporta `configured=false` en un secret requerido.

Documentado / verificado en:
- `docs/onworking COA/node_secret_migration_runbook.md`
- `docs/node-config-control-plane-spec.md`
- `src/bin/sy_admin.rs`
- `src/bin/sy_architect.rs`

Avance actual en `archi`:
- [x] lectura de OpenAI key únicamente desde `secrets.json` local
- [x] `SCMD` local `GET /architect/control/config-get`
- [x] `SCMD` local `POST /architect/control/config-set`
- [x] recarga en memoria del runtime AI después de persistir la key
- [x] redaction del comando `SCMD` antes de persistirlo en el historial del chat
- [x] retiro del fallback legacy para la OpenAI key: `archi` ya no toma credenciales desde `config.json` ni `hive.yaml`

## 4. Fase S3 - contrato de nodos

Meta:
- cada nodo sigue siendo dueño de su payload/config
- pero todos publican el mismo tipo de metadata de secrets

Checklist:
- [x] S3-T1. Fijar contrato mínimo para `contract.secrets[*]` en `CONFIG_GET`:
  - `field`
  - `storage_key`
  - `required`
  - `configured`
  - `value_redacted`
  - `persistence`
- [x] S3-T2. Acordar que `CONFIG_GET` nunca devuelve valores secretos en claro.
- [x] S3-T3. Acordar que `CONFIG_SET` puede transportar secrets dentro del `payload.config` del nodo.
- [x] S3-T4. Acordar que el nodo es responsable de separar config normal y secrets al persistir.
- [ ] S3-T5. Documentar por familia de nodos qué campos son secret-bearing.
  - Estado actual: `AI` documentado; `IO` y `WF` quedan abiertos por definición de cada nodo/familia.

Documentado en:
- `docs/node-config-control-plane-spec.md`
- `docs/onworking COA/node-secret-config-spec.md`
- `docs/AI_nodes_spec.md`
- `docs/ai-nodes-examples-annex.md`

Soporte común ya disponible en:
- `crates/fluxbee_sdk/src/node_secret.rs` (`NodeSecretDescriptor`)

## 5. Fase S4 - adopción inicial en `AI.common`

Meta:
- tomar el caso real más urgente: OpenAI key en nodo AI

Checklist:
- [x] S4-T1. Definir el campo canónico del contrato AI para la OpenAI key.
- [x] S4-T2. Hacer que `CONFIG_GET` del nodo AI declare esa key en `contract.secrets`.
- [x] S4-T3. Persistir la key localmente en `secrets.json` usando helpers del SDK.
- [x] S4-T4. Asegurar que el nodo AI lea primero del secret local antes de fallback legacy.
- [x] S4-T5. Mantener compatibilidad temporal con env/legacy mientras migra.
- [x] S4-T6. Evitar que la key aparezca en logs, `CONFIG_RESPONSE`, status o debug output.
- [x] S4-T7. Agregar pruebas del caso:
  - set secret
  - restart node
  - secret reload/readback redacted
  - no key en logs

Implementado en:
- `nodes/ai/ai-generic/src/bin/ai_node_runner.rs`

Notas:
- campo canónico publicado por `CONFIG_GET`: `config.secrets.openai.api_key`
- aliases legacy todavía aceptados durante migración:
  - `config.behavior.openai.api_key`
  - `config.behavior.api_key`
- `CONFIG_GET` ahora expone `api_key_source` y `contract.secrets[*]`
- el runner persiste la key en `secrets.json`, la retira del `effective_config` y migra config legacy inline al secret local durante bootstrap cuando puede
- tests agregados en el runner AI:
  - roundtrip local de `secrets.json`
  - migración bootstrap de secret inline hacia secret local
  - validación de que el config serializado/readback ya no conserva la key inline

## 6. Fase S5 - migración operativa

Meta:
- dejar de depender de `hive.yaml` para secrets de nodos

Checklist:
- [x] S5-T1. Inventariar qué nodos hoy leen keys/secrets desde `hive.yaml`.
- [x] S5-T2. Priorizar migración inicial: `AI.common`.
- [x] S5-T3. Definir estrategia de transición para instalaciones existentes.
- [x] S5-T4. Documentar procedimiento operativo:
  - instalar nodo
  - correr `CONFIG_GET`
  - detectar faltantes
  - enviar `CONFIG_SET`
- [x] S5-T5. Quitar examples/docs que sigan sugiriendo secrets en `hive.yaml`.

Documentado en:
- `docs/onworking COA/node_secret_migration_inventory.md`
- `docs/onworking COA/node_secret_migration_runbook.md`

Limpieza documental aplicada en:
- `docs/AI_nodes_spec.md`
- `docs/ai-nodes-examples-annex.md`
- `docs/onworking NOE/node_runtime_install_ops.md`
- `docs/examples/ai_common_chat.config.json`

## 7. Fase S6 - hardening opcional v1.1

Meta:
- dejar el tema evaluado sin bloquear v1

Checklist:
- [ ] S6-T1. Evaluar cifrado at-rest transparente en SDK.
- [ ] S6-T2. Si se hace, documentar que protege exposición accidental, no acceso root.
- [ ] S6-T3. Decidir inputs de derivación local (`hive_id`, `node_name`, salt local).
- [ ] S6-T4. Definir criterio de rollback/migración si se habilita después de v1.

## 8. Orden sugerido para arrancar el lunes

1. `S1-T1` a `S1-T10` en `fluxbee_sdk`
2. `S3-T1` a `S3-T4` como contrato mínimo cerrado
3. `S4-T1` a `S4-T7` en `AI.common`
4. `S2-T1` a `S2-T6` para operación/documentación
5. `S5-T1` a `S5-T5` para limpieza y transición

## 9. Criterio de cierre v1

Se considera cerrado cuando:

- [x] existe helper estándar en `fluxbee_sdk`
- [x] `AI.common` ya no necesita leer su key principal desde `hive.yaml`
- [x] `CONFIG_GET` expone metadata de secrets sin revelar valores
- [x] `CONFIG_SET` permite cargar secrets operativamente desde `archi`
- [x] secrets no aparecen en logs, history ni status

Nota:
- `S6` queda explícitamente fuera del cierre de `v1`; cifrado at-rest se mantiene como hardening opcional `v1.1`.
