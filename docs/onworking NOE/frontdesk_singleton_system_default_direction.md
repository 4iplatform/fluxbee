# Frontdesk Singleton System-Default Direction

Fecha: 2026-04-06
Estado: propuesta de refinamiento sobre estado actual del repo

## 1. Objetivo

Dejar explícita la dirección de producto/arquitectura para `AI.frontdesk.gov` a partir del estado actual del repo:

- se parece más a `SY.architect` en ownership y bootstrap,
- pero no necesita convertirse en `SY.*`,
- y no debe seguir pensándose como "un AI node más" ni como `AI.common` con otro prompt.

Este documento no cambia el contrato actual del core.
Ordena la lectura de lo ya documentado y separa:

- lo que ya favorece esta dirección,
- lo que sigue heredado del modelo viejo,
- y lo que habría que redefinir después.

## 2. Síntesis propuesta

Dirección propuesta:

- `AI.frontdesk.gov` sigue siendo formalmente un runtime/nodo no-`SY`.
- Debe tratarse como un nodo `system-default` por hive.
- Debe existir una sola instancia canónica por hive.
- Su prompt y flujo base deben pertenecer al runtime `AI.frontdesk.gov`.
- No debe depender de que el operador cargue el prompt funcional por `CONFIG_SET`.
- Las secrets/keys deben seguir separadas del prompt y persistidas como secreto local del nodo.

En términos prácticos:

- `AI.frontdesk.gov` no debe seguir pensándose como `AI.common` + config.
- Debe pensarse como runtime singleton especializado del sistema.

## 3. Evidencia del repo que ya favorece esta dirección

### 3.1 Frontdesk ya está tratado como nodo especial del sistema

La spec vigente ya lo define así:

- `AI.frontdesk.gov` es un nodo AI no-`SY`, parte del "system default set": [ai-frontdesk-gov-spec.md](d:/repos/json-router/docs/ai-frontdesk-gov-spec.md)
- Tiene nombre L2 fijo:
  - `AI.frontdesk.gov`
  - `AI.frontdesk.gov@<hive_id>`

Esto ya favorece una instancia canónica por hive, no un pool replicable genérico.

### 3.2 El core/router ya lo trata como destino especial

El core ya tiene una referencia canónica a "identity frontdesk":

- `government.identity_frontdesk` en config de hive: [mod.rs](d:/repos/json-router/src/config/mod.rs)
- routing forzado cuando `registration_status=temporary`: [mod.rs](d:/repos/json-router/src/router/mod.rs)
- autorización explícita en `SY.identity` para `ILK_REGISTER`, `ILK_ADD_CHANNEL` y `TNT_CREATE` del frontdesk configurado: [sy_identity.rs](d:/repos/json-router/src/bin/sy_identity.rs)

Esto se parece más a un nodo sistémico singleton que a un `AI.*` arbitrario.

### 3.3 La separación por runtime ya está documentada

El repo ya dejó atrás la dirección vieja de "mismo runner con flag gov" como objetivo final:

- separación por runtime, no por `AI_NODE_MODE`: [README.md](d:/repos/json-router/nodes/ai/README.md)
- `AI.frontdesk.gov` con ownership propio y no mezclado en `AI.common`: [README.md](d:/repos/json-router/nodes/gov/ai-frontdesk-gov/README.md)
- el runbook AI ya dice:
  - el prompt/flujo de `AI.frontdesk.gov` pertenece al runtime frontdesk
  - `AI.common` no debe transportar lógica gov/frontdesk
  : [ai-nodes-deploy-runbook.md](d:/repos/json-router/docs/ai-nodes-deploy-runbook.md)

## 4. Qué quedó heredado del modelo viejo

Todavía hay partes del repo que responden a la etapa anterior, donde frontdesk era tratado como un AI node más:

