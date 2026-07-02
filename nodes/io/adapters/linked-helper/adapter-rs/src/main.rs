mod cli;
mod cloud_client;
mod discovery;
mod platform;
mod report_client;
mod runtime_db;
mod self_update;
mod state;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use cli::{AdapterCli, AdapterCommand, DiscoverMode};
use clap::Parser;
use cloud_client::{
    enroll_adapter,
    send_alive,
    send_discovery,
    AdapterAliveLinkedHelperPayload,
    AdapterAliveRequest,
    AdapterAliveServicePayload,
    AdapterDiscoveryRequest,
    AdapterDiscoveryRequestItem,
    AdapterEnrollRequest,
    AdapterHttpError,
    AdapterUpdateDirective,
    AdapterUpdateTarget,
};

/// The authoritative adapter version: the compiled binary's own version. This
/// is what is reported to Cloud (so it stays truthful across a self-update) and
/// what the update directive is compared against.
pub const ADAPTER_VERSION: &str = env!("CARGO_PKG_VERSION");
use discovery::{
    build_manual_discovery_item,
    scan_linkedhelper_partitions,
    ScanResult,
    ScannedDiscoveryInstance,
};
use platform::PlatformOps;
use sha2::{Digest, Sha256};
use serde_json::Value;
use state::{
    AdapterDesiredBindingState,
    read_adapter_state_with_runtime,
    runtime_db_path_from_state_path,
    write_adapter_state,
    AdapterCloudDiscoveredInstanceState,
    AdapterPendingUpdate,
    AdapterState,
    AdapterUpdateRecord,
};
use time::OffsetDateTime;

/**
 * Entry point for the Rust adapter.
 */
fn main() {
    if let Err(error) = run() {
        eprintln!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": false,
                "error": error.to_string(),
            }))
            .unwrap_or_else(|_| format!("{{\"ok\":false,\"error\":\"{}\"}}", error))
        );
        std::process::exit(1);
    }
}

