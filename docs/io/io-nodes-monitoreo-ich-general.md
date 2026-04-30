# Definición general — Monitoreo de estados de ICH en nodos IO

> Nota general derivada del trabajo sobre `IO.linkedhelper`.
>
> Objetivo: dejar asentado que el monitoreo de estados de ICH propios no es una particularidad de Linked Helper, sino una responsabilidad generalizable para nodos IO.

---

# 1. Principio general

Cada nodo IO debería poder:

- conocer cuáles son sus ICHs propios;
- observar cambios relevantes en el estado de esos ICHs;
- y tomar medidas operativas en función de esos cambios.

Esto incluye, al menos, casos como:

- habilitación de automatización;
- deshabilitación de automatización;
- otros cambios operativos relevantes del canal que afecten el comportamiento del nodo.

---

# 2. Alcance de esta responsabilidad

Esta responsabilidad implica:

- monitoreo interno del estado de ICH;
- actualización del estado observado por el nodo;
- preparación de cambios pendientes para sus consumers/adapters;
- y reacción operativa del nodo cuando corresponda.

## Ejemplos de reacción operativa
- dejar de consumir o reportar mensajes para automatización;
- volver a habilitar un flujo;
- informar cambios incrementales a un adapter externo;
- colapsar estados pendientes por ICH para evitar entregar historial redundante.

---

# 3. Lo que esta responsabilidad NO implica

No implica necesariamente que el nodo IO sea responsable de:

- exponer administrativamente el listado completo de ICHs habilitados/deshabilitados;
- exponer ILKs pendientes;
- ni ser la fuente de verdad de esos estados.

Eso puede quedar del lado de identity/core y de otros mecanismos administrativos del sistema.

---

# 4. Distinción clave

## Nodo IO
- observa;
- reacciona;
- conserva estado operativo mínimo;
- informa cambios incrementales a quien corresponda.

## Identity/Core
- mantiene la fuente de verdad del estado de ICH;
- gobierna la exposición administrativa de ese estado;
- decide los mecanismos globales para consultarlo o modificarlo.

---

# 5. Implicancia para nuevos nodos IO

Cuando se diseñe un nuevo nodo IO, conviene evaluar explícitamente:

1. qué ICHs considera propios;
2. cómo observará los cambios de estado de esos ICHs;
3. qué estado operativo mínimo deberá persistir;
4. cómo informará cambios relevantes a sus consumers/adapters;
5. y qué parte de la administración de esos estados queda fuera del nodo.
