# Plan de Convergencia: NodeClient (Split sender/receiver, RX/TX y reconexión)

**Fecha:** 2026-01-30  
**Alcance:** `crates/jsr_client` + call sites (router y nodos SY/RT)  
**Objetivo:** Converger el código a la spec vigente (`docs/02-protocolo.md`, sección 10).

---

## 1. Estado actual vs Spec

| Aspecto | Spec (lo que escribimos) | Código actual | Gap |
|---|---|---|---|
| Modelo split | `NodeSender` + `NodeReceiver` (tupla) | `NodeClient` único | Grande |
| Modelo RX/TX | Colas mpsc + tasks desacopladas | Socket directo | Grande |
| API pública | `sender.send()` + `receiver.recv()` | `send/recv(&mut self)` | Grande |
| `recv_timeout` | Existe | No existe | Agregar |
| `try_recv` | Existe | No existe | Agregar |
| Reconnect | Automático en librería | Solo al inicio | Grande |
| Errores | `Timeout`, `Disconnected`, `HandshakeFailed` | No definidos | Agregar |
| `close()` | Envía `WITHDRAW` | No existe | Agregar |

---

## 2. Decisiones cerradas

- **Split explícito:** `connect()` retorna `(NodeSender, NodeReceiver)`; no hay `NodeClient` único en la API final.
- **Sin interior mutability:** el split resuelve `&self` vs `&mut self`.
- **Backpressure:** colas `mpsc` de tamaño fijo (256).
- **Reconexión automática:** backoff exponencial; al reconectar se vacía la TX queue.
- **Close limpio:** `NodeSender::close()` envía `WITHDRAW` antes de cerrar.
- **Errores definidos:** `Disconnected`, `Timeout`, `HandshakeFailed`, `Io`, `Json`.
- **Referencias:** spec `docs/02-protocolo.md` (sección 10) y modelo `tokio::net::TcpStream::into_split`.

---

## 3. Estrategia de migración (por fases)

### Fase 1 — Estructuras nuevas (sin romper nada)
- Introducir `NodeSender` / `NodeReceiver` y `ConnectionManager` interno.
- Mantener compat temporal mientras se agrega la nueva API.

### Fase 2 — Infraestructura RX/TX
- Implementar colas `mpsc` + tasks `rx_loop`/`tx_loop`.
- Centralizar framing y serialización en las tasks internas.
- Exponer estados de conexión (connected/reconnecting).

### Fase 3 — API completa (split model)
- Implementar `send`, `recv`, `recv_timeout`, `try_recv`, `close`, getters.
- Alinear errores con la spec.
- `connect()` retorna tupla; preparar migración total.

### Fase 4 — Reconexión automática
- Backoff exponencial (100ms → 30s).
- Re-HELLO/ANNOUNCE al reconectar.
- Vaciado explícito de TX queue al caer el socket.

### Fase 5 — Migración de call sites
- Actualizar `src/bin/sy_admin.rs`, `src/bin/sy_config_routes.rs` y demás nodos.
- Ajustar firmas, uso de `NodeSender`/`NodeReceiver`, manejo de errores.

### Fase 6 — Limpieza
- Eliminar APIs transitorias y referencias a `NodeClient`.
- Actualizar ejemplos y documentación local.

---

## 4. Orden de PRs sugerido (para minimizar riesgo)

1) **PR1:** Nuevas estructuras (`NodeSender/NodeReceiver/ConnectionManager`).  
2) **PR2:** RX/TX queues + tasks internas (sin cambiar call sites).  
3) **PR3:** API completa split + errores nuevos.  
4) **PR4:** Reconexión automática + métricas de estado.  
5) **PR5:** Migración de call sites.  
6) **PR6:** Limpieza y eliminación de APIs transitorias.

---

## 5. Riesgos conocidos

- Cambios de firma impactan varios binarios y crates dependientes.
- Reconexión automática puede romper expectativas si duplica envíos.
- Buffering en TX necesita límites estrictos para evitar crecimiento sin control.

---

## 6. Referencias

- Spec: `docs/02-protocolo.md` (sección 10)
- Tokio: `tokio::net::TcpStream::into_split` (documentación oficial)