/**
 * Routes CLI commands to the adapter workflows.
 */
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = AdapterCli::parse();
    let state_path = resolve_state_path(cli.state_file.as_deref());

    match cli.command {
        AdapterCommand::Start {
            cloud,
            token,
            display_name,
            device_hint,
            version,
            partitions_root,
            interval_seconds,
            once,
        } => {
            let mut state = if state_path.exists() {
                if token.is_some() || cloud.is_some() || display_name.is_some() || device_hint.is_some() || version.is_some() {
                    return Err(format!(
                        "Adapter bootstrap already exists at [{}]. `start` will not reenroll implicitly. Remove the local state files or use a dedicated future reenroll flow.",
                        state_path.display()
                    )
                    .into());
                }
                read_adapter_state_with_runtime(&state_path)?
            } else {
                let cloud = cloud.ok_or_else(|| {
                    format!(
                        "Missing --cloud for first start because no adapter state exists at [{}].",
                        state_path.display()
                    )
                })?;
                let token = token.ok_or_else(|| {
                    format!(
                        "Missing --token for first start because no adapter state exists at [{}].",
                        state_path.display()
                    )
                })?;

                let result = enroll_adapter(
                    &cloud,
                    &AdapterEnrollRequest {
                        enrollment_token: token,
                        adapter_type: "linkedhelper".to_string(),
                        display_name,
                        device_hint,
                        version: version.unwrap_or_else(|| "0.1.0-rust-service".to_string()),
                    },
                )?;

                let state = AdapterState::from_enroll_result(&cloud, result.result);
                persist_adapter_state_artifacts(&state_path, &state)?;
                state
            };

            run_service_loop(
                &mut state,
                &state_path,
                partitions_root,
                interval_seconds,
                once,
            )?;
        }
        AdapterCommand::Enroll {
            cloud,
            token,
            display_name,
            device_hint,
            version,
            force,
        } => {
            if state_path.exists() && !force {
                return Err(format!(
                    "Adapter state already exists at [{}]. Use --force to overwrite the local enrollment state.",
                    state_path.display()
                )
                .into());
            }

            let result = enroll_adapter(
                &cloud,
                &AdapterEnrollRequest {
                    enrollment_token: token,
                    adapter_type: "linkedhelper".to_string(),
                    display_name,
                    device_hint,
                    version: version.unwrap_or_else(|| "0.1.0-rust-service".to_string()),
                },
            )?;

            let state = AdapterState::from_enroll_result(&cloud, result.result);
            persist_adapter_state_artifacts(&state_path, &state)?;

            print_json(&serde_json::json!({
                "ok": true,
                "action": "enrolled",
                "stateFile": state_path,
                "adapterId": state.adapter_id,
                "tenantId": state.tenant_id,
                "discoveryUrl": state.sync_config.discovery_url,
                "aliveUrl": state.alive_url(),
            }))?;
        }
        AdapterCommand::Status => {
            let state = read_adapter_state_with_runtime(&state_path)?;
            let runtime_db_path = runtime_db_path_from_state_path(&state_path);
            let runtime_db_snapshot = runtime_db::load_runtime_snapshot(&runtime_db_path)?;
            print_json(&serde_json::json!({
                "ok": true,
                "action": "status",
                "stateFile": state_path,
                "runtimeDbFile": runtime_db_path,
                "runtimeDb": runtime_db_snapshot.as_ref().map(|snapshot| serde_json::json!({
                    "runtimeMeta": snapshot.runtime_meta,
                    "desiredBindings": snapshot.desired_bindings.iter().map(|binding| serde_json::json!({
                        "localInstanceId": binding.local_instance_id,
                        "managedInstanceId": binding.managed_instance_id,
                        "status": binding.status,
                    })).collect::<Vec<_>>(),
                    "instanceRuntimeState": snapshot.instance_runtime_states.iter().map(|instance| serde_json::json!({
                        "localInstanceId": instance.local_instance_id,
                        "managedInstanceId": instance.managed_instance_id,
                        "effectiveStatus": instance.effective_status,
                        "lastEventAt": instance.last_event_at,
                        "lastSentAt": instance.last_sent_at,
                        "lastAckedAt": instance.last_acked_at,
                        "lastCheckpointTs": instance.last_checkpoint_ts,
                        "lastCheckpointCursor": instance.last_checkpoint_cursor,
                        "lastRuntimeErrorCode": instance.last_runtime_error_code,
                        "lastRuntimeErrorMessage": instance.last_runtime_error_message,
                    })).collect::<Vec<_>>(),
                    "syncCheckpoints": snapshot.sync_checkpoints.iter().map(|checkpoint| serde_json::json!({
                        "localInstanceId": checkpoint.local_instance_id,
                        "channel": checkpoint.channel,
                        "checkpointType": checkpoint.checkpoint_type,
                        "checkpointValue": checkpoint.checkpoint_value,
                        "lastConfirmedSentAt": checkpoint.last_confirmed_sent_at,
                    })).collect::<Vec<_>>(),
                })),
                "state": state,
            }))?;
        }
        AdapterCommand::Discover {
            mode:
                DiscoverMode::Manual {
                    instance_id,
                    instance_path,
                    account_display_name,
                    account_email,
                    account_fingerprint,
                },
        } => {
            let state = read_adapter_state_with_runtime(&state_path)?;
            let item = build_manual_discovery_item(
                instance_id,
                instance_path,
                account_display_name,
                account_email,
                account_fingerprint,
            );
            let result = send_discovery(
                &state.sync_config.discovery_url,
                &state.adapter_secret,
                &AdapterDiscoveryRequest {
                    adapter_type: "linkedhelper".to_string(),
                    instances: vec![item],
                },
            )?;

            print_json(&serde_json::json!({
                "ok": true,
                "action": "discovery_sent",
                "adapterId": state.adapter_id,
                "tenantId": state.tenant_id,
                "result": result.result,
            }))?;
        }
        AdapterCommand::Discover {
            mode: DiscoverMode::PayloadFile { payload_file },
        } => {
            let state = read_adapter_state_with_runtime(&state_path)?;
            let payload_text = fs::read_to_string(&payload_file)?;
            let payload: AdapterDiscoveryRequest = serde_json::from_str(&payload_text)?;
            let result = send_discovery(
                &state.sync_config.discovery_url,
                &state.adapter_secret,
                &payload,
            )?;

            print_json(&serde_json::json!({
                "ok": true,
                "action": "discovery_sent",
                "adapterId": state.adapter_id,
                "tenantId": state.tenant_id,
                "result": result.result,
            }))?;
        }
        AdapterCommand::Alive { partitions_root } => {
            let mut state = read_adapter_state_with_runtime(&state_path)?;
            let started_at = Instant::now();
            let cycle = run_alive_cycle(
                &mut state,
                &state_path,
                partitions_root.as_deref(),
                started_at,
                false,
            )?;

            print_json(&serde_json::json!({
                "ok": true,
                "action": "alive_sent",
                "adapterId": state.adapter_id,
                "tenantId": state.tenant_id,
                "status": cycle.status,
                "lhRootStatus": cycle.lh_root_status,
                "instancesCount": cycle.instances_count,
                "compatibility": cycle.alive_response.compatibility,
            }))?;
        }
        AdapterCommand::Scan { partitions_root } => {
            let scanned = scan_linkedhelper_partitions(&partitions_root)?;
            let discovery_items: Vec<AdapterDiscoveryRequestItem> = scanned
                .instances
                .iter()
                .map(scanned_instance_to_request)
                .collect();

            print_json(&serde_json::json!({
                "ok": true,
                "action": "scan_completed",
                "partitionsRoot": partitions_root,
                "instances": discovery_items,
                "warnings": scanned.warnings,
            }))?;
        }
        AdapterCommand::DiscoverScan { partitions_root } => {
            let state = read_adapter_state_with_runtime(&state_path)?;
            let scanned = scan_linkedhelper_partitions(&partitions_root)?;
            let discovery_items: Vec<AdapterDiscoveryRequestItem> = scanned
                .instances
                .iter()
                .map(scanned_instance_to_request)
                .collect();

            let result = send_discovery(
                &state.sync_config.discovery_url,
                &state.adapter_secret,
                &AdapterDiscoveryRequest {
                    adapter_type: "linkedhelper".to_string(),
                    instances: discovery_items,
                },
            )?;

            print_json(&serde_json::json!({
                "ok": true,
                "action": "scan_and_discovery_sent",
                "adapterId": state.adapter_id,
                "tenantId": state.tenant_id,
                "warnings": scanned.warnings,
                "result": result.result,
            }))?;
        }
        AdapterCommand::Run {
            partitions_root,
            interval_seconds,
            once,
        } => {
            let mut state = read_adapter_state_with_runtime(&state_path)?;
            run_service_loop(
                &mut state,
                &state_path,
                partitions_root,
                interval_seconds,
                once,
            )?;
        }
    }

    Ok(())
}

