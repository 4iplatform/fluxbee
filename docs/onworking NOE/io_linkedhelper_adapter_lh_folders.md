# Guía / resumen de hallazgos relevantes en carpetas de Linked Helper
## Para diseño del adapter

> Documento de trabajo para conservar lo encontrado durante la inspección inicial de carpetas de Linked Helper.
>
> Objetivo: resumir qué partes de la estructura parecen útiles para el adapter, qué rol podría cumplir cada una y qué cosas no conviene tomar como fuente principal.

---

# 1. Contexto general

Linked Helper se instala en una PC y en una misma instalación pueden existir **múltiples instancias / profiles** funcionando en paralelo.

Esto refuerza la idea de diseño ya aceptada:

- **un adapter por PC**
- capaz de observar **muchas instancias/profiles**
- y de comunicarse con un único nodo `IO.linkedhelper`

---

# 2. Estructura general observada

A nivel global de la instalación se observaron carpetas/archivos del estilo:

- `Preferences`
- `Local State`
- `Cache/`
- `Local Storage/`
- `Session Storage/`
- `Network/`
- `blob_storage/`
- `SharedStorage`
- `config.json`
- `r-info.json`
- `last-backup-info.json`
- `Instances/`
- `Partitions/`

## Lectura inicial
Esto sugiere una estructura en capas:

### A. Capa global de instalación / runtime
Información general del software y del entorno local de la PC.

### B. Capa por instancia/profile
Información específica de cada cuenta/profile.

### C. Capa launcher/global operativa
Información sobre ejecución, salud de instancias y estado operativo general del software.

---

# 3. Carpeta global de Linked Helper (sin `Partitions`)

## Qué parece útil
La carpeta general sirve principalmente para:

- entender la estructura global de la instalación;
- identificar componentes del runtime;
- descubrir ubicaciones relevantes;
- y ubicar dónde cuelgan los datos específicos de las instancias.

## Qué no parece principal para el adapter
No parece ser la mejor fuente para:

- conversaciones;
- mensajes;
- estado fino por profile;
- o acciones específicas del avatar/profile.

## `Instances/`
La carpeta `Instances/` parece representar más bien:

- versión instalada del software;
- runtime empaquetado;
- binarios y recursos de la app;

y **no** los profiles/cuentas automatizables en sí.

### Conclusión
Para el adapter:
- `Instances/` no debería confundirse con instancias de profile;
- parece ser una capa de software/runtime.

---

# 4. Carpeta `Partitions`

Dentro de `Partitions` se observaron estructuras del tipo:

- `linked-helper-account-xxxxxx-main`
- `linked-helper-account-xxxxxx-content`
- `linked-helper-launcher`

donde `xxxxxx` representa el identificador de una instancia/profile particular.

Esto confirma que hay una separación clara entre:

- **datos específicos de una instancia/profile**
- y **datos operativos globales del launcher**

---

# 5. Carpeta `linked-helper-account-xxxxxx-content`

## Qué se observó
La carpeta `content` contiene principalmente artefactos del tipo:

- `Cache/`
- `Code Cache/`
- `blob_storage/`

## Interpretación
Parece corresponder más al mundo:

- renderer/webview;
- cachés;
- artefactos temporales o de interfaz;

y no a la fuente principal de datos de negocio.

## Conclusión
Para el MVP del adapter:

- **no parece una fuente principal**
- y no conviene diseñar la lógica central del adapter apoyándose en `content`

Puede llegar a servir más adelante para análisis específicos, pero no se ve como núcleo del diseño.

---

# 6. Carpeta `linked-helper-account-xxxxxx-main`

Esta carpeta sí aparece como la más relevante para el adapter.

## Elementos observados
- `lh.db`
- backups de `lh.db`
- `preferences.json`
- logs `.z`
- carpeta `errors/`
- otras carpetas auxiliares de Chromium/Electron

