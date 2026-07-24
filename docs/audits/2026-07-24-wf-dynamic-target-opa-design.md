# Diseño — target dinámico en WF y su impacto en OPA/router (2026-07-24)

**Pregunta:** hacer  del WF dinámico (computado en runtime) — ¿abre agujero de OPA/autoridad en el router? Análisis multi-agente (23 agentes, verificado).

## Respuesta

Neither cleanly. "Which nodes a WF may message" is enforced by the WF publish-time target check ONLY — and that check (go/pkg/wfcel/validate.go:200 -> IsValidL2Name at :168) is nothing more than a regex on the L2-name string (`^[A-Za-z][A-Za-z0-9_.-]*@...$`). It is a syntactic well-formedness test, not an allowlist, capability, tenant, or SY.* exclusion; a static WF today can already declare target=SY.vault@motherbee or another tenant's AI node and pass. The router does NOT re-authorize user-kind src->dst paths via OPA at delivery. WF send_message emits Destination::Unicast(action.Target) with meta.type="user" (execSendMessage, actions.go:116-152). The user-layer OPA (resolve_target_with_identity, mod.rs:4982) is consulted ONLY on Destination::Resolve (target-less selection), never on an explicit Unicast target. The SYSTEM-authority OPA gate (authorize_system in serialize_for_local_delivery, mod.rs:6120-6149) fires ONLY when is_system_kind(msg_type) AND is_protected_system_action — a user-kind frame skips it entirely and is serialized/delivered unconditionally. So for the AI-host routing case the router's only runtime gate is vpn_allows_between (mod.rs:5674-5700), which for two ordinary app nodes reduces to src_vpn==dst_vpn (coarse VPN/tenant co-membership), and even returns true unconditionally when the destination name starts with SY./RT. (mod.rs:5696) — bypassing VPN for system-node targets. Bottom line: option (b) does NOT hold for user-kind WF traffic; the publish-time literal is the only per-node artifact, and it was only ever a syntax check + a human-auditable static value.

## Qué autoriza OPA/router hoy sobre un mensaje user del WF

Concretely, on a WF-originated frame at delivery the router does the following. (1) resolve_by_name resolves the L2 name via the FIB — pure lookup, no policy. (2) vpn_allows_between(meta, src, src_vpn, dst, dst_vpn) is the sole authorization predicate on the Unicast data path (mod.rs:1124-1158, 3120, 3921-3993): it returns true if either endpoint is a system node (SY./RT., line 5696), or if the frame is system-kind AND global-scope (line 5693); otherwise it requires src_vpn==dst_vpn (line 5699). No per-source/per-target identity, role, or allowlist is consulted. (3) serialize_for_local_delivery (mod.rs:6114) runs the OPA-backed authority gate authorize_system(action, src_l2_name, hive_id) ONLY inside `if is_system_kind(msg_type)` AND `if is_protected_system_action(action)`. That gate keys on the router-authoritative src_l2_name (overwritten from the authenticated source registry at mod.rs:1017/3104/3892), NOT on the target; a WF (role not in the authority table) is denied for any of the 25 PROTECTED_SYSTEM_ACTIONS regardless of target. But a meta.type="user" frame never reaches this block, and a system-kind frame naming a NON-protected verb (e.g. VAULT_GET) is also delivered uninspected — authorization then rests entirely on the destination node's own caller-authz (e.g. sy_vault self-authorizes by src_ilk, which a WF frame lacks). So: OPA authorizes ONLY system-kind protected-action frames, keyed on origin not destination; user-kind WF sends get only VPN segmentation.

## Veredicto: partial

## Diseño recomendado

Add a runtime destination gate that does not exist today; do NOT rely on the router for it. Two viable placements: (A) WF-runtime allowlist — after resolving the $ref/CEL target in execSendMessage, re-run IsValidL2Name on the RESOLVED string AND check it against a per-WF-definition (or per-tenant/ilk) allowlist of permitted destinations declared in the WorkflowDefinition, rejecting SY.*/RT.* names and anything off the list before Dispatcher.SendMsg. This restores publish-time enumerability (the allowlist is the auditable static set) while letting the chosen target be computed at runtime. (B) Router delivery-time user_allow — wire a genuine per-node authority check for user-kind Unicast frames at the delivery gate, keyed on the authoritative src_l2_name + resolved dst (the "user_allow" half the composition doc at system_policy.rs:18 anticipates but never wired in), analogous to the io.cloud confused-deputy fix. Regardless of A or B, the resolved value MUST be constrained to (1) exclude SY.*/RT. system-node names (they bypass VPN at mod.rs:5696) and (2) stay within the WF's own tenant/VPN if cross-tenant containment matters — because a system+global-scope frame also bypasses VPN (mod.rs:5693). Recommend A as primary (cheap, local, restores auditability, matches the "declarative per-node manifest" pattern) with the resolved-target re-validation being mandatory; B as the defense-in-depth backstop if the WF runtime is itself in the threat model.