struct AliveCycleResult {
    status: String,
    lh_root_status: String,
    instances_count: usize,
    discovery_sent: bool,
    warnings: Vec<String>,
    alive_response: cloud_client::AdapterAliveResponse,
    node_reports: Vec<Value>,
}

/**
 * Runs the normal adapter service loop using an already loaded adapter state.
 */
// The `test-crash-on-boot` fault-injection feature exits mid-function, making
// the remainder unreachable; silence the resulting warnings only in that build.
#[cfg_attr(
    feature = "test-crash-on-boot",
    allow(unreachable_code, unused_variables, unused_mut, unused_assignments)
)]
fn run_service_loop(
    state: &mut AdapterState,
    state_path: &Path,
    partitions_root: Option<String>,
    interval_seconds: Option<u64>,
    once: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(value) = interval_seconds {
        state.poll_interval_seconds = value.max(5);
        persist_adapter_state_artifacts(state_path, state)?;
    }

    // Keep the persisted version truthful to the running binary (e.g. after a
    // self-update re-exec). The reported version already uses ADAPTER_VERSION.
    if state.adapter_version != ADAPTER_VERSION {
        state.adapter_version = ADAPTER_VERSION.to_string();
        persist_adapter_state_artifacts(state_path, state)?;
    }

    // If a self-update swapped this binary in, count this boot and, if it has
    // crash-looped past the max, roll back to the retained previous binary
    // (this diverges — restarts into the restored binary). Otherwise the update
    // is finalized after the first healthy alive below.
    handle_pending_update_boot(state, state_path)?;

    // Test-only fault injection (compiled only with the `test-crash-on-boot`
    // feature): crash right after the boot-gate has counted this boot, so the
    // supervised-rollback path can be exercised end-to-end.
    #[cfg(feature = "test-crash-on-boot")]
    {
        eprintln!("test-crash-on-boot: intentional exit after boot-gate");
        std::process::exit(101);
    }

    let mut update_finalized = state.runtime.pending_update.is_none();
    // Release ids that failed to apply this process lifetime (backoff) and
    // non-required upgrade offers already logged once.
    let mut failed_update_releases: HashSet<String> = HashSet::new();
    let mut announced_update_releases: HashSet<String> = HashSet::new();

    let started_at = Instant::now();
    loop {
        let cycle = run_alive_cycle(
            state,
            state_path,
            partitions_root.as_deref(),
            started_at,
            true,
        )?;

        if !update_finalized {
            finalize_pending_update(state, state_path)?;
            update_finalized = true;
        }

        print_json(&serde_json::json!({
            "ok": true,
            "action": "run_cycle_completed",
            "adapterId": state.adapter_id,
            "tenantId": state.tenant_id,
            "status": cycle.status,
            "lhRootStatus": cycle.lh_root_status,
            "instancesCount": cycle.instances_count,
            "discoverySent": cycle.discovery_sent,
            "desiredStateChanged": cycle.alive_response.desired_state_changed,
            "desiredStateVersion": cycle.alive_response.desired_state_version,
            "compatibility": cycle.alive_response.compatibility,
            "warnings": cycle.warnings,
            "nodeReports": cycle.node_reports,
        }))?;

        // Evaluate the Cloud update directive. A required update that applies
        // successfully re-execs and never returns here.
        maybe_apply_update(
            state,
            state_path,
            &cycle.alive_response.update,
            &mut failed_update_releases,
            &mut announced_update_releases,
        );

        if once {
            break;
        }

        thread::sleep(Duration::from_secs(state.poll_interval_seconds.max(5)));
    }

    Ok(())
}

/**
 * Applies the Cloud update directive: `required` updates are applied (download,
 * verify, swap, re-exec); non-required `available` offers are logged once. A
 * release that fails is skipped for the rest of this process lifetime; the next
 * restart re-evaluates it.
 */
