# Windows self-update spike (before implementing `platform/windows.rs`)

The Unix self-update design (rename-based swap + finalize-on-next-boot + boot-gate
rollback) is expected to generalize to Windows, with only the restart mechanism
differing. This spike confirms that **before** we implement `platform/windows.rs`,
so we don't discover a forced rearchitecture late.

Needs a Windows environment (a Proxmox Windows VM or a Windows box) — not runnable
from the current macOS/Linux tooling. Timebox: a few hours.

## The two assumptions to confirm

1. **A running `.exe` can be moved.** Windows locks a running executable against
   *overwrite/delete*, but `MoveFileEx` (i.e. `std::fs::rename`) should still let
   us rename it. If true, the Unix swap shape works verbatim:
   `rename(exe → exe.prev)` → write new `exe` → (delete `exe.prev` on the next
   boot, once the old process has exited — which `finalize_pending_update`
   already does).
2. **Restart into the new binary works without `exec()`.** Windows has no
   `exec()`. Confirm that, under the chosen supervisor, either:
   - the service exits and the manager starts a fresh process (mirrors our
     supervised `restart`), or
   - an explicit SCM "restart" achieves the same,
   and that the fresh process runs the *new* binary at the same path.

## Seam contract `platform/windows.rs` must satisfy

Same trait as `platform/unix.rs` (`PlatformOps`):
- `swap_binary(current_exe, new_bytes) -> Result<prev_path, UpdateError>`: stage
  a temp file, `rename(exe → prev)`, `rename(temp → exe)`; on failure restore
  `prev`. No `chmod` (Windows uses ACLs). Return the retained `prev` path.
- `restart(current_exe, supervised)`: `supervised` → exit (SCM restarts) so the
  boot-gate counts the boot; non-supervised → spawn a new process at
  `current_exe` with the same args and exit. Never returns on success.

Everything else (download, sha256 verify, `pending_update`/`bootAttempts`,
`finalize_pending_update`, the boot-gate, persistent backoff) is OS-agnostic and
already done — Windows only fills these two methods.

## Procedure

1. Cross-compile or build the adapter for `x86_64-pc-windows-msvc`.
2. Run it under a supervisor: a **Windows Service** (via a `windows-service`
   wrapper) or, for the spike only, a **Scheduled Task** set to restart.
3. Point it at the mock cloud (`scripts/adapter-update-mock-cloud.py`) serving a
   v2 artifact + a `required` directive (same as the Linux harness).
4. Observe: does the adapter rename its running `.exe`, write v2, restart, and
   come up as v2? Then repeat with a crash-looping v2 (`--features
   test-crash-on-boot`) and confirm the boot-gate rolls back to v1.

## Decision matrix

| Result | Action |
|---|---|
| Both assumptions hold | Implement `windows.rs` per the contract above (rename dance + spawn/SCM restart). No rearchitecture. |
| Rename-of-running-exe fails | Swap must happen while stopped → move swap into an external updater/bootstrapper invoked by the SCM on stop; the seam absorbs it (Unix stays in-process). |
| Restart can't reach the new binary | Standardize on SCM-driven restart (or an updater process) for the supervised path; keep the trait, change only the Windows `restart` impl. |

## Out of scope for the spike
Authenticode signing (Windows prerequisite — AV/SmartScreen will fight an
unsigned self-swapped exe; lands with the signing pipeline, #4), the MSI/exe
installer, and the full SCM service wrapper. The spike only de-risks the
swap + restart mechanics.