## Conclusión principal
`linked-helper-account-xxxxxx-main/lh.db` parece ser la **fuente principal de negocio** por profile.

---

# 7. Hallazgos relevantes en `lh.db`

## 7.1. Cuenta/profile local
Tabla observada:
- `li_accounts`

### Qué sugiere
Cada carpeta `...-main` parece corresponder a un profile concreto.
La tabla `li_accounts` parece representar justamente esa cuenta/profile local.

### Utilidad potencial para el adapter
- identificar el profile;
- enriquecer `profile_create`;
- obtener nombre visible y algunos datos básicos del profile.

---

## 7.2. Conversaciones
Tablas observadas:
- `chats`
- `chat_participants`
- `chat_meta`
- `chat_external_ids`
- `chat_messages_cursor`

### Utilidad potencial
Estas tablas parecen ser la base para:

- detectar conversaciones;
- detectar qué participants intervienen;
- relacionar conversaciones con el profile;
- y reconstruir el contexto conversacional.

---

## 7.3. Mensajes
Tablas observadas:
- `messages`
- `participant_messages`
- `participant_messages_versions`
- `message_external_ids`

### Utilidad potencial
Estas tablas parecen ser la base para:

- detectar nuevos mensajes;
- leer contenido de mensaje;
- asociar mensajes a participants y conversaciones;
- y consolidar mensajes localmente en el adapter.

### Conclusión
La decisión ya tomada de consolidar mensajes en el adapter sigue teniendo mucho sentido a la luz de esta estructura.

---

## 7.4. Mensajes pendientes / salientes
Tabla observada:
- `pending_messages`

### Utilidad potencial
Esta tabla parece especialmente importante para:

- entender el flujo de mensajes salientes;
- detectar mensajes todavía no enviados / en cola;
- y modelar mejor la ejecución de acciones en LH.

### Conclusión
`pending_messages` merece inspección más profunda cuando se baje al detalle del flujo saliente del adapter.

---

## 7.5. Personas / contactos
Tablas observadas:
- `person_mini_profile`
- `person_email`
- `person_external_ids`
- múltiples tablas `person_*`

### Utilidad potencial
Pueden servir para:

- nombre visible del contacto;
- ids externos;
- eventuales metadatos de contacto.

### Precaución
No conviene asumir de entrada que toda esa información:
- es siempre confiable;
- está siempre completa;
- o es toda necesaria para el MVP.

---

## 7.6. Campañas / acciones
Tablas observadas:
- `actions`
- `action_results`
- `campaigns`
- `campaign_actions`

### Utilidad potencial
Estas tablas podrían ser relevantes para:

- estado operativo del profile;
- automatizaciones/campañas;
- alertas vinculadas al estado de la instancia;
- y trazabilidad de acciones.

### Observación importante
Además de la DB, se mencionó la existencia en `main` de un archivo `.json` que marca el estado actual de la campaña / del avatar/profile (por ejemplo si está activa o no), y que en el servicio actual ya se usa para alertar cuando cambia ese estado.

### Conclusión
Ese tipo de archivo **sí** parece una fuente mucho más interesante para alertas operativas que los logs.

---

# 8. Carpeta `linked-helper-launcher`

## Qué se observó
Se detectó una carpeta `linked-helper-launcher` con:

- logs;
- carpeta `errors/`;
- y otros artefactos de operación global.

## Lo valioso conceptualmente
Esta carpeta parece representar el plano global operativo del software en la PC, no el detalle de una conversación puntual.

## Qué podría aportar
- estado general del launcher;
- salud / actividad de instancias;
- errores operativos globales;
- información útil para observabilidad.

## Qué NO conviene hacer como estrategia principal
No conviene diseñar el adapter apoyándose en:

- monitoreo intensivo de archivos de log;
- scraping continuo de logs;
- lectura frecuente de logs como fuente de verdad;
- o depender de eso para estados críticos.