fn maybe_apply_update(
    state: &mut AdapterState,
    state_path: &Path,
    directive: &AdapterUpdateDirective,
    failed_releases: &mut HashSet<String>,
    announced_releases: &mut HashSet<String>,
) {
    let Some(target) = directive.target.as_ref() else {
        return;
    };
    if target.version == ADAPTER_VERSION {
        return;
    }
    if failed_releases.contains(&target.release_id) {
        return;
    }
    // Persistent backoff: a release that already failed or was rolled back stays
    // skipped across restarts (so a rolled-back update is not re-attempted in a
    // loop) until Cloud publishes a different release id.
    if let Some(last) = &state.runtime.last_update {
        if last.release_id == target.release_id
            && matches!(last.result.as_str(), "failed" | "rolled_back")
        {
            return;
        }
    }

    if directive.required {
        if let Err(error) = try_apply_update(state, state_path, target) {
            eprintln!(
                "self-update: required update to {} ({}) failed: {}",
                target.version, target.release_id, error
            );
            failed_releases.insert(target.release_id.clone());
        }
    } else if directive.available && announced_releases.insert(target.release_id.clone()) {
        let _ = print_json(&serde_json::json!({
            "ok": true,
            "action": "update_available",
            "adapterId": state.adapter_id,
            "currentVersion": ADAPTER_VERSION,
            "availableVersion": target.version,
            "releaseId": target.release_id,
            "reason": directive.reason,
        }));
    }
}

/**
 * Downloads, verifies, and swaps in a target release, then re-execs into the
 * new binary. Any pre-swap failure leaves the current binary untouched; a
 * re-exec failure rolls back to the retained previous binary. Records the
 * outcome in runtime state either way.
 */
fn try_apply_update(
    state: &mut AdapterState,
    state_path: &Path,
    target: &AdapterUpdateTarget,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!(
        "self-update: applying required update {} ({})",
        target.version, target.release_id
    );
    state.runtime.adapter_status = Some(String::from("update_applying"));
    persist_adapter_state_artifacts(state_path, state)?;

    let exe = std::env::current_exe()?;

    // Download + verify + swap. Any error here leaves the running binary intact.
    let prepared = (|| {
        let url = self_update::resolve_download_url(&state.cloud_base_url, &target.url);
        let bytes = self_update::download_artifact(&url, &state.adapter_secret)?;
        self_update::verify_artifact(&bytes, target)?;
        platform::current().swap_binary(&exe, &bytes)
    })();

    let prev = match prepared {
        Ok(prev) => prev,
        Err(error) => {
            record_update_failure(state, state_path, target, "failed", &error.to_string())?;
            return Err(Box::new(error));
        }
    };

    // Binary swapped: mark the in-flight update so the new process can finalize
    // it (or the boot-gate can roll it back if it crash-loops).
    state.runtime.pending_update = Some(AdapterPendingUpdate {
        release_id: target.release_id.clone(),
        from_version: ADAPTER_VERSION.to_string(),
        to_version: target.version.clone(),
        prev_binary_path: prev.to_string_lossy().to_string(),
        started_at: current_timestamp_iso(),
        boot_attempts: 0,
    });
    persist_adapter_state_artifacts(state_path, state)?;

    // Restart into the new binary. Under a supervisor this exits and never
    // returns; otherwise it re-execs in place. A returned error means the
    // restart itself failed, so roll back to the retained previous binary.
    let error = platform::current().restart(&exe, platform::running_supervised());
    let _ = self_update::restore_prev(&exe, &prev);
    state.runtime.pending_update = None;
    record_update_failure(state, state_path, target, "rolled_back", &error.to_string())?;
    Err(Box::new(error))
}

/**
 * Max number of consecutive boots with a pending self-update marker before the
 * boot-gate concludes the new binary is crash-looping and rolls back.
 */
const MAX_UPDATE_BOOT_ATTEMPTS: u32 = 3;

/// Pure boot-gate decision: whether a pending update that has booted this many
/// times should be rolled back (crash-loop guard) rather than given another try.
fn boot_gate_should_rollback(boot_attempts: u32) -> bool {
    boot_attempts >= MAX_UPDATE_BOOT_ATTEMPTS
}

/**
 * Boot-gate for a pending self-update (supervised rollback). Called once at the
 * start of the run loop:
 * - no pending marker → nothing to do;
 * - the marker's boot count has reached the max → the new binary is crash-looping:
 *   restore the retained previous binary, record `rolled_back`, and restart into
 *   it (diverges under a supervisor);
 * - otherwise → increment the boot count and continue (the update is finalized
 *   after the first healthy alive).
 *
 * Best-effort: this only auto-rolls-back when running under a supervisor that
 * restarts the process; a binary that crashes before this code runs is out of
 * scope for phase 1 (documented in the runbook).
 */