## Opciones

- **Per-WF-definition target allowlist enforced in execSendMessage (resolve $ref, then re-run IsValidL2Name + membership check + reject SY./RT.) before SendMsg**

  Trade-off: Cheapest, local to wf-generic, restores publish-time enumerability as the allowlist; but it is a WF-layer (not router) control, so a compromised/buggy WF runtime could bypass it — acceptable if the WF runtime is trusted, not if it is in the threat model.

- **Router delivery-time user_allow OPA gate for user-kind Unicast, keyed on authoritative src_l2_name + resolved dst (wire the currently-unwired user side of final_allow)**

  Trade-off: True router-enforced boundary independent of WF integrity and directly analogous to the io.cloud fix; but touches the hot router path, requires an operator-managed Rego policy + baked wasm, and adds latency/complexity to EVERY user-kind delivery, not just WF sends.

- **Tenant/VPN scoping only: constrain resolved target to the WF's own VPN and forbid SY./RT. names, relying on existing vpn_allows_between for the rest**

  Trade-off: Minimal code; blocks cross-tenant and system-node reach. But it is coarse: within one VPN a dynamic target can still reach ANY node, so intra-tenant blast radius (any AI/IO/app node in the segment) is unrestricted — no per-node containment.

- **Keep target STATIC; achieve routing via Destination::Resolve so the existing user-layer OPA (resolve_target_with_identity) selects among operator-approved candidates**

  Trade-off: Reuses the one place OPA already influences user-kind delivery and keeps an auditable candidate set in Rego; but it is a larger change to the WF send model (Resolve vs Unicast), OPA there is a SELECTOR not a hard deny, and it constrains the 'AI host picks a specialist' UX to a pre-enumerated candidate list.

## Puntos a decidir (charla)

- The static-target check was NEVER a security allowlist — it is a regex only. So dynamic target does not remove an authority gate that existed; it removes publish-time ENUMERABILITY (a reviewer can no longer see the fixed set of nodes each WF may reach). Confirm that auditability, not a broken authority boundary, is the thing we are protecting.
- There is NO router OPA authorization on user-kind WF->AI sends today. The only runtime bound is VPN co-membership. Decide whether that coarse VPN boundary is acceptable containment, or whether we need a per-node/per-tenant destination allowlist.
- Where should the new gate live: in the WF runtime (cheap, restores auditability, but trusts the WF process) or in the router delivery gate (true boundary, but hot-path + operator-managed Rego)? This mirrors the io.cloud confused-deputy decision.
- Mandatory regardless of choice: the resolved dynamic target MUST re-run IsValidL2Name and MUST be forbidden from resolving to SY.*/RT. names — those bypass VPN isolation at the router (mod.rs:5696), so a computed target could reach a system node cross-VPN.
- Cross-tenant question: do WF and its candidate AI specialists share a VPN? If tenant==VPN and they are co-located, VPN membership gives essentially no restriction; if cross-tenant routing is intended, we need an explicit same-tenant/VPN check on the resolved target since neither the router authority gate nor publish validation constrains a user-typed target's tenant.
- Note the naming/identity adjacency: authorize_system grants broad SYSTEM authority to role WF.orch.diag (system_policy.rs:134). Ensure no dynamic-target wf-generic instance is ever named into an authorized SY./WF.orch.diag role, or it would additionally gain protected-action authority.
- Also note execSendMessage DEFAULTS msgType to 'system' (actions.go:116). A dynamic-target WF that leaves type unset and sets scope=global can emit system-kind+global frames that bypass VPN (mod.rs:5693) — decide whether to force meta.type='user' for the AI-host routing action.
---

## Decisión (user, 2026-07-24)

**A (ahora, wf-generic runtime):** target dinámico + al resolver: re-validar IsValidL2Name, prohibir
`SY.*`/`RT.*`, forzar `meta.type=user`, allowlist opcional por-definición (enumerabilidad).

**B (FOLLOW-UP OBLIGATORIO — no olvidar — en el Rego de OPA system del router):**
1. **Denegar `WF.*` → `SY.vault` (y SY.* system nodes)** — cierra el hueco pre-existente (hoy un WF,
   estático o dinámico, puede targetear SY.vault; solo lo frena la autz del propio vault por src_ilk).
   El user confirmó: va en el CER/Rego de OPA system.
2. **Enforce same-tenant para sends del WF**: permitido si `dst.tenant == wf.tenant` **O** si
   `wf.tenant == ROOT/DEFAULT SYSTEM TENANT` (excepción para WF de sistema — secundario). VPN ≠ tenant:
   esto se enforcea por TENANT (identity), no por VPN co-membership.

Ubicación de B: el gate OPA "user_allow" del router (la mitad no cableada de `final_allow` en
system_policy.rs:18), keyed en src_l2_name autoritativo (=el WF) + dst resuelto + sus tenants.
Análogo al fix del confused-deputy de io.cloud.