### Motivos
- puede ser pesado;
- puede ser poco confiable;
- puede ser frágil ante cambios de formato;
- y podría generar problemas de acceso/bloqueo si se hiciera mal.

## Conclusión práctica
Los logs del launcher pueden ser útiles para:
- inspección manual;
- debugging;
- o casos puntuales de diagnóstico;

pero **no deberían ser la fuente principal de estados operativos del adapter**.

---

# 9. Qué tipos de fuentes sí parecen convenientes

## 9.1. Fuente principal por profile
`linked-helper-account-xxxxxx-main/lh.db`

Para:
- conversaciones;
- mensajes;
- contactos;
- pending messages;
- acciones/campañas;
- y consolidación.

## 9.2. Fuente operativa de estado por profile
Archivos de estado explícitos, por ejemplo:
- `.json` de estado de campaña / actividad del profile

Para:
- alertas de cambio de estado;
- saber si una instancia/profile está activa o no;
- y otros estados operativos concretos.

## 9.3. Fuente global de instalación / discovery
Carpeta general de LH y estructuras globales.

Para:
- ubicar `Partitions`;
- entender layout global;
- descubrir rutas y componentes del runtime.

## 9.4. Fuente global operativa
`linked-helper-launcher`

Para:
- observabilidad general;
- potencial health global;
- errores operativos;
- pero no como fuente principal de estado crítico.

---

# 10. Qué NO parece buena fuente principal

## 10.1. `...-content`
No parece una buena fuente principal para el adapter del MVP.

## 10.2. Logs del launcher
No parecen una buena fuente principal de estados reales del sistema.

## 10.3. Runtime global (`Instances/`)
No parece representar los profiles automatizables, sino el software/runtime.

---

# 11. Implicancias para el adapter

Con lo observado hasta ahora, el adapter debería modelarse pensando en al menos dos planos:

## 11.1. Plano global por PC
- instalación global de LH;
- discovery de estructuras;
- health/observabilidad general;
- múltiples profiles corriendo en la misma máquina.

## 11.2. Plano por profile
- `linked-helper-account-xxxxxx-main`;
- `lh.db`;
- conversaciones;
- mensajes;
- estado local del profile;
- acciones/campañas;
- y estado operativo específico.

---

# 12. Conclusiones prácticas para el diseño

## 12.1. Decisión fuerte
`lh.db` de cada `...-main` parece ser la mejor fuente principal para el MVP del adapter.

## 12.2. Consolidación
La consolidación local de mensajes en el adapter sigue estando bien orientada.

## 12.3. Alertas
Para alertas, conviene priorizar:

- archivos/estados explícitos;
- cambios detectables en DB o archivos de estado;
- y no logs como fuente primaria.

## 12.4. Launcher
El launcher puede servir para observabilidad y posiblemente discovery, pero no debería volverse el eje del adapter.

---

# 13. Qué conviene revisar más adelante

1. schema más detallado de `lh.db`
2. significado exacto de `pending_messages`
3. archivo `.json` de estado de campaña/profile en `main`
4. carpeta `errors/` de `linked-helper-launcher` (solo si se quiere afinar alertas)
5. posible relación entre ids de DB y ids visibles del profile/cuenta

---

# 14. Síntesis final

Lo encontrado hasta ahora sugiere que:

- el adapter debe pensarse como **uno por PC**;
- esa PC puede tener **múltiples profiles**;
- la carpeta global de LH sirve para **discovery/contexto de instalación**;
- `linked-helper-account-xxxxxx-main/lh.db` parece ser la **fuente principal de datos operativos por profile**;
- `linked-helper-account-xxxxxx-content` no parece central para el MVP;
- `linked-helper-launcher` puede ser útil para **observabilidad**, pero no conviene usar sus logs como fuente primaria de estados;
- y para alertas operativas parecen más prometedores los **archivos de estado explícitos** (por ejemplo JSONs de estado) y los cambios detectables en fuentes estructuradas que los logs.