fn handle_pending_update_boot(
    state: &mut AdapterState,
    state_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(pending) = state.runtime.pending_update.clone() else {
        return Ok(());
    };

    if boot_gate_should_rollback(pending.boot_attempts) {
        eprintln!(
            "self-update: new binary crash-looped ({} boots); rolling back to {}",
            pending.boot_attempts, pending.prev_binary_path
        );
        let exe = std::env::current_exe()?;
        let prev = Path::new(&pending.prev_binary_path);
        state.runtime.pending_update = None;
        state.runtime.last_update = Some(AdapterUpdateRecord {
            release_id: pending.release_id.clone(),
            from_version: pending.from_version.clone(),
            to_version: pending.to_version.clone(),
            applied_at: current_timestamp_iso(),
            result: String::from("rolled_back"),
            error: Some(format!("crash-looped after {} boots", pending.boot_attempts)),
        });
        state.runtime.adapter_status = Some(String::from("update_failed"));

        if let Err(error) = self_update::restore_prev(&exe, prev) {
            // Cannot restore: keep running the new binary but record the failure.
            eprintln!("self-update: rollback restore failed: {}", error);
            persist_adapter_state_artifacts(state_path, state)?;
            return Ok(());
        }
        persist_adapter_state_artifacts(state_path, state)?;

        // Restart into the restored (previous) binary. Diverges under a supervisor.
        let error = platform::current().restart(&exe, platform::running_supervised());
        eprintln!("self-update: rollback restart failed: {}", error);
        return Ok(());
    }

    if let Some(pending_mut) = state.runtime.pending_update.as_mut() {
        pending_mut.boot_attempts += 1;
    }
    persist_adapter_state_artifacts(state_path, state)?;
    Ok(())
}

/**
 * Records a failed/rolled-back self-update outcome and marks the runtime status.
 */
fn record_update_failure(
    state: &mut AdapterState,
    state_path: &Path,
    target: &AdapterUpdateTarget,
    result: &str,
    error: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    state.runtime.last_update = Some(AdapterUpdateRecord {
        release_id: target.release_id.clone(),
        from_version: ADAPTER_VERSION.to_string(),
        to_version: target.version.clone(),
        applied_at: current_timestamp_iso(),
        result: result.to_string(),
        error: Some(error.to_string()),
    });
    state.runtime.adapter_status = Some(String::from("update_failed"));
    persist_adapter_state_artifacts(state_path, state)?;
    Ok(())
}

/**
 * Finalizes a pending self-update after the new binary reaches its first healthy
 * alive: records the outcome, drops the retained previous binary, and clears the
 * marker. A version that does not match the intended target is recorded as
 * `version_mismatch` (the binary that booted is not the one we installed).
 */
fn finalize_pending_update(
    state: &mut AdapterState,
    state_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(pending) = state.runtime.pending_update.clone() else {
        return Ok(());
    };

    let result = if ADAPTER_VERSION == pending.to_version {
        "success"
    } else {
        "version_mismatch"
    };

    let _ = fs::remove_file(&pending.prev_binary_path);
    state.runtime.last_update = Some(AdapterUpdateRecord {
        release_id: pending.release_id,
        from_version: pending.from_version,
        to_version: pending.to_version,
        applied_at: current_timestamp_iso(),
        result: result.to_string(),
        error: None,
    });
    state.runtime.pending_update = None;
    persist_adapter_state_artifacts(state_path, state)?;
    eprintln!(
        "self-update: finalized ({}); now running {}",
        result, ADAPTER_VERSION
    );
    Ok(())
}

/**
 * Applies the last desired bindings snapshot received from Cloud.
 */
fn apply_alive_desired_state(
    state: &mut AdapterState,
    alive_response: &cloud_client::AdapterAliveResponse,
) {
    state.runtime.last_known_desired_state_version = Some(alive_response.desired_state_version);

    if let Some(desired_state) = &alive_response.desired_state {
        state.runtime.desired_bindings = desired_state
            .bindings
            .iter()
            .map(|binding| AdapterDesiredBindingState {
                local_instance_id: binding.local_instance_id.clone(),
                managed_instance_id: binding.managed_instance_id.clone(),
                status: binding.status.clone(),
                report_to_kind: binding.report_to.as_ref().map(|report_to| report_to.kind.clone()),
                report_to_url: binding.report_to.as_ref().map(|report_to| report_to.url.clone()),
            })
            .collect();
    }
}

/**
 * Persists the transitional JSON state and mirrors the current runtime snapshot into SQLite.
 */
fn persist_adapter_state_artifacts(
    state_path: &Path,
    state: &AdapterState,
) -> Result<(), Box<dyn std::error::Error>> {
    write_adapter_state(state_path, state)?;
    runtime_db::sync_runtime_db(&runtime_db_path_from_state_path(state_path), state)?;
    Ok(())
}

/**
 * Applies one Cloud request error to the persisted adapter runtime snapshot.
 */
fn persist_cloud_error_state(
    state: &mut AdapterState,
    state_path: &Path,
    error: &AdapterHttpError,
    fallback_status: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    state.runtime.last_error_code = Some(derive_cloud_error_code(error));
    state.runtime.last_error_message = Some(error.to_string());
    state.runtime.cloud_last_response_status = Some(format!(
        "{}_error",
        error.operation
    ));
    state.runtime.adapter_status = Some(if error.is_auth_error() {
        String::from("needs_reenrollment")
    } else {
        fallback_status.to_string()
    });
    persist_adapter_state_artifacts(state_path, state)?;
    Ok(())
}

/**
 * Executes one adapter service cycle with scan, alive, and optional incremental discovery.
 */
