# gov-common

Shared library for the runtimes of the `gov` family.

Goal:
- gather the reusable helpers and contracts of `sy.frontdesk.gov`,
- keep frontdesk from depending on `nodes/ai/common`,
- keep the ownership split between the `AI` family and the `gov` family.

Current state:
- base env/config helpers for a gov node,
- a shared contract for the configuration of the gov identity bridge,
- common normalization of the error payload for identity tools.

Direction:
- gradually move reusable logic from `nodes/gov/ai-frontdesk-gov` into this crate,
- keep `ai.common` and `sy.frontdesk.gov` without a cross-dependency.