- la spec actual todavía dice "usa el lifecycle estándar de AI Nodes": [ai-frontdesk-gov-spec.md](d:/repos/json-router/docs/ai-frontdesk-gov-spec.md)
- el runbook operativo actual sigue planteando `publish/update/spawn` con el flujo normal de runtimes IA: [ai-nodes-deploy-runbook.md](d:/repos/json-router/docs/ai-nodes-deploy-runbook.md)
- el ejemplo de config de frontdesk mete el prompt funcional completo en config mutable: [ai_frontdesk_gov.config.json](d:/repos/json-router/docs/examples/ai_frontdesk_gov.config.json)
- varios documentos viejos todavía reflejan la etapa `mode=gov`: [frontdesk-ai-nodes-implementation-spec.md](d:/repos/json-router/docs/onworking%20NOE/frontdesk-ai-nodes-implementation-spec.md)

Nada de eso invalida la nueva dirección.
Solo muestra que deploy/bootstrap/config todavía no fueron realineados del todo.

## 5. Comparación útil con `SY.architect`

Lo que sí conviene tomar de `SY.architect`:

- prompt base perteneciente al nodo/runtime, no a config mutable del operador
- separación entre comportamiento base y secrets
- comportamiento estable aunque la key no esté configurada todavía
- identidad operacional fuerte del nodo dentro del sistema

Lo que no hace falta copiar literalmente:

- no hace falta convertir frontdesk a `SY.*`
- no hace falta desplegarlo exactamente como binario core estilo `sy-architect`
- no hace falta meterlo dentro del mismo lifecycle que los daemons core

Conclusión:

- "parecerse a architect" debe leerse como ownership/bootstrapping,
- no como cambio automático de clase a `SY.*`.

## 6. Dirección recomendada

Lectura recomendada del repo a partir de ahora:

- `AI.frontdesk.gov` = runtime singleton system-default por hive
- no replicable por diseño operativo normal
- su comportamiento base vive con el runtime
- `CONFIG_SET` queda para configuración operativa/secrets, no para transportar el prompt funcional principal

## 7. Implicancias prácticas

### 7.1 Prompt

El prompt funcional de frontdesk debería vivir:

- en código del runtime, o
- en assets versionados dentro del package/runtime `AI.frontdesk.gov`

No debería vivir como dependencia obligatoria de:

- `config.behavior.instructions` cargado por deploy manual,
- o un ejemplo JSON copiado por operador para que el nodo "cobre identidad".

### 7.2 Keys

Las keys de provider no necesitan cambiar de modelo.
La dirección vigente del repo ya es razonable:

- `config.secrets.openai.api_key`
- persistencia local en `secrets.json`
- hot reload por `CONFIG_SET`

Eso ya está alineado con la idea de separar prompt de secreto.

### 7.3 Deploy

El deploy actual por `publish/update/spawn` debe leerse como estado heredado, no como decisión cerrada.

Lo importante a redefinir después no es primero el mecanismo exacto, sino el contrato esperado:

- frontdesk sale con el sistema,
- hay una sola instancia canónica por hive,
- no depende de inyectarle el prompt funcional por config mutable.

## 8. Criterio de lectura recomendado para próximos cambios

Hasta que se redefina formalmente el deploy/bootstrap:

- si hay conflicto entre "frontdesk como AI node genérico" y "frontdesk como singleton system-default", tomar esta segunda lectura como dirección objetivo,
- manteniendo compatibilidad con el core actual.

## 9. Tareas derivadas

- [ ] Definir documentalmente el contrato objetivo de deploy/bootstrap para `AI.frontdesk.gov` como singleton system-default por hive.
- [x] Mover el prompt funcional principal de frontdesk fuera de config operativa mutable.
- [ ] Revisar si el deploy actual debe seguir usando el camino general `deploy-ia-node.sh` o si necesita un camino de instalación/base distinto.
- [ ] Actualizar la spec de frontdesk para reflejar:
  - singleton por hive,
  - ownership del prompt por runtime,
  - y separación entre prompt base vs secrets/config operativa.
- [ ] Revisar si el nombre canónico debe seguir siendo `AI.frontdesk.gov` o si el core/doc histórica todavía asume `AI.frontdesk`.