fn run_alive_cycle(
    state: &mut AdapterState,
    state_path: &Path,
    partitions_root_arg: Option<&str>,
    started_at: Instant,
    send_discovery_if_changed: bool,
) -> Result<AliveCycleResult, Box<dyn std::error::Error>> {
    let partitions_root = resolve_partitions_root(state, partitions_root_arg);
    let scan_started_at = current_timestamp_iso();
    let scan_result = match partitions_root.as_deref() {
        Some(path) if Path::new(path).exists() => scan_linkedhelper_partitions(path)?,
        Some(path) => ScanResult {
            instances: vec![],
            warnings: vec![format!("Configured Partitions root does not exist: {}", path)],
        },
        None => ScanResult {
            instances: vec![],
            warnings: vec![String::from(
                "Linked Helper Partitions root is not configured yet. Use --partitions-root or set lh_root_path in state.",
            )],
        },
    };

    let instances_count = scan_result.instances.len();
    let lh_root_status = if partitions_root.is_some() && scan_result.warnings.is_empty() {
        "found".to_string()
    } else if partitions_root.is_some() {
        "degraded".to_string()
    } else {
        "not_found".to_string()
    };

    let compatibility_status = derive_compatibility_status(&scan_result);
    let status = derive_adapter_status(&lh_root_status, &compatibility_status, &scan_result);
    let discovery_items: Vec<AdapterDiscoveryRequestItem> = scan_result
        .instances
        .iter()
        .map(scanned_instance_to_request)
        .collect();
    let discovery_hash = compute_discovery_hash(&discovery_items)?;
    let now = current_timestamp_iso();
    let mut discovery_sent = false;
    if send_discovery_if_changed
        && status != "needs_reenrollment"
        && state.runtime.last_discovery_hash.as_ref() != Some(&discovery_hash)
    {
        let discovery_response = match send_discovery(
            &state.sync_config.discovery_url,
            &state.adapter_secret,
            &AdapterDiscoveryRequest {
                adapter_type: state.adapter_type.clone(),
                instances: discovery_items,
            },
        ) {
            Ok(response) => response,
            Err(error) => {
                persist_cloud_error_state(state, state_path, &error, &status)?;
                return Err(Box::new(error));
            }
        };

        state.runtime.last_successful_discovery_at = Some(now.clone());
        state.runtime.last_discovery_hash = Some(discovery_hash);
        state.runtime.cloud_last_response_status = Some(String::from("discovery_accepted"));
        state.runtime.cloud_discovered_instances = discovery_response
            .result
            .items
            .iter()
            .map(|item| AdapterCloudDiscoveredInstanceState {
                discovered_instance_id: item.discovered_instance_id.clone(),
                local_instance_id: item.local_instance_id.clone(),
                status: item.status.clone(),
                managed_instance_id: item.managed_instance_id.clone(),
                report_to: item.report_to.clone(),
            })
            .collect();
        discovery_sent = true;
    }

    let alive_response = match send_alive(
        &state.alive_url(),
        &state.adapter_secret,
        &AdapterAliveRequest {
            adapter_type: state.adapter_type.clone(),
            adapter_version: ADAPTER_VERSION.to_string(),
            adapter_build: state.adapter_build.clone(),
            os: current_os(),
            arch: current_arch(),
            status: status.clone(),
            reported_at: now.clone(),
            last_known_desired_state_version: state.runtime.last_known_desired_state_version,
            service: Some(AdapterAliveServicePayload {
                mode: Some(String::from("daemon")),
                uptime_seconds: Some(started_at.elapsed().as_secs()),
                last_successful_discovery_at: state.runtime.last_successful_discovery_at.clone(),
                last_error_code: state.runtime.last_error_code.clone(),
                last_error_message: state.runtime.last_error_message.clone(),
            }),
            linkedhelper: Some(AdapterAliveLinkedHelperPayload {
                lh_root_status: Some(lh_root_status.clone()),
                lh_root_path: partitions_root.clone(),
                instances_count: Some(instances_count),
                schema_signature: Some(compute_schema_signature(&scan_result)?),
                compatibility_status: Some(compatibility_status.clone()),
                capabilities: Some(vec![
                    String::from("linkedhelper.discovery.v1"),
                    String::from("linkedhelper.li_accounts.v1"),
                    String::from("linkedhelper.summary.v1"),
                ]),
            }),
        },
    ) {
        Ok(response) => response,
        Err(error) => {
            persist_cloud_error_state(state, state_path, &error, &status)?;
            return Err(Box::new(error));
        }
    };

    apply_alive_desired_state(state, &alive_response);
    state.runtime.adapter_status = Some(status.clone());
    state.runtime.last_successful_alive_at = Some(now.clone());
    state.runtime.last_scan_at = Some(scan_started_at);
    state.runtime.last_seen_instances_count = Some(instances_count);
    state.runtime.lh_root_status = Some(lh_root_status.clone());
    state.runtime.cloud_last_response_status = Some(alive_response.adapter_status.clone());
    state.runtime.last_error_code = None;
    state.runtime.last_error_message = None;

    persist_adapter_state_artifacts(state_path, state)?;

    let node_reports = report_active_bindings_to_nodes(state, &now);

    Ok(AliveCycleResult {
        status,
        lh_root_status,
        instances_count,
        discovery_sent,
        warnings: scan_result.warnings,
        alive_response,
        node_reports,
    })
}

