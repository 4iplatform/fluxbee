# SY.frontdesk.gov

El frontdesk de identidad: completa el registro de una persona que llegó con un ILK temporal y la
registra en SY.identity (`ILK_REGISTER`).

## Qué es

- Un **nodo de sistema** del core, en motherbee: `sy-frontdesk-gov.service`, binario
  `/usr/bin/sy-frontdesk-gov`, arrancado sin argumentos. **No** es un runtime ni una instancia de
  `ai.generic`, y se actualiza con el `.deb`.
- **Autónomo:**
  - trae su prompt embebido;
  - el proveedor y el modelo salen de la sección `ai` de `hive.yaml`;
  - la clave sale de SY.vault por `resource_type` (tenant raíz).
  - `CONFIG_SET` no acepta campos de config: solo vuelve a leer la clave del vault.
- **Dos entradas:**
  - `frontdesk_handoff`: datos ya estructurados, por ejemplo `register_human` de io.cloud. Funciona
    sin LLM, aunque falte la clave.
  - conversacional: el LLM junta los datos y llama a la tool `ilk_register`.
- Lo compartido con otros componentes `.gov` vive en `nodes/gov/common` (`gov-common`).
- Los logs no llevan datos personales: del registro se loguea `ilk_id`, `tenant_id` y qué campos
  vinieron, nunca sus valores.

## Compilar y probar

Desde la raíz del repo:

```bash
cargo test -p sy-frontdesk-gov -p gov-common
cargo build --release -p sy-frontdesk-gov --bin sy-frontdesk-gov
```

Para correrlo hace falta un router y un `hive.yaml` (de ahí toma su nombre,
`SY.frontdesk.gov@<hive>`).

## Pendiente

El camino de tenants todavía no cierra con lo que dicen los documentos: el tenant del caso, los
tenants nuevos `pending` y el merge por email. El detalle está en `lab/logbook/FINDINGS.md`.
