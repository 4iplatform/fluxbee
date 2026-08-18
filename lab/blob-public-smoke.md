# Smoke: circuito de artefactos públicos (io.blob) — publish / expiry / unpublish

Prueba E2E del circuito completo: un nodo productor (AI) genera un HTML, lo **publica**
como URL pública read-only, se **sirve** por `SY.edge`, **expira** solo por TTL, y el
operador puede **revocarlo**. Cierra el "smoke de producto" pendiente en
`docs/io-blob-spec-v1.md`.

Topología real (prod): **motherbee** corre `SY.admin` + `IO.blob` + `AI.chat`;
**ingress** corre `SY.edge`. Los bytes `public/` replican motherbee→ingress por syncthing;
el control viaja por RPC de malla. `publish_artifact` es **mesh-only** (el productor lo
llama por socket; admin resuelve su tenant desde el `src_l2_name` estampado por el router,
nunca asertado). El operador NO puede publicar por HTTP —sólo revocar.

## 0. Prerrequisitos

- Binarios nuevos desplegados en las VMs: `sy_admin`, `sy_edge`, `io-blob`, `ai_node_runner`
  (rebuild del `.deb` o hot-swap de estos cuatro). Toolchain Rust **1.92** (ethnum rompe en 1.97).
- motherbee + ingress arriba y unidos (add_ingress hecho, `info.yaml` del ingress en
  `status: connected`).
- `AI.chat@motherbee` con identidad resuelta (vault presente) — sólo entonces se registra la
  tool `publish_html_page`. Y con un proveedor LLM configurado (para que decida llamar la tool).

## 1. Config (Step 3)

Admin necesita saber a qué edge empujar la fila y con qué base construir la URL pública.
Dos vías (env gana sobre hive.yaml):

**Vía env — drop-in systemd de `sy-admin` (motherbee):**
```ini
# /etc/systemd/system/sy-admin.service.d/public-artifacts.conf
[Service]
Environment=SY_ADMIN_PUBLIC_EDGE_NODE=SY.edge@<hive-ingress>
Environment=SY_ADMIN_PUBLIC_BASE_URL=https://<ip-o-dns-ingress>:8443
```
> Si `SY_ADMIN_PUBLIC_EDGE_NODE` se omite, admin autodescubre el ingress `connected` desde
> `hives/*/info.yaml` (`resolve_public_edge_node`). `SY_ADMIN_PUBLIC_BASE_URL` sí conviene
> fijarla: sin ella la respuesta trae `public_url: null` y la tool cae al path relativo
> `/public/<key>`.

**El edge (ingress) ya escucha** en su listener público (`SY_EDGE_HTTP_LISTEN` o
`edge.listen` en hive.yaml, gate de rol=ingress). Con TLS: `/public/<key>` sobre `:8443`.

`systemctl daemon-reload && systemctl restart sy-admin` tras el drop-in.

## 2. Publicar (pata productor — la única que crea la URL)

Disparar a `AI.chat` un mensaje que pida publicar. El modelo llama `publish_html_page`
(que valida el HTML, escribe el blob, promueve, y hace `publish_artifact` a admin por socket):

> «Generá una página HTML simple autocontenida que diga "hola fluxbee" y **publicala**
>  como URL pública.»

La tool devuelve al modelo `{status:"ok", url, publication_id, expires_at}`; el modelo debe
devolver la URL al usuario en su respuesta.

**Verificar el ledger autoritativo (motherbee):**
```
GET  http://127.0.0.1:<admin>/publications        # o el listado que exponga admin
# la publicación debe estar status="published", con edge_node, tenant_id, expires_at
```

**Servir — desde una máquina que alcance el ingress:**
```
curl -k https://<ingress>:8443/public/<key>        # 200 + el HTML; content-type del row
```
> `<key>` = la cola de `url`/`public_url` que devolvió la tool. Es un capability token
> opaco de 128 bits, NO el sha256.

## 3. Expiry (GC admin-side, `run_publication_expiry_sweep`)

Publicar con TTL corto para no esperar (mínimo 60s):
- Repetir el paso 2 pidiendo una página con expiración corta, o llamar la tool con
  `expires_in_secs` bajo si se instrumenta un productor de prueba.

El reaper de admin corre cada **600s**; para forzar la ventana, esperar a que
`expires_at <= now` y al siguiente tick. Verificar:
```
curl -k https://<ingress>:8443/public/<key>        # 404 (el edge rechaza expires_at<=now
                                                    #      al servir Y su reaper local dropea la fila)
# ledger admin: status="expired", released_at seteado
```
> Defensa en profundidad: el edge dropea filas expiradas por su cuenta (al cargar y en su
> reaper); el sweep de admin ADEMÁS libera los bytes `public/` (`MSG_BLOB_RELEASE` a io.blob)
> y retira el ledger autoritativo. El `MSG_EDGE_UNPUBLISH_BLOB` es idempotente (una fila ya
> dropeada por el edge responde `removed:false` → admin lo trata como ok, no se atasca).

## 4. Unpublish (revocación del operador — D2)

Cualquier tenant, desde el HTTP localhost de admin (motherbee):
```
curl -sS -X POST http://127.0.0.1:<admin>/artifacts/unpublish \
  -H 'content-type: application/json' \
  -d '{"publication_id":"pub:<uuid>"}'
# -> {"status":"ok","payload":{"publication_id":"pub:...","unpublished":true}}
```
Verificar: `curl` al `/public/<key>` → 404; ledger admin `status="unpublished"`, `released_at` seteado.
Idempotente: repetir devuelve `unpublished:false` (ya estaba).

## 5. Criterio de éxito

- [ ] publish por tool devuelve una URL y el `curl` la sirve (200 + HTML correcto).
- [ ] el tenant servido es el del nodo productor (no asertado en el request).
- [ ] expiry: tras TTL, `curl` 404 y ledger `expired` + bytes liberados.
- [ ] unpublish operador: 404 + ledger `unpublished`; idempotente.
- [ ] el command-log de admin (`GET /hives/<hive>/commands`) muestra las entradas
      `publish_artifact` / `unpublish_artifact` con `origin` correcto.