/**
 * Reports each active binding to its IO.linkedhelper node over the intermediate
 * `direct_node_http` path. This phase only sends heartbeats (no events yet).
 * A node being unreachable is recorded per binding and never aborts the cycle.
 */
fn report_active_bindings_to_nodes(state: &AdapterState, now: &str) -> Vec<Value> {
    let mut node_reports: Vec<Value> = Vec::new();

    for binding in &state.runtime.desired_bindings {
        if binding.status != "active" {
            continue;
        }
        let Some(kind) = binding.report_to_kind.as_deref() else {
            continue;
        };
        if kind != "direct_node_http" {
            node_reports.push(serde_json::json!({
                "localInstanceId": binding.local_instance_id,
                "managedInstanceId": binding.managed_instance_id,
                "skipped": true,
                "reason": format!("unsupported report_to.kind: {}", kind),
            }));
            continue;
        }
        let Some(url) = binding
            .report_to_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };

        let request_id = format!("hb-{}-{}", binding.local_instance_id, now);
        match report_client::send_node_heartbeat(
            url,
            &state.adapter_id,
            &state.adapter_secret,
            &binding.managed_instance_id,
            Some(&binding.local_instance_id),
            &request_id,
        ) {
            Ok(outcome) => {
                node_reports.push(serde_json::json!({
                    "localInstanceId": binding.local_instance_id,
                    "managedInstanceId": binding.managed_instance_id,
                    "url": url,
                    "ok": outcome.ok,
                    "statusCode": outcome.status_code,
                }));
            }
            Err(error) => {
                eprintln!(
                    "node report failed for binding {} ({}): {}",
                    binding.local_instance_id, url, error
                );
                node_reports.push(serde_json::json!({
                    "localInstanceId": binding.local_instance_id,
                    "managedInstanceId": binding.managed_instance_id,
                    "url": url,
                    "ok": false,
                    "statusCode": error.status_code,
                    "error": error.message,
                }));
            }
        }
    }

    node_reports
}

fn resolve_state_path(raw_state_file: Option<&str>) -> PathBuf {
    let relative = raw_state_file.unwrap_or(".linkedhelper-adapter-state.json");
    Path::new(relative).to_path_buf()
}

/**
 * Resolves the effective Partitions root, prioritizing the explicit CLI override.
 */
fn resolve_partitions_root(state: &mut AdapterState, partitions_root_arg: Option<&str>) -> Option<String> {
    if let Some(value) = partitions_root_arg {
        let normalized = value.trim().to_string();
        if !normalized.is_empty() {
            state.lh_root_path = Some(normalized.clone());
            return Some(normalized);
        }
    }

    state.lh_root_path.clone()
}

/**
 * Converts one scanned LinkedHelper instance into the Cloud discovery contract.
 */
/// Computes a stable account fingerprint from the Linked Helper account
/// identity (LinkedIn external id, else email) — independent of the local
/// instance folder — so Cloud can match/rebind the same account across an
/// adapter reinstall or a local_instance_id change. Returns None when no stable
/// identifier is available (Cloud then falls back to the technical binding).
fn compute_account_fingerprint(instance: &ScannedDiscoveryInstance) -> Option<String> {
    let basis = instance
        .li_external_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("li_external_id:{}", value.to_ascii_lowercase()))
        .or_else(|| {
            instance
                .email
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| format!("email:{}", value.to_ascii_lowercase()))
        })?;
    let mut hasher = Sha256::new();
    hasher.update(b"linkedhelper_account:v1:");
    hasher.update(basis.as_bytes());
    Some(format!("lhacct_{:x}", hasher.finalize()))
}

fn scanned_instance_to_request(instance: &ScannedDiscoveryInstance) -> AdapterDiscoveryRequestItem {
    AdapterDiscoveryRequestItem {
        local_instance_id: instance.local_instance_id.clone(),
        local_path: Some(instance.local_path.clone()),
        account_fingerprint: compute_account_fingerprint(instance),
        account_hint: Some(serde_json::json!({
            "liAccountId": instance.li_account_id,
            "displayName": instance.display_name,
            "email": instance.email,
            "avatarUrl": instance.avatar_url,
            "lastLoginAt": instance.last_login_at,
            "accountCreatedAt": instance.account_created_at,
            "accountUpdatedAt": instance.account_updated_at,
            "liExternalId": instance.li_external_id,
            "matchesLocalInstanceId": instance.matches_local_instance_id,
            "lhUserId": instance.lh_user_id,
            "lhUserExternalId": instance.lh_user_external_id,
            "lhUserLastLoginAt": instance.lh_user_last_login_at,
        })),
        metadata: Some(serde_json::json!({
            "source": "linkedhelper-partitions-scan",
            "hasLhDb": instance.has_lh_db,
            "hasPreferencesJson": instance.has_preferences_json,
            "liAccountId": instance.li_account_id,
            "liExternalId": instance.li_external_id,
            "matchesLocalInstanceId": instance.matches_local_instance_id,
            "preferencesMwState": instance.preferences_mw_state,
            "summary": {
                "chatsCount": instance.chats_count,
                "pendingMessagesCount": instance.pending_messages_count,
                "campaignsCount": instance.campaigns_count,
                "activeCampaignsCount": instance.active_campaigns_count,
                "pausedCampaignsCount": instance.paused_campaigns_count,
                "archivedCampaignsCount": instance.archived_campaigns_count,
                "hasActiveCampaigns": instance.has_active_campaigns,
            },
        })),
    }
}

