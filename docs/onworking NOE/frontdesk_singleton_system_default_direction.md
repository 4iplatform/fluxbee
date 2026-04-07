# Frontdesk Singleton System Default Direction

Fecha: 2026-04-06
Estado: propuesta de direccion

## 1. Objetivo

Dejar explicita la direccion actual para `SY.frontdesk.gov`:

- debe parecerse mas a `SY.architect` en ownership y bootstrap
- ya no debe tratarse como un `AI.*` comun
- debe considerarse nodo singleton del sistema por hive

## 2. Sintesis propuesta

Direccion propuesta:

- `SY.frontdesk.gov` se trata como nodo `SY.*`
- existe una sola instancia canonica por hive
- sale con el sistema
- su prompt y flujo base pertenecen al runtime del propio frontdesk
- no debe depender de cargar su prompt funcional por `CONFIG_SET`
- las keys siguen separadas del prompt y, por ahora, pueden entrar temporalmente por `CONFIG_SET`

## 3. Evidencia del repo que ya favorece esta direccion

- `government.identity_frontdesk` existe como referencia canonica en config de hive
- el router puede forzar destino al frontdesk cuando `registration_status=temporary`
- `SY.identity` autoriza operaciones sensibles para el frontdesk configurado
- el servicio ya forma parte del set base que orchestrator contempla

## 4. Que quedo heredado del modelo viejo

Todavia quedan rastros de la etapa previa:

- documentos que lo describian como `AI.frontdesk.gov`
- runbooks que lo trataban como runtime AI gestionado por deploy/spawn
- ejemplos donde el prompt funcional completo vivia en config mutable

## 5. Direccion recomendada

Tomar como direccion estable:

- `SY.frontdesk.gov` como nodo singleton `system-default`
- prompt base runtime-owned
- `CONFIG_SET` temporalmente permitido para keys y parametros operativos
- decision final de bootstrap/secrets pendiente de `CORE`

## 6. Implicancias practicas

### Prompt

Debe vivir:

- en codigo del runtime, o
- en assets versionados del propio nodo

No debe vivir como dependencia obligatoria de config operativa mutable.

### Keys

Solucion temporal vigente:

- `config.secrets.openai.api_key`
- persistencia en `secrets.json`
- entrada via `CONFIG_SET`

Solucion final:

- pendiente de definicion de `CORE`

### Deploy

El deploy heredado no debe leerse como contrato final.
Lo importante es la semantica:

- sale con el sistema
- hay una sola instancia por hive
- y su comportamiento base no depende de inyectar prompt por config mutable

## 7. Tareas derivadas

- [ ] Definir documentalmente el contrato objetivo de bootstrap/deploy para `SY.frontdesk.gov`
- [x] Mover el prompt funcional principal de frontdesk fuera de config operativa mutable
- [ ] Definir con `CORE` el modelo final de keys/secrets para frontdesk
- [ ] Revisar si queda algun surface/trace importante con naming viejo
