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
- [ ] S2-T1. Documentar ejemplos canónicos de `SCMD` para secrets vía `CONFIG_SET`.
- [ ] S2-T2. Documentar ejemplo canónico de `CONFIG_GET` para descubrir faltantes de secrets.
- [ ] S2-T3. Revisar logging de `SY.admin` para asegurar que payloads secretos no se impriman en claro.
- [ ] S2-T4. Revisar logging/UX de `archi` para asegurar que no refleje secrets en respuestas o historial visible.
- [ ] S2-T5. Alinear ayuda de `archi` para que entienda que keys/secrets entran por `CONFIG_SET` y no por `hive.yaml`.
- [ ] S2-T6. Definir wording estándar para respuestas de `archi` cuando un nodo reporta `configured=false` en un secret requerido.

## 4. Fase S3 - contrato de nodos

Meta:
- cada nodo sigue siendo dueño de su payload/config
- pero todos publican el mismo tipo de metadata de secrets

Checklist:
- [ ] S3-T1. Fijar contrato mínimo para `contract.secrets[*]` en `CONFIG_GET`:
  - `field`
  - `storage_key`
  - `required`
  - `configured`
  - `value_redacted`
  - `persistence`
- [ ] S3-T2. Acordar que `CONFIG_GET` nunca devuelve valores secretos en claro.
- [ ] S3-T3. Acordar que `CONFIG_SET` puede transportar secrets dentro del `payload.config` del nodo.
- [ ] S3-T4. Acordar que el nodo es responsable de separar config normal y secrets al persistir.
- [ ] S3-T5. Documentar por familia de nodos qué campos son secret-bearing.

## 5. Fase S4 - adopción inicial en `AI.common`

Meta:
- tomar el caso real más urgente: OpenAI key en nodo AI

Checklist:
- [ ] S4-T1. Definir el campo canónico del contrato AI para la OpenAI key.
- [ ] S4-T2. Hacer que `CONFIG_GET` del nodo AI declare esa key en `contract.secrets`.
- [ ] S4-T3. Persistir la key localmente en `secrets.json` usando helpers del SDK.
- [ ] S4-T4. Asegurar que el nodo AI lea primero del secret local antes de fallback legacy.
- [ ] S4-T5. Mantener compatibilidad temporal con env/legacy mientras migra.
- [ ] S4-T6. Evitar que la key aparezca en logs, `CONFIG_RESPONSE`, status o debug output.
- [ ] S4-T7. Agregar pruebas del caso:
  - set secret
  - restart node
  - secret reload/readback redacted
  - no key en logs

## 6. Fase S5 - migración operativa

Meta:
- dejar de depender de `hive.yaml` para secrets de nodos

Checklist:
- [ ] S5-T1. Inventariar qué nodos hoy leen keys/secrets desde `hive.yaml`.
- [ ] S5-T2. Priorizar migración inicial: `AI.common`.
- [ ] S5-T3. Definir estrategia de transición para instalaciones existentes.
- [ ] S5-T4. Documentar procedimiento operativo:
  - instalar nodo
  - correr `CONFIG_GET`
  - detectar faltantes
  - enviar `CONFIG_SET`
- [ ] S5-T5. Quitar examples/docs que sigan sugiriendo secrets en `hive.yaml`.

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

- [ ] existe helper estándar en `fluxbee_sdk`
- [ ] `AI.common` ya no necesita leer su key principal desde `hive.yaml`
- [ ] `CONFIG_GET` expone metadata de secrets sin revelar valores
- [ ] `CONFIG_SET` permite cargar secrets operativamente desde `archi`
- [ ] secrets no aparecen en logs, history ni status