/**
 * Builds a deterministic hash for the current discovery payload.
 */
fn compute_discovery_hash(
    discovery_items: &[AdapterDiscoveryRequestItem],
) -> Result<String, Box<dyn std::error::Error>> {
    let serialized = serde_json::to_string(discovery_items)?;
    Ok(sha256_hex(&serialized))
}

/**
 * Summarizes the scanned schema/data availability into a comparable signature.
 */
fn compute_schema_signature(scan_result: &ScanResult) -> Result<String, Box<dyn std::error::Error>> {
    let mut canonical: Vec<BTreeMap<String, Value>> = Vec::new();
    for instance in &scan_result.instances {
        let mut item = BTreeMap::new();
        item.insert(
            String::from("localInstanceId"),
            Value::String(instance.local_instance_id.clone()),
        );
        item.insert(String::from("hasLhDb"), Value::Bool(instance.has_lh_db));
        item.insert(
            String::from("hasPreferencesJson"),
            Value::Bool(instance.has_preferences_json),
        );
        item.insert(
            String::from("liAccountColumns"),
            serde_json::json!({
                "hasDisplayName": instance.display_name.is_some(),
                "hasEmail": instance.email.is_some(),
                "hasAvatarUrl": instance.avatar_url.is_some(),
                "hasLastLoginAt": instance.last_login_at.is_some(),
                "hasCreatedAt": instance.account_created_at.is_some(),
                "hasUpdatedAt": instance.account_updated_at.is_some(),
            }),
        );
        item.insert(
            String::from("summaryFields"),
            serde_json::json!({
                "hasChatsCount": instance.chats_count.is_some(),
                "hasPendingMessagesCount": instance.pending_messages_count.is_some(),
                "hasCampaignsCount": instance.campaigns_count.is_some(),
            }),
        );
        canonical.push(item);
    }

    let serialized = serde_json::to_string(&canonical)?;
    Ok(format!("sha256:{}", sha256_hex(&serialized)))
}

/**
 * Derives the local compatibility status from the current scan output.
 */
fn derive_compatibility_status(scan_result: &ScanResult) -> String {
    if scan_result.instances.is_empty() {
        return String::from("unknown");
    }

    // Hard incompatibility: the Linked Helper DB is present but its expected
    // schema is unreadable (no li_accounts row). This is the case Cloud must be
    // able to block on, so we emit `unsupported` (previously this fell through
    // to `unknown`, making Cloud's block path unreachable).
    if scan_result
        .instances
        .iter()
        .any(|instance| instance.has_lh_db && instance.li_account_id.is_none())
    {
        return String::from("unsupported");
    }

    if scan_result
        .instances
        .iter()
        .all(|instance| instance.has_lh_db && instance.li_account_id.is_some())
    {
        return String::from("compatible");
    }

    if scan_result.instances.iter().any(|instance| !instance.has_lh_db) {
        return String::from("degraded");
    }

    String::from("unknown")
}

/**
 * Derives the adapter runtime status combining path resolution and compatibility.
 */
fn derive_adapter_status(
    lh_root_status: &str,
    compatibility_status: &str,
    scan_result: &ScanResult,
) -> String {
    if lh_root_status == "not_found" {
        return String::from("lh_path_not_found");
    }

    if compatibility_status == "unsupported" {
        return String::from("degraded");
    }

    if !scan_result.warnings.is_empty() || compatibility_status == "degraded" {
        return String::from("degraded");
    }

    String::from("running")
}

/**
 * Maps HTTP/auth failures into one stable local error code.
 */
fn derive_cloud_error_code(error: &AdapterHttpError) -> String {
    if error.is_auth_error() {
        return String::from("cloud_auth_error");
    }

    match error.status_code {
        Some(status) => format!("cloud_http_{}", status),
        None => String::from("cloud_network_error"),
    }
}

fn current_os() -> String {
    std::env::consts::OS.to_string()
}

fn current_arch() -> String {
    std::env::consts::ARCH.to_string()
}

fn current_timestamp_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"))
}

fn sha256_hex(value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(value.as_bytes());
    format!("{:x}", digest.finalize())
}

fn print_json(value: &serde_json::Value) -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_gate_rolls_back_only_at_or_above_max() {
        assert!(!boot_gate_should_rollback(0));
        assert!(!boot_gate_should_rollback(MAX_UPDATE_BOOT_ATTEMPTS - 1));
        assert!(boot_gate_should_rollback(MAX_UPDATE_BOOT_ATTEMPTS));
        assert!(boot_gate_should_rollback(MAX_UPDATE_BOOT_ATTEMPTS + 5));
    }
}
