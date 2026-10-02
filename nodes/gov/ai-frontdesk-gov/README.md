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
- **Tenant:** el registro va al tenant del caso, el que SY.identity tiene para el ILK temporal (el
  frontdesk lo lee del SHM de identity). El `tenant_id` de un handoff solo tiene que coincidir. El
  frontdesk nunca crea tenants, ni toma uno de la persona o del LLM; sin tenant responde
  `TENANT_UNRESOLVED` y no registra nada.
- **Tenant raíz:** nadie se registra ahí. Si el caso es del tenant raíz, responde
  `TENANT_NOT_REGISTRABLE` (`error_code` `TENANT_ROOT_NOT_REGISTRABLE`) sin llamar a SY.identity
  ni al LLM, y la persona sigue temporal. SY.identity rechaza lo mismo para cualquier llamador.
- **Email ya registrado:** si el email ya es de otra persona del tenant, SY.identity hace el merge
  (el canal pasa a ese ILK y se completan solo los datos que le faltaban) y el frontdesk responde
  `MERGED`.
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

- Probar que la persona es dueña del email (por ejemplo un código de un solo uso) antes del merge:
  hoy quien escribe el email de otro asocia su canal al ILK de esa persona.
- Las personas que llegan por nodos IO del tenant raíz (las instancias base de
  `fluxbee-firstboot`) quedan temporales: decisión del operador pendiente.
- Los contadores de la spec (§12) y los reintentos conversacionales.

El contrato completo está en `docs/ai-frontdesk-gov-spec.md`.
