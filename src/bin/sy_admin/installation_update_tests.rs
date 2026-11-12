//! The installation's update (docs/hive-config-update-v1.md §6, "As built (stage 4)"): the decision
//! per hive and category as a table, the state on disk, the explicit run and the background task
//! against a fake mesh, and the size of the answers.

use super::*;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, AtomicUsize};

const H_OLD: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const H_NEW: &str = "2222222222222222222222222222222222222222222222222222222222222222";
const H_NEWER: &str = "3333333333333333333333333333333333333333333333333333333333333333";
const SHA_V2: &str = "bbbb";
const SHA_V3: &str = "cccc";

fn temp_state(prefix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).expect("state dir");
    dir
}

/// The target of 8.x after a release: 0.1.123's core and the file's version 3.
fn target() -> Target {
    Target {
        core: Some(CoreTarget {
            manifest_hash: H_NEW.to_string(),
            version: Some("0.1.123".to_string()),
        }),
        core_note: None,
        config: Some(ConfigTarget {
            version: 3,
            sha256: SHA_V3.to_string(),
        }),
        config_note: None,
    }
}

/// A hive's `core`: what it installed (`source`, a hash) or nothing recorded (a package install, the
/// dist hash then), its last update, and what awaits its update.
fn core(installed: Option<(&str, &str)>, dist: &str, last_update: Value, awaiting: Value) -> Value {
    json!({
        "status": "ok",
        "manifest_hash": dist,
        "components": {
            "rt-gateway": {"service": "rt-gateway", "version": "0.1.123", "build_id": "1", "sha256": "x"},
            "sy-orchestrator": {"service": "sy-orchestrator", "version": "0.1.123", "build_id": "1", "sha256": "y"},
        },
        "installed": installed.map(|(source, hash)| json!({"manifest_hash": hash, "source": source, "installed_at_ms": "1"})),
        "last_update": last_update,
        "awaiting_update": awaiting,
    })
}

/// A core update that ended and settled.
fn settled_update() -> Value {
    json!({
        "phase": "done", "status": "ok", "started_at_ms": "100", "finished_at_ms": "200",
        "updated": ["sy-orchestrator", "rt-gateway"], "left_out": [],
        "settled": {"generation": "update-1", "dropped": [], "installed": [], "pending": [], "errors": [], "settled": true, "at_ms": "300"},
    })
}

/// A hive's `config`: its copy's state and the config update it applied last; `applied: None`
/// leaves the key out (an orchestrator before 0.1.122).
fn config(state: &str, version: u64, sha: &str, applied: Option<Option<(u64, &str)>>) -> Value {
    let mut config = json!({"file": "valid", "version": version, "sha256": sha, "state": state});
    if let Some(applied) = applied {
        config["applied"] = match applied {
            Some((version, sha)) => {
                json!({"version": version, "sha256": sha, "applied_at_ms": 1, "restarted": []})
            }
            None => Value::Null,
        };
    }
    config
}

fn snapshot(hive: &str, role: &str, core: Value, config: Value) -> Value {
    json!({"hive_id": hive, "role": role, "core": core, "config": config})
}

/// A worker on the target, settled, its config applied.
fn on_target(hive: &str) -> Value {
    snapshot(
        hive,
        "worker",
        core(
            Some(("update", H_NEW)),
            H_NEW,
            settled_update(),
            Value::Null,
        ),
        config("current", 3, SHA_V3, Some(Some((3, SHA_V3)))),
    )
}

fn decide_for(snapshot: &Value, kind: RunKind, include: &[&str]) -> Decision {
    decide_known(snapshot, kind, include, &BTreeMap::new())
}

fn decide_known(
    snapshot: &Value,
    kind: RunKind,
    include: &[&str],
    known: &BTreeMap<String, String>,
) -> Decision {
    let hive = snapshot["hive_id"].as_str().expect("hive_id");
    let include: Vec<String> = include.iter().map(|hive| hive.to_string()).collect();
    decide(
        HiveFacts {
            hive,
            is_motherbee: hive == "motherbee",
            snapshot: Some(snapshot),
        },
        &target(),
        kind,
        &include,
        known,
    )
}

fn is_excluded(step: &Step, says: &str) -> bool {
    matches!(step, Step::Excluded { reason, .. } if reason.contains(says))
}

fn excluded_as(step: &Step) -> Option<Exclusion> {
    match step {
        Step::Excluded { kind, .. } => Some(*kind),
        _ => None,
    }
}

/// The decision per hive and category, as a table, for each kind of run (§6 and the brief's
/// exclusions).
#[test]
fn the_decision_per_hive_and_category() {
    let explicit = RunKind::Explicit;
    let automatic = RunKind::Automatic;

    // On the target: nothing to do, whoever runs.
    for kind in [explicit, automatic, RunKind::Background] {
        let decision = decide_for(&on_target("worker1"), kind, &[]);
        assert_eq!(
            decision,
            Decision {
                core: Step::OnTarget,
                config: Step::OnTarget
            }
        );
    }

    // The core needed (the previous release installed), the config applied already.
    let behind_core = snapshot(
        "worker1",
        "worker",
        core(
            Some(("update", H_OLD)),
            H_NEW,
            settled_update(),
            Value::Null,
        ),
        config("current", 3, SHA_V3, Some(Some((3, SHA_V3)))),
    );
    for kind in [explicit, automatic] {
        let decision = decide_for(&behind_core, kind, &[]);
        assert_eq!(decision.core, Step::Act);
        assert_eq!(decision.config, Step::OnTarget);
    }

    // The config needed: the core on the target, an older version applied.
    let behind_config = snapshot(
        "worker1",
        "worker",
        core(
            Some(("update", H_NEW)),
            H_NEW,
            settled_update(),
            Value::Null,
        ),
        config("current", 3, SHA_V3, Some(Some((2, SHA_V2)))),
    );
    assert_eq!(
        decide_for(&behind_config, explicit, &[]),
        Decision {
            core: Step::OnTarget,
            config: Step::Act
        }
    );
    // Its first config update (applied null) is one too.
    let never_applied = snapshot(
        "worker1",
        "worker",
        core(
            Some(("update", H_NEW)),
            H_NEW,
            settled_update(),
            Value::Null,
        ),
        config("current", 3, SHA_V3, Some(None)),
    );
    assert_eq!(decide_for(&never_applied, explicit, &[]).config, Step::Act);

    // Both: the core first, the config after it.
    let behind_both = snapshot(
        "worker1",
        "worker",
        core(
            Some(("update", H_OLD)),
            H_NEW,
            settled_update(),
            Value::Null,
        ),
        config("current", 3, SHA_V3, Some(Some((2, SHA_V2)))),
    );
    let decision = decide_for(&behind_both, explicit, &[]);
    assert_eq!(decision.core, Step::Act);
    assert!(matches!(decision.config, Step::AfterCore(_)));

    // Settling: the target installed, the orchestrator it installed not settled yet.
    let settling = snapshot(
        "worker1",
        "worker",
        core(
            Some(("update", H_NEW)),
            H_NEW,
            json!({"phase": "done", "status": "ok", "started_at_ms": "100", "updated": ["sy-orchestrator"]}),
            Value::Null,
        ),
        config("current", 3, SHA_V3, Some(Some((2, SHA_V2)))),
    );
    let decision = decide_for(&settling, explicit, &[]);
    assert!(matches!(decision.core, Step::InFlight(_)));
    assert!(matches!(decision.config, Step::AfterCore(_)));
    // Restarting onto an update: followed, not sent again.
    let restarting = snapshot(
        "worker1",
        "worker",
        core(
            Some(("update", H_OLD)),
            H_NEW,
            json!({"phase": "restarting", "started_at_ms": "400", "updated": ["rt-gateway"]}),
            Value::Null,
        ),
        config("current", 3, SHA_V3, Some(Some((3, SHA_V3)))),
    );
    assert!(matches!(
        decide_for(&restarting, explicit, &[]).core,
        Step::InFlight(_)
    ));
    // On the target with a binary awaiting its update: a core update installs it.
    let awaiting = snapshot(
        "worker1",
        "worker",
        core(
            Some(("update", H_NEW)),
            H_NEW,
            settled_update(),
            json!({"components": [{"component": "sy-cognition", "reason": "sha256 mismatch"}]}),
        ),
        config("current", 3, SHA_V3, Some(Some((3, SHA_V3)))),
    );
    assert_eq!(decide_for(&awaiting, explicit, &[]).core, Step::Act);

    // Rolled back by the operator (A-5): an installation-wide run keeps its core unless it names
    // the hive, and still applies its config; the automatic step and the background task leave it
    // alone, both categories.
    let rolled_back = snapshot(
        "egress1",
        "egress",
        core(
            Some(("core_rollback", H_OLD)),
            H_NEW,
            settled_update(),
            Value::Null,
        ),
        config("current", 3, SHA_V3, Some(Some((2, SHA_V2)))),
    );
    let decision = decide_for(&rolled_back, explicit, &[]);
    assert!(is_excluded(&decision.core, "rolled its core back"));
    assert_eq!(excluded_as(&decision.core), Some(Exclusion::RolledBack));
    assert_eq!(decision.config, Step::Act);
    if let Step::Excluded { next, .. } = &decision.core {
        assert!(next.contains("POST /hives/egress1/update"), "{next}");
        assert!(next.contains("include_rolled_back"), "{next}");
    }
    assert_eq!(
        decide_for(&rolled_back, explicit, &["egress1"]).core,
        Step::Act
    );
    for (kind, who) in [
        (automatic, "the automatic step leaves it alone"),
        (RunKind::Background, "the background task leaves it alone"),
    ] {
        let decision = decide_for(&rolled_back, kind, &["egress1"]);
        assert!(is_excluded(&decision.core, who));
        assert!(is_excluded(&decision.config, who));
    }
    // A rolled-back hive whose core is the target's is on it.
    let rolled_back_to_target = snapshot(
        "egress1",
        "egress",
        core(
            Some(("core_rollback", H_NEW)),
            H_NEW,
            settled_update(),
            Value::Null,
        ),
        config("current", 3, SHA_V3, Some(Some((3, SHA_V3)))),
    );
    assert_eq!(
        decide_for(&rolled_back_to_target, explicit, &[]).core,
        Step::OnTarget
    );

    // Ahead of the motherbee: its core is a newer release SY.admin saw as a target. The automatic
    // step leaves it alone; an explicit run moves it (the installation's rollback).
    let known: BTreeMap<String, String> = [(H_NEWER.to_string(), "0.1.124".to_string())].into();
    let ahead = snapshot(
        "worker1",
        "worker",
        core(
            Some(("update", H_NEWER)),
            H_NEW,
            settled_update(),
            Value::Null,
        ),
        config("current", 3, SHA_V3, Some(Some((3, SHA_V3)))),
    );
    assert_eq!(decide_known(&ahead, explicit, &[], &known).core, Step::Act);
    let decision = decide_known(&ahead, automatic, &[], &known);
    assert!(is_excluded(&decision.core, "newer than the motherbee's"));
    assert!(is_excluded(&decision.config, "newer than the motherbee's"));
    // A hash SY.admin never saw is behind, not ahead.
    assert_eq!(decide_for(&ahead, automatic, &[]).core, Step::Act);

    // A copy edited by hand: no run applies config to it; the automatic step leaves the hive alone.
    let edited = snapshot(
        "worker1",
        "worker",
        core(
            Some(("update", H_OLD)),
            H_NEW,
            settled_update(),
            Value::Null,
        ),
        config("edited_by_hand", 3, "dddd", Some(Some((2, SHA_V2)))),
    );
    let decision = decide_for(&edited, explicit, &[]);
    assert_eq!(decision.core, Step::Act);
    assert!(is_excluded(&decision.config, "edited by hand"));
    assert_eq!(excluded_as(&decision.config), Some(Exclusion::HandEdited));
    let decision = decide_for(&edited, automatic, &[]);
    assert!(is_excluded(&decision.core, "edited by hand"));
    assert!(is_excluded(&decision.config, "edited by hand"));

    // A hive behind 0.1.122 takes no category config: its core first; nothing when its core is
    // left as it is.
    let before_0_1_122 = snapshot(
        "worker1",
        "worker",
        core(
            Some(("update", H_OLD)),
            H_NEW,
            settled_update(),
            Value::Null,
        ),
        json!({"file": "valid", "version": 1, "sha256": "aaaa", "state": "behind"}),
    );
    let decision = decide_for(&before_0_1_122, explicit, &[]);
    assert_eq!(decision.core, Step::Act);
    assert!(matches!(&decision.config, Step::AfterCore(why) if why.contains("0.1.122")));
    let rolled_back_old = snapshot(
        "worker1",
        "worker",
        core(
            Some(("core_rollback", H_OLD)),
            H_NEW,
            settled_update(),
            Value::Null,
        ),
        json!({"file": "absent", "version": null, "sha256": null}),
    );
    let decision = decide_for(&rolled_back_old, explicit, &[]);
    assert!(is_excluded(&decision.core, "rolled its core back"));
    assert!(is_excluded(&decision.config, "takes no category config"));
    assert_eq!(
        excluded_as(&decision.config),
        Some(Exclusion::OldOrchestrator)
    );
    // Before 0.1.121: no config section at all.
    let no_config = json!({"hive_id": "worker1", "role": "worker",
        "core": core(Some(("update", H_OLD)), H_NEW, settled_update(), Value::Null)});
    assert!(matches!(
        decide_for(&no_config, explicit, &[]).config,
        Step::AfterCore(_)
    ));

    // The motherbee is the core's reference; its config is applied as any hive's.
    let motherbee = snapshot(
        "motherbee",
        "motherbee",
        core(None, H_OLD, Value::Null, Value::Null),
        config("current", 3, SHA_V3, Some(Some((2, SHA_V2)))),
    );
    assert_eq!(
        decide_for(&motherbee, explicit, &[]),
        Decision {
            core: Step::OnTarget,
            config: Step::Act
        }
    );

    // No /versions: nothing known.
    let unknown = decide(
        HiveFacts {
            hive: "worker1",
            is_motherbee: false,
            snapshot: None,
        },
        &target(),
        explicit,
        &[],
        &BTreeMap::new(),
    );
    assert!(matches!(unknown.core, Step::Unknown(_)));
    assert!(matches!(unknown.config, Step::Unknown(_)));

    // A target without a file to apply leaves every hive's config as it is, saying why.
    let no_file = Target {
        config: None,
        config_note: Some("the motherbee's installation file is edited by hand".to_string()),
        ..target()
    };
    let decision = decide(
        HiveFacts {
            hive: "worker1",
            is_motherbee: false,
            snapshot: Some(&behind_config),
        },
        &no_file,
        explicit,
        &[],
        &BTreeMap::new(),
    );
    assert!(is_excluded(&decision.config, "edited by hand"));
    assert_eq!(excluded_as(&decision.config), Some(Exclusion::NoTarget));
}

/// F4: a release that drops a node leaves it in `left_out`, which the new orchestrator's settle
/// never rewrites: the settle judges it, and the hive reads as settled.
#[test]
fn a_release_that_drops_a_node_reads_as_settled() {
    let dropped = json!({
        "phase": "done", "status": "ok", "started_at_ms": "100",
        "updated": ["sy-orchestrator", "rt-gateway"], "left_out": ["sy-dropped"],
        "settled": {"generation": "update-1", "dropped": ["sy-dropped"], "installed": [],
                    "pending": [], "errors": [], "settled": true, "at_ms": "300"},
    });
    let hive = core(Some(("update", H_NEW)), H_NEW, dropped, Value::Null);
    assert_eq!(core_settle(&hive), Settle::Settled);
    let worker = snapshot(
        "worker1",
        "worker",
        hive,
        config("current", 3, SHA_V3, Some(Some((3, SHA_V3)))),
    );
    assert_eq!(watch_core(&worker, H_NEW, Some("50")), CoreWatch::Settled);
    assert_eq!(
        decide_for(&worker, RunKind::Explicit, &[]).core,
        Step::OnTarget
    );
    // Without a settle to judge it (an update that did not replace the orchestrator), what it left
    // out stays open.
    let unsettled = json!({"phase": "done", "status": "ok", "started_at_ms": "100",
        "updated": ["rt-gateway"], "left_out": ["sy-dropped"]});
    assert!(matches!(
        core_settle(&core(Some(("update", H_NEW)), H_NEW, unsettled, Value::Null)),
        Settle::Open(items) if items == vec!["left_out=sy-dropped".to_string()]
    ));
}

/// The target is what the motherbee holds: its installed core (the dist hash after a package
/// install), its version, and its file while it is a version it recorded.
#[test]
fn the_target_is_what_the_motherbee_holds() {
    let motherbee = snapshot(
        "motherbee",
        "motherbee",
        core(
            None,
            &format!("sha256:{}", H_NEW.to_uppercase()),
            Value::Null,
            Value::Null,
        ),
        config("current", 3, SHA_V3, Some(Some((3, SHA_V3)))),
    );
    let found = target_of(&motherbee);
    assert!(found.same_as(&target()));
    assert_eq!(
        found.core.as_ref().and_then(|core| core.version.as_deref()),
        Some("0.1.123")
    );
    assert!(!motherbee_rolled_back(&motherbee));

    // A hand edit of its file names no config to apply, with why.
    let edited = snapshot(
        "motherbee",
        "motherbee",
        core(Some(("update", H_NEW)), H_OLD, Value::Null, Value::Null),
        config("edited_by_hand", 3, "dddd", Some(None)),
    );
    let found = target_of(&edited);
    assert_eq!(
        found.core.as_ref().map(|core| core.manifest_hash.as_str()),
        Some(H_NEW)
    );
    assert!(found.config.is_none());
    assert!(found
        .config_note
        .as_deref()
        .is_some_and(|note| note.contains("edited by hand")));
    // set_installation_ai's: the version it wrote.
    let written = target_after_write(&edited, 4, "EEEE");
    assert_eq!(
        written.config,
        Some(ConfigTarget {
            version: 4,
            sha256: "eeee".to_string()
        })
    );
    assert!(written.config_note.is_none());

    // A core that cannot be read names none.
    let broken =
        json!({"hive_id": "motherbee", "core": {"status": "error", "message": "manifest missing"}});
    let found = target_of(&broken);
    assert!(found.core.is_none());
    assert!(found
        .core_note
        .as_deref()
        .is_some_and(|note| note.contains("manifest missing")));

    // F10: a motherbee whose own core is rolled back pairs another hash with dist's version: it
    // names no target.
    let rolled_back = snapshot(
        "motherbee",
        "motherbee",
        core(
            Some(("core_rollback", H_OLD)),
            H_NEW,
            Value::Null,
            Value::Null,
        ),
        config("current", 3, SHA_V3, Some(Some((3, SHA_V3)))),
    );
    assert!(motherbee_rolled_back(&rolled_back));
}

/// One update at a time: the same target resumes it, another closes it as superseded, kept in the
/// history, which keeps the last 20, newest first; an update is done once every hive is on it, and
/// held while only the operator's rollbacks are off it.
#[test]
fn one_update_at_a_time_with_a_bounded_history() {
    let mut file = UpdatesFile::default();
    let (first, opening) = file.open_or_resume(&target(), "http", Some("deploy 0.1.123"), 1_000);
    assert_eq!(opening, Opening::Opened);
    assert_eq!(first, "upd-1000");
    let (again, opening) = file.open_or_resume(&target(), "executor", None, 2_000);
    assert_eq!(
        (again.as_str(), opening),
        (first.as_str(), Opening::Resumed)
    );
    assert_eq!(
        file.open.as_ref().map(|open| open.origin.as_str()),
        Some("http")
    );
    assert_eq!(
        file.open.as_ref().and_then(|open| open.last_run_at_ms),
        Some(2_000)
    );

    let other = Target {
        config: Some(ConfigTarget {
            version: 4,
            sha256: "dddd".to_string(),
        }),
        ..target()
    };
    let (second, opening) = file.open_or_resume(&other, "set_installation_ai", None, 3_000);
    assert_eq!(opening, Opening::Superseded(first.clone()));
    assert_eq!(file.closed.len(), 1);
    assert_eq!(file.closed[0].state, UpdateState::Superseded);
    assert_eq!(
        file.closed[0].superseded_by.as_deref(),
        Some(second.as_str())
    );
    assert_eq!(file.closed[0].closed_at_ms, Some(3_000));

    // Done once every hive is on the target, and not before.
    file.refresh_hives(
        &second,
        &["motherbee".to_string(), "worker1".to_string()],
        Vec::new(),
    );
    assert_eq!(file.settle_state(4_000), None);
    for entry in file.open.as_mut().expect("open").hives.values_mut() {
        entry.core.state = Standing::OnTarget;
        entry.config.state = Standing::OnTarget;
    }
    assert_eq!(
        file.settle_state(4_000),
        Some((second.clone(), UpdateState::Done))
    );
    assert!(file.open.is_none());
    assert_eq!(file.closed[0].state, UpdateState::Done);
    assert_eq!(
        file.current().map(|update| update.id.as_str()),
        Some(second.as_str())
    );

    // The history keeps the last 20.
    for n in 0..30u64 {
        let target = Target {
            config: Some(ConfigTarget {
                version: 10 + n,
                sha256: format!("{n:04}"),
            }),
            ..target()
        };
        file.open_or_resume(&target, "http", None, 10_000 + n);
    }
    assert_eq!(file.closed.len(), HISTORY_KEPT);
    assert!(file
        .closed
        .windows(2)
        .all(|pair| pair[0].opened_at_ms > pair[1].opened_at_ms));
    // The versions it saw as targets, by hash.
    assert_eq!(
        file.known_cores().get(H_NEW).map(String::as_str),
        Some("0.1.123")
    );
}

/// F2 (d): `held`, A-5's one allowed divergence: an open update whose only hives off the target
/// are core exclusions for the operator's rollback, each with its config on the target. Anything
/// else off the target keeps it open; a stale target never holds or closes it.
#[test]
fn an_update_is_held_by_the_operators_rollbacks_only() {
    let mut file = UpdatesFile::default();
    let (id, _) = file.open_or_resume(&target(), "http", None, 1_000);
    file.refresh_hives(
        &id,
        &[
            "motherbee".to_string(),
            "worker1".to_string(),
            "egress1".to_string(),
        ],
        Vec::new(),
    );
    let set = |file: &mut UpdatesFile, hive: &str, core: CategoryEntry, config: CategoryEntry| {
        let entry = file
            .open
            .as_mut()
            .expect("open")
            .hives
            .get_mut(hive)
            .expect("hive");
        entry.core = core;
        entry.config = config;
    };
    let on = || CategoryEntry {
        state: Standing::OnTarget,
        reason: None,
        exclusion: None,
        outcome: None,
        next: None,
    };
    let excluded = |kind: Exclusion| CategoryEntry {
        state: Standing::Excluded,
        reason: Some("why".to_string()),
        exclusion: Some(kind),
        outcome: None,
        next: Some("what moves it".to_string()),
    };
    set(&mut file, "motherbee", on(), on());
    set(&mut file, "worker1", on(), on());
    set(&mut file, "egress1", excluded(Exclusion::RolledBack), on());
    assert_eq!(
        file.settle_state(2_000),
        Some((id.clone(), UpdateState::Held))
    );
    let open = file.open.as_ref().expect("held is still the open update");
    assert_eq!(open.held_hives(), Some(vec!["egress1".to_string()]));
    let view = record_view(open);
    assert_eq!(view["state"], json!("held"));
    assert_eq!(view["held"], json!(["egress1"]));

    // Its config off the target keeps it open: so does any other exclusion.
    for (core, config) in [
        (
            excluded(Exclusion::RolledBack),
            excluded(Exclusion::RolledBack),
        ),
        (on(), excluded(Exclusion::HandEdited)),
        (excluded(Exclusion::Ahead), on()),
        (
            CategoryEntry {
                state: Standing::Pending,
                ..on()
            },
            on(),
        ),
    ] {
        set(&mut file, "egress1", core, config);
        file.settle_state(3_000);
        assert_eq!(
            file.open.as_ref().map(|open| open.state),
            Some(UpdateState::Open)
        );
    }
    // A stale target holds nothing and closes nothing.
    set(&mut file, "egress1", on(), on());
    file.open.as_mut().expect("open").stale_target = Some(StaleTarget {
        target: target(),
        since_ms: 1,
    });
    assert_eq!(file.settle_state(4_000), None);
    assert!(file.open.is_some());
    // The held hive moved by its per-hive update: done.
    file.open.as_mut().expect("open").stale_target = None;
    assert_eq!(file.settle_state(5_000), Some((id, UpdateState::Done)));
}

/// The state is on disk, written atomically: SY.admin's start resumes the open update, and a core
/// or a config found in flight stays in flight for the follow (F6: a core sent with no answer is
/// followed, never turned into not_answered at once); a file that does not parse is moved aside.
#[test]
fn the_state_survives_a_restart() {
    let state_dir = temp_state("installation-update-restart");
    let updates = Updates::load(&state_dir);
    let (id, _) = updates.change(|file| {
        let opened = file.open_or_resume(&target(), "http", Some("deploy 0.1.123"), now_epoch_ms());
        file.refresh_hives(
            &opened.0,
            &[
                "motherbee".to_string(),
                "worker1".to_string(),
                "egress1".to_string(),
            ],
            Vec::new(),
        );
        opened
    });
    assert!(updates.record(
        &id,
        "motherbee",
        Category::Core,
        Standing::OnTarget,
        None,
        None,
        None
    ));
    // worker1: its core update answered restarting; its config being applied.
    let mut restarting = Outcome::new(OutcomeStatus::Restarting, RunKind::Explicit, 10, 20);
    restarting.previous_update = Some("100".to_string());
    restarting.answered_ms = Some(5);
    assert!(updates.record(
        &id,
        "worker1",
        Category::Core,
        Standing::InFlight,
        None,
        Some(restarting),
        None
    ));
    let sent = Outcome::new(OutcomeStatus::Sent, RunKind::Explicit, 30, 30);
    assert!(updates.record(
        &id,
        "worker1",
        Category::Config,
        Standing::InFlight,
        None,
        Some(sent.clone()),
        None
    ));
    // egress1: its core update sent, no answer recorded.
    let mut sent_core = sent;
    sent_core.previous_update = Some("none".to_string());
    assert!(updates.record(
        &id,
        "egress1",
        Category::Core,
        Standing::InFlight,
        None,
        Some(sent_core),
        None
    ));
    assert!(updates_path(&state_dir).exists());

    let resumed = Updates::load(&state_dir);
    let open = resumed.read(|file| file.open.clone()).expect("resumed");
    assert_eq!(open.id, id);
    assert_eq!(open.reason.as_deref(), Some("deploy 0.1.123"));
    let worker = &open.hives["worker1"];
    assert_eq!(worker.core.state, Standing::InFlight);
    assert_eq!(
        worker
            .core
            .outcome
            .as_ref()
            .and_then(|o| o.previous_update.as_deref()),
        Some("100")
    );
    assert_eq!(worker.config.state, Standing::InFlight);
    assert_eq!(look_at(worker), Look::Follow);
    let egress = &open.hives["egress1"];
    assert_eq!(egress.core.state, Standing::InFlight);
    assert_eq!(
        egress.core.outcome.as_ref().map(|o| o.status),
        Some(OutcomeStatus::Sent)
    );
    assert!(egress.automatic.is_none());
    assert_eq!(look_at(egress), Look::Follow);
    assert_eq!(open.hives["motherbee"].core.state, Standing::OnTarget);

    // Unreadable: moved aside, and the state starts empty.
    fs::write(updates_path(&state_dir), b"{not json").expect("garbage");
    let fresh = Updates::load(&state_dir);
    assert!(fresh.read(|file| file.open.is_none() && file.closed.is_empty()));
    let aside = fs::read_dir(state_dir.join("sy-admin"))
        .expect("dir")
        .flatten()
        .any(|entry| entry.file_name().to_string_lossy().contains("unreadable"));
    assert!(aside);
    // An outcome of an update no longer open is not recorded.
    assert!(!fresh.record(
        &id,
        "worker1",
        Category::Core,
        Standing::OnTarget,
        None,
        None,
        None
    ));
}

/// The watch of a core update: waiting while it restarts or settles, settled once the orchestrator
/// it installed settled it, failed when its health gate rolled it back; an older record never
/// reads as this update's, and says the update was not seen yet.
#[test]
fn a_core_update_is_watched_to_its_settle() {
    let restarting = on_target_with(core(
        Some(("update", H_OLD)),
        H_NEW,
        json!({"phase": "restarting", "started_at_ms": "500"}),
        Value::Null,
    ));
    assert_eq!(
        watch_core(&restarting, H_NEW, Some("100")),
        CoreWatch::Waiting {
            installed: false,
            open: vec![],
            seen: true
        }
    );
    let installed = on_target_with(core(
        Some(("update", H_NEW)),
        H_NEW,
        json!({"phase": "done", "status": "ok", "started_at_ms": "500", "updated": ["sy-orchestrator"]}),
        Value::Null,
    ));
    assert_eq!(
        watch_core(&installed, H_NEW, Some("100")),
        CoreWatch::Waiting {
            installed: true,
            open: vec![],
            seen: true
        }
    );
    assert_eq!(
        watch_core(&on_target("worker1"), H_NEW, Some("100")),
        CoreWatch::Settled
    );
    let rolled_back = on_target_with(core(
        Some(("update", H_OLD)),
        H_NEW,
        json!({"phase": "done", "status": "rollback", "started_at_ms": "500", "errors": ["core health gate failed: sy-identity"]}),
        Value::Null,
    ));
    assert!(matches!(
        watch_core(&rolled_back, H_NEW, Some("100")),
        CoreWatch::Failed { code, detail } if code == "CORE_UPDATE_ROLLED_BACK" && detail.contains("sy-identity")
    ));
    // The same record as before the update: the update has not written its own yet, and is not
    // seen.
    assert_eq!(
        watch_core(&rolled_back, H_NEW, Some("500")),
        CoreWatch::Waiting {
            installed: false,
            open: vec![],
            seen: false
        }
    );
    // On the target, something left open.
    let open = on_target_with(core(
        Some(("update", H_NEW)),
        H_NEW,
        json!({"phase": "done", "status": "ok", "started_at_ms": "500",
               "updated": ["sy-orchestrator"], "settled": {"settled": false, "pending": ["sy-new"], "errors": []}}),
        Value::Null,
    ));
    assert!(matches!(
        watch_core(&open, H_NEW, None),
        CoreWatch::Waiting { installed: true, open, .. } if open.len() == 1
    ));
}

fn on_target_with(core: Value) -> Value {
    snapshot(
        "worker1",
        "worker",
        core,
        config("current", 3, SHA_V3, Some(Some((3, SHA_V3)))),
    )
}

#[test]
fn versions_compare_as_releases() {
    assert_eq!(version_newer("0.1.124", "0.1.123"), Some(true));
    assert_eq!(version_newer("0.1.123", "0.1.123"), Some(false));
    assert_eq!(version_newer("0.1.99", "0.1.123"), Some(false));
    assert_eq!(version_newer("0.2.0", "0.1.999"), Some(true));
    assert_eq!(version_newer("0.1.123,0.1.124", "0.1.123"), None);
    assert_eq!(version_newer("0.1.88-gate1", "0.1.88"), None);
}

#[test]
fn the_requests_are_taken_exactly() {
    assert_eq!(
        parse_update_request(&Value::Null),
        Ok(UpdateRequest::default())
    );
    assert_eq!(
        parse_update_request(
            &json!({"reason": " deploy 0.1.123 ", "include_rolled_back": ["egress1", "egress1"]})
        ),
        Ok(UpdateRequest {
            reason: Some("deploy 0.1.123".to_string()),
            include_rolled_back: vec!["egress1".to_string()],
        })
    );
    // The executor sends null for an optional field a plan leaves out.
    assert_eq!(
        parse_update_request(&json!({"reason": null, "include_rolled_back": null})),
        Ok(UpdateRequest::default())
    );
    assert!(parse_update_request(&json!({"target": "worker1"})).is_err());
    assert!(parse_update_request(&json!({"include_rolled_back": "egress1"})).is_err());
    assert!(parse_update_request(&json!({"reason": 7})).is_err());
    assert_eq!(parse_history_flag(&json!({})), Ok(false));
    assert_eq!(parse_history_flag(&json!({"history": true})), Ok(true));
    // SCMD's query strings come as strings.
    assert_eq!(parse_history_flag(&json!({"history": "true"})), Ok(true));
    assert!(parse_history_flag(&json!({"history": "maybe"})).is_err());
    assert!(parse_history_flag(&json!({"hive": "worker1"})).is_err());
}

// ---------------------------------------------------------------------------------------------
// A fake mesh
// ---------------------------------------------------------------------------------------------

/// What a hive does with an update.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Behaves {
    /// Applies it: a core update answers restarting, and its next read shows it settled; a config
    /// update applies the version.
    Well,
    /// Answers every update with an error.
    Refuses,
    /// Gives no answer to a core update (TIMEOUT) and never applies it.
    TimesOut,
    /// Gives no answer to a core update (TIMEOUT), and applies it all the same.
    TimesOutButApplies,
}

/// Hives that answer `/versions` from a table the test changes, apply updates as told, and keep
/// what they were sent.
struct FakeMesh {
    hives: Vec<String>,
    snapshots: Mutex<HashMap<String, Value>>,
    down: Mutex<HashSet<String>>,
    behaves: Mutex<HashMap<String, Behaves>>,
    sent: Mutex<Vec<(String, Value)>>,
    reads: AtomicU64,
    /// Reads of one hive outstanding at once, and the most seen.
    reading_now: Mutex<HashMap<String, usize>>,
    most_at_once: AtomicUsize,
    /// How long a read takes.
    read_delay: Mutex<Duration>,
    registry_fails: AtomicBool,
    /// The next reads of a hive its restart loses (FINDINGS A-250): each waits out the bound it
    /// was given and fails as SY.admin's dispatcher does when no answer comes.
    lost_reads: Mutex<HashMap<String, usize>>,
    /// The bound each read was given, by hive, in order.
    read_bounds: Mutex<Vec<(String, Duration)>>,
}

impl FakeMesh {
    fn new(snapshots: Vec<Value>) -> Arc<FakeMesh> {
        let hives = snapshots
            .iter()
            .map(|snapshot| snapshot["hive_id"].as_str().expect("hive_id").to_string())
            .collect();
        Arc::new(FakeMesh {
            hives,
            snapshots: Mutex::new(
                snapshots
                    .into_iter()
                    .map(|snapshot| (snapshot["hive_id"].as_str().unwrap().to_string(), snapshot))
                    .collect(),
            ),
            down: Mutex::new(HashSet::new()),
            behaves: Mutex::new(HashMap::new()),
            sent: Mutex::new(Vec::new()),
            reads: AtomicU64::new(0),
            reading_now: Mutex::new(HashMap::new()),
            most_at_once: AtomicUsize::new(0),
            read_delay: Mutex::new(Duration::ZERO),
            registry_fails: AtomicBool::new(false),
            lost_reads: Mutex::new(HashMap::new()),
            read_bounds: Mutex::new(Vec::new()),
        })
    }

    /// The next `count` reads of `hive` are lost.
    fn lose_reads(&self, hive: &str, count: usize) {
        lock(&self.lost_reads).insert(hive.to_string(), count);
    }

    fn lost_reads_left(&self, hive: &str) -> usize {
        lock(&self.lost_reads).get(hive).copied().unwrap_or(0)
    }

    /// The bounds the reads of `hive` were given, in order.
    fn read_bounds_of(&self, hive: &str) -> Vec<Duration> {
        lock(&self.read_bounds)
            .iter()
            .filter(|(of, _)| of == hive)
            .map(|(_, bound)| *bound)
            .collect()
    }

    fn set_down(&self, hive: &str, down: bool) {
        if down {
            lock(&self.down).insert(hive.to_string());
        } else {
            lock(&self.down).remove(hive);
        }
    }

    fn behave(&self, hive: &str, behaves: Behaves) {
        lock(&self.behaves).insert(hive.to_string(), behaves);
    }

    fn sent_to(&self, hive: &str) -> Vec<Value> {
        lock(&self.sent)
            .iter()
            .filter(|(to, _)| to == hive)
            .map(|(_, payload)| payload.clone())
            .collect()
    }

    fn snapshot(&self, hive: &str) -> Value {
        lock(&self.snapshots)[hive].clone()
    }

    fn set_snapshot(&self, hive: &str, snapshot: Value) {
        lock(&self.snapshots).insert(hive.to_string(), snapshot);
    }
}

#[async_trait]
impl UpdateMesh for FakeMesh {
    async fn versions(&self, hive: &str, timeout: Duration) -> Result<Value, String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        lock(&self.read_bounds).push((hive.to_string(), timeout));
        let at_once = {
            let mut reading = lock(&self.reading_now);
            let count = reading.entry(hive.to_string()).or_insert(0);
            *count += 1;
            *count
        };
        self.most_at_once.fetch_max(at_once, Ordering::SeqCst);
        let lost = match lock(&self.lost_reads).get_mut(hive) {
            Some(left) if *left > 0 => {
                *left -= 1;
                true
            }
            _ => false,
        };
        if lost {
            tokio::time::sleep(timeout).await;
            *lock(&self.reading_now).get_mut(hive).expect("reading") -= 1;
            return Err(format!(
                "timeout waiting response trace_id=t target=SY.orchestrator@motherbee \
                 request_msg=get_versions response_msg=admin timeout_ms={}",
                timeout.as_millis()
            ));
        }
        let delay = *lock(&self.read_delay);
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        let answer = (|| {
            if lock(&self.down).contains(hive) {
                return Err("target hive not reachable in LSA (stale/missing)".to_string());
            }
            let mut snapshots = lock(&self.snapshots);
            let snapshot = snapshots.get_mut(hive).ok_or("no such hive")?;
            let now = snapshot.clone();
            // A core restarting settles by the next read.
            if text_of(
                snapshot
                    .get("core")
                    .and_then(|core| core.get("last_update")),
                "phase",
            ) == Some("restarting")
            {
                snapshot["core"]["installed"] =
                    json!({"manifest_hash": H_NEW, "source": "update", "installed_at_ms": "2"});
                snapshot["core"]["last_update"] = settled_update();
                snapshot["core"]["last_update"]["started_at_ms"] = json!("900");
            }
            Ok(now)
        })();
        *lock(&self.reading_now).get_mut(hive).expect("reading") -= 1;
        answer
    }

    async fn update(
        &self,
        hive: &str,
        payload: Value,
        _timeout: Duration,
    ) -> Result<Value, String> {
        lock(&self.sent).push((hive.to_string(), payload.clone()));
        if lock(&self.down).contains(hive) {
            return Err(format!(
                "router returned UNREACHABLE for target=SY.orchestrator@{hive} reason=NO_ROUTE"
            ));
        }
        let behaves = lock(&self.behaves)
            .get(hive)
            .copied()
            .unwrap_or(Behaves::Well);
        let core = payload["category"].as_str() == Some("core");
        match behaves {
            Behaves::Refuses => {
                return Ok(
                    json!({"status": "error", "error_code": "RESTART_FAILED", "message": "sy-cognition did not come back"}),
                )
            }
            Behaves::TimesOut if core => {
                return Err("timeout waiting SYSTEM_UPDATE_RESPONSE".to_string())
            }
            _ => {}
        }
        let mut snapshots = lock(&self.snapshots);
        let snapshot = snapshots.get_mut(hive).ok_or("no such hive")?;
        match payload["category"].as_str() {
            Some("core") => {
                snapshot["core"]["last_update"] = json!({"phase": "restarting", "started_at_ms": "900", "updated": ["sy-orchestrator"]});
                if behaves == Behaves::TimesOutButApplies {
                    return Err("timeout waiting SYSTEM_UPDATE_RESPONSE".to_string());
                }
                Ok(json!({"status": "ok", "phase": "restarting", "updated": ["sy-orchestrator"]}))
            }
            Some("config") => {
                snapshot["config"]["applied"] = json!({
                    "version": payload["manifest_version"],
                    "sha256": payload["manifest_hash"],
                    "applied_at_ms": 5,
                    "restarted": ["sy-cognition"],
                });
                Ok(json!({"status": "ok", "restarted": ["sy-cognition"], "unchanged": []}))
            }
            other => Err(format!("an unexpected category {other:?}")),
        }
    }

    fn orchestrator_connected(&self, hive: &str) -> Option<bool> {
        Some(!lock(&self.down).contains(hive))
    }

    fn hives(&self) -> Result<(Vec<String>, Vec<Value>), String> {
        if self.registry_fails.load(Ordering::SeqCst) {
            return Err(
                "the hive registry (/var/lib/fluxbee/hives) cannot be read: EIO".to_string(),
            );
        }
        Ok((self.hives.clone(), Vec::new()))
    }

    fn motherbee(&self) -> &str {
        "motherbee"
    }
}

/// Bounds of milliseconds: a run ends at once where PROD waits.
fn quick() -> Bounds {
    Bounds {
        answer: Duration::from_millis(30),
        answer_poll: Duration::from_millis(10),
        apply: Duration::from_millis(40),
        first_wait: Duration::from_millis(5),
        max_wait: Duration::from_millis(10),
        update_call: Duration::from_millis(500),
        settle: Duration::from_secs(30),
        flight_check: Duration::from_millis(10),
        retry_check: Duration::from_millis(10),
        slow_check: Duration::from_secs(600),
        read: Duration::from_millis(500),
        restart_read: Duration::from_millis(500),
    }
}

fn motherbee_on_target() -> Value {
    snapshot(
        "motherbee",
        "motherbee",
        core(None, H_NEW, Value::Null, Value::Null),
        config("current", 3, SHA_V3, Some(Some((3, SHA_V3)))),
    )
}

fn worker_behind() -> Value {
    snapshot(
        "worker1",
        "worker",
        core(
            Some(("update", H_OLD)),
            H_NEW,
            settled_update(),
            Value::Null,
        ),
        config("current", 3, SHA_V3, Some(Some((2, SHA_V2)))),
    )
}

fn egress_behind_config() -> Value {
    snapshot(
        "egress1",
        "egress",
        core(
            Some(("update", H_NEW)),
            H_NEW,
            settled_update(),
            Value::Null,
        ),
        config("current", 3, SHA_V3, Some(Some((2, SHA_V2)))),
    )
}

fn egress_rolled_back(config_applied: (u64, &str)) -> Value {
    snapshot(
        "egress1",
        "egress",
        core(
            Some(("core_rollback", H_OLD)),
            H_NEW,
            settled_update(),
            Value::Null,
        ),
        config("current", 3, SHA_V3, Some(Some(config_applied))),
    )
}

async fn explicit_run(updates: &Arc<Updates>, mesh: &Arc<FakeMesh>, include: &[String]) -> RunEnd {
    let dyn_mesh: Arc<dyn UpdateMesh> = mesh.clone();
    let motherbee = mesh.snapshot("motherbee");
    run_explicit(
        updates,
        &dyn_mesh,
        "http",
        Some("deploy 0.1.123"),
        include,
        &motherbee,
        &target_of(&motherbee),
        &quick(),
        Duration::from_secs(10),
    )
    .await
}

fn entry_of(updates: &Updates, hive: &str) -> HiveEntry {
    updates.read(|file| file.open.as_ref().expect("open").hives[hive].clone())
}

/// An explicit run: the core first, which answers restarting and is followed to its settle; then
/// the config of the same run; then the update is done. A second run with every hive on the
/// target has nothing to do and records nothing.
#[tokio::test]
async fn a_run_updates_the_core_then_the_config_and_closes_done() {
    let updates = Updates::load(&temp_state("installation-update-run"));
    let mesh = FakeMesh::new(vec![motherbee_on_target(), worker_behind()]);

    let end = explicit_run(&updates, &mesh, &[]).await;
    let RunEnd::Update { update, opening } = end else {
        panic!("an update opened");
    };
    assert_eq!(opening, Opening::Opened);
    assert_eq!(update.state, UpdateState::Open);
    let worker = &update.hives["worker1"];
    assert_eq!(worker.role.as_deref(), Some("worker"));
    assert_eq!(worker.core.state, Standing::InFlight);
    let outcome = worker.core.outcome.as_ref().expect("outcome");
    assert_eq!(outcome.status, OutcomeStatus::Restarting);
    assert!(outcome.answered_ms.is_some());
    assert_eq!(worker.config.state, Standing::Pending);
    assert_eq!(update.hives["motherbee"].core.state, Standing::OnTarget);
    assert_eq!(
        mesh.sent_to("worker1"),
        vec![json!({"category": "core", "manifest_version": 0, "manifest_hash": H_NEW})]
    );
    assert!(pending_of(&update)[0].starts_with("worker1 (core in_flight"));

    // The background task follows it: its first read sees it restarting, the next one settled; then
    // its config goes, by the run that sent the core.
    let dyn_mesh: Arc<dyn UpdateMesh> = mesh.clone();
    follow(
        &updates,
        dyn_mesh.as_ref(),
        &update.id,
        "worker1",
        &target(),
        &quick(),
    )
    .await;
    assert_eq!(entry_of(&updates, "worker1").core.state, Standing::InFlight);
    follow(
        &updates,
        dyn_mesh.as_ref(),
        &update.id,
        "worker1",
        &target(),
        &quick(),
    )
    .await;
    let worker = entry_of(&updates, "worker1");
    assert_eq!(worker.core.state, Standing::OnTarget);
    let core_outcome = worker.core.outcome.as_ref().expect("core");
    assert_eq!(core_outcome.status, OutcomeStatus::Settled);
    assert!(core_outcome.installed_ms.is_some() && core_outcome.settled_ms.is_some());
    assert_eq!(worker.config.state, Standing::OnTarget);
    let config_outcome = worker.config.outcome.as_ref().expect("config");
    assert_eq!(config_outcome.by, RunKind::Explicit);
    assert_eq!(
        config_outcome.restarted.clone(),
        Some(vec!["sy-cognition".to_string()])
    );
    assert_eq!(updates.settle_state(), Some(UpdateState::Done));
    assert_eq!(updates.state_of(&update.id), Some(UpdateState::Done));

    // Again: every hive on the target, no update open: nothing to do, nothing recorded.
    let sent_before = lock(&mesh.sent).len();
    let end = explicit_run(&updates, &mesh, &[]).await;
    assert!(matches!(end, RunEnd::NothingToDo { .. }));
    assert_eq!(lock(&mesh.sent).len(), sent_before);
    assert_eq!(updates.read(|file| file.closed.len()), 1);
}

/// The gate "egress1 down across a config change, then back": the hive that did not answer stays
/// pending, the background task leaves it while its orchestrator is not connected, and the
/// automatic step finishes it once it is, once, recording the attempt and its outcome; the update
/// then closes done.
#[tokio::test]
async fn a_hive_that_comes_back_gets_the_update_finished_once() {
    let updates = Updates::load(&temp_state("installation-update-automatic"));
    let mesh = FakeMesh::new(vec![motherbee_on_target(), egress_behind_config()]);
    mesh.set_down("egress1", true);

    let RunEnd::Update { update, .. } = explicit_run(&updates, &mesh, &[]).await else {
        panic!("an update opened");
    };
    let egress = &update.hives["egress1"];
    for category in [&egress.core, &egress.config] {
        assert_eq!(category.state, Standing::Pending);
        assert_eq!(
            category.outcome.as_ref().map(|o| o.status),
            Some(OutcomeStatus::NotAnswered)
        );
        // F12: the automatic step is promised only for a hive it does not leave alone.
        assert!(category
            .next
            .as_deref()
            .is_some_and(|next| next.contains("automatic step") && next.contains("unless")));
    }
    assert_eq!(look_at(egress), Look::Automatic);

    // Still down: the cheap read of shared memory says so, and nothing is read.
    let dyn_mesh: Arc<dyn UpdateMesh> = mesh.clone();
    let reads = mesh.reads.load(Ordering::SeqCst);
    automatic_step(
        &updates,
        dyn_mesh.as_ref(),
        &update.id,
        "egress1",
        &target(),
        &quick(),
    )
    .await;
    assert_eq!(mesh.reads.load(Ordering::SeqCst), reads);
    assert!(entry_of(&updates, "egress1").automatic.is_none());

    // Back: finished, by the automatic step, and recorded.
    mesh.set_down("egress1", false);
    automatic_step(
        &updates,
        dyn_mesh.as_ref(),
        &update.id,
        "egress1",
        &target(),
        &quick(),
    )
    .await;
    let egress = entry_of(&updates, "egress1");
    let attempt = egress.automatic.as_ref().expect("the attempt");
    assert!(attempt.finished_at_ms.is_some());
    assert_eq!(
        attempt.result.as_deref(),
        Some("core on_target, config on_target")
    );
    let config = egress.config.outcome.as_ref().expect("config");
    assert_eq!(
        (config.status, config.by),
        (OutcomeStatus::Ok, RunKind::Automatic)
    );
    assert_eq!(
        mesh.sent_to("egress1").last(),
        Some(&json!({"category": "config", "manifest_version": 3, "manifest_hash": SHA_V3}))
    );
    assert_eq!(updates.settle_state(), Some(UpdateState::Done));
}

/// Once per hive and target: an automatic attempt that fails is left for the operator, and never
/// tried again; the automatic step never acts on an excluded hive; and an explicit run that names
/// a rolled-back hive moves its core.
#[tokio::test]
async fn the_automatic_step_is_once_and_never_for_an_excluded_hive() {
    let updates = Updates::load(&temp_state("installation-update-once"));
    let rolled_back = egress_rolled_back((2, SHA_V2));
    let mut worker = rolled_back.clone();
    worker["hive_id"] = json!("worker1");
    worker["role"] = json!("worker");
    let mesh = FakeMesh::new(vec![motherbee_on_target(), egress_behind_config(), worker]);
    mesh.set_down("egress1", true);
    mesh.set_down("worker1", true);
    let RunEnd::Update { update, .. } = explicit_run(&updates, &mesh, &[]).await else {
        panic!("an update opened");
    };
    let id = update.id.clone();
    let dyn_mesh: Arc<dyn UpdateMesh> = mesh.clone();

    // egress1 comes back and refuses its config: the attempt is recorded, its error left for the
    // operator with its next.
    mesh.set_down("egress1", false);
    mesh.behave("egress1", Behaves::Refuses);
    automatic_step(
        &updates,
        dyn_mesh.as_ref(),
        &id,
        "egress1",
        &target(),
        &quick(),
    )
    .await;
    let egress = entry_of(&updates, "egress1");
    let config = egress.config.outcome.as_ref().expect("config");
    assert_eq!(
        (config.status, config.by),
        (OutcomeStatus::Error, RunKind::Automatic)
    );
    assert_eq!(config.error_code.as_deref(), Some("RESTART_FAILED"));
    assert!(egress
        .config
        .next
        .as_deref()
        .is_some_and(|next| next.contains("did not restart")));
    assert!(egress.automatic.is_some());
    // F2 (b): an error is read again only now and then.
    assert_eq!(look_at(&egress), Look::Slow);
    let sent = mesh.sent_to("egress1").len();
    mesh.behave("egress1", Behaves::Well);
    automatic_step(
        &updates,
        dyn_mesh.as_ref(),
        &id,
        "egress1",
        &target(),
        &quick(),
    )
    .await;
    assert_eq!(
        mesh.sent_to("egress1").len(),
        sent,
        "never a second automatic attempt"
    );

    // worker1, rolled back by the operator, comes back: the automatic step leaves it alone, and
    // says so.
    let before = mesh.sent_to("worker1").len();
    mesh.set_down("worker1", false);
    automatic_step(
        &updates,
        dyn_mesh.as_ref(),
        &id,
        "worker1",
        &target(),
        &quick(),
    )
    .await;
    assert_eq!(mesh.sent_to("worker1").len(), before);
    let worker = entry_of(&updates, "worker1");
    for category in [&worker.core, &worker.config] {
        assert_eq!(category.state, Standing::Excluded);
        assert_eq!(
            category.outcome.as_ref().map(|o| (o.status, o.by)),
            Some((OutcomeStatus::Excluded, RunKind::Automatic))
        );
        assert!(category
            .reason
            .as_deref()
            .is_some_and(|why| why.contains("automatic step leaves it alone")));
    }
    assert_eq!(
        worker.automatic.and_then(|attempt| attempt.result),
        Some("core excluded, config excluded".to_string())
    );
    assert_eq!(updates.state_of(&id), Some(UpdateState::Open));

    // An explicit run that names it moves its core.
    let include = vec!["worker1".to_string()];
    explicit_run(&updates, &mesh, &include).await;
    assert_eq!(
        mesh.sent_to("worker1").first(),
        Some(&json!({"category": "core", "manifest_version": 0, "manifest_hash": H_NEW}))
    );
}

/// The background task's tick: it refreshes the hives, looks at what each needs, and the update
/// closes done; a superseded update's late outcome is not recorded.
#[tokio::test]
async fn the_background_task_finishes_an_open_update() {
    let updates = Updates::load(&temp_state("installation-update-tick"));
    let mesh = FakeMesh::new(vec![
        motherbee_on_target(),
        worker_behind(),
        egress_behind_config(),
    ]);
    mesh.set_down("egress1", true);
    let RunEnd::Update { update, .. } = explicit_run(&updates, &mesh, &[]).await else {
        panic!("an update opened");
    };
    mesh.set_down("egress1", false);
    let dyn_mesh: Arc<dyn UpdateMesh> = mesh.clone();
    let bounds = quick();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while updates.state_of(&update.id) == Some(UpdateState::Open) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{:#?}",
            updates.read(|f| f.clone())
        );
        tick(&updates, &dyn_mesh, &bounds).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(updates.state_of(&update.id), Some(UpdateState::Done));
    let done = updates.read(|file| file.closed[0].clone());
    assert!(done.hives["egress1"].automatic.is_some());
    assert!(done.hives["worker1"].automatic.is_none());
    assert_eq!(done.hives["worker1"].config.state, Standing::OnTarget);

    // Late: an outcome for an update that is no longer open.
    assert!(!updates.record(
        &update.id,
        "worker1",
        Category::Core,
        Standing::Pending,
        None,
        None,
        None
    ));
}

/// F1: a hive registry the task cannot read never drops the spokes: the background task neither
/// refreshes nor closes the update, and an explicit run refuses.
#[tokio::test]
async fn a_registry_that_cannot_be_read_closes_nothing() {
    let updates = Updates::load(&temp_state("installation-update-registry"));
    let mesh = FakeMesh::new(vec![motherbee_on_target(), worker_behind()]);
    mesh.set_down("worker1", true);
    let RunEnd::Update { update, .. } = explicit_run(&updates, &mesh, &[]).await else {
        panic!("an update opened");
    };
    mesh.registry_fails.store(true, Ordering::SeqCst);
    let dyn_mesh: Arc<dyn UpdateMesh> = mesh.clone();
    for _ in 0..3 {
        tick(&updates, &dyn_mesh, &quick()).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let open = updates
        .read(|file| file.open.clone())
        .expect("still open, not done");
    assert_eq!(open.id, update.id);
    assert!(open.hives.contains_key("worker1"));
    assert_eq!(
        explicit_run(&updates, &mesh, &[]).await,
        RunEnd::RegistryUnreadable(
            "the hive registry (/var/lib/fluxbee/hives) cannot be read: EIO".to_string()
        )
    );
    let (status, body) = run_answer(
        "update_installation",
        RunEnd::RegistryUnreadable("EIO".to_string()),
        &UpdateRequest::default(),
        Duration::ZERO,
    );
    assert_eq!(status, 503);
    assert!(body.contains("HIVE_REGISTRY_UNREADABLE"));
}

/// F2 (a): at most one read of a hive's /versions outstanding, whoever reads it; a read that timed
/// out keeps the hive quiet for another bound, as the motherbee's orchestrator may still hold it.
#[tokio::test]
async fn a_hive_is_read_once_at_a_time() {
    let updates = Updates::load(&temp_state("installation-update-reads"));
    let mesh = FakeMesh::new(vec![motherbee_on_target(), worker_behind()]);
    *lock(&mesh.read_delay) = Duration::from_millis(30);
    let dyn_mesh: Arc<dyn UpdateMesh> = mesh.clone();
    let bounds = quick();
    let reads = (0..4).map(|_| updates.read_versions(dyn_mesh.as_ref(), "worker1", &bounds));
    let answers = futures::future::join_all(reads).await;
    assert!(answers.iter().all(Result::is_ok));
    assert_eq!(mesh.most_at_once.load(Ordering::SeqCst), 1);

    // A read SY.admin gave up (TIMEOUT): the hive is not read again for another bound.
    struct Silent;
    #[async_trait]
    impl UpdateMesh for Silent {
        async fn versions(&self, _hive: &str, _timeout: Duration) -> Result<Value, String> {
            Err("timeout waiting admin answer".to_string())
        }
        async fn update(&self, _: &str, _: Value, _: Duration) -> Result<Value, String> {
            unreachable!()
        }
        fn orchestrator_connected(&self, _: &str) -> Option<bool> {
            Some(true)
        }
        fn hives(&self) -> Result<(Vec<String>, Vec<Value>), String> {
            Ok((vec!["motherbee".to_string()], Vec::new()))
        }
        fn motherbee(&self) -> &str {
            "motherbee"
        }
    }
    let first = updates.read_versions(&Silent, "egress1", &bounds).await;
    assert!(first.is_err());
    let reads_before = mesh.reads.load(Ordering::SeqCst);
    let quiet = updates
        .read_versions(dyn_mesh.as_ref(), "egress1", &bounds)
        .await;
    assert!(quiet.is_err_and(|why| why.contains("still be queued")));
    assert_eq!(mesh.reads.load(Ordering::SeqCst), reads_before);
}

/// An explicit run with `bounds` whose core update for worker1 answers `restarting`: its record
/// has worker1 in its restart. The update's id.
async fn run_into_restart(updates: &Arc<Updates>, mesh: &Arc<FakeMesh>, bounds: &Bounds) -> String {
    let dyn_mesh: Arc<dyn UpdateMesh> = mesh.clone();
    let motherbee = mesh.snapshot("motherbee");
    let RunEnd::Update { update, .. } = run_explicit(
        updates,
        &dyn_mesh,
        "http",
        Some("deploy 0.1.131"),
        &[],
        &motherbee,
        &target_of(&motherbee),
        bounds,
        Duration::from_secs(10),
    )
    .await
    else {
        panic!("an update opened");
    };
    let worker = &update.hives["worker1"];
    assert!(worker.in_restart(), "{worker:#?}");
    update.id
}

/// worker1 restarted onto the target and settled by the orchestrator it installed, its config
/// still the version before.
fn worker_settled_config_behind() -> Value {
    let mut settled = worker_behind();
    settled["core"]["installed"] =
        json!({"manifest_hash": H_NEW, "source": "update", "installed_at_ms": "2"});
    settled["core"]["last_update"] = settled_update();
    settled["core"]["last_update"]["started_at_ms"] = json!("900");
    settled
}

/// Waits until the update `id` is done, at most `within`.
async fn wait_done(updates: &Updates, id: &str, within: Duration) {
    let deadline = tokio::time::Instant::now() + within;
    while updates.state_of(id) != Some(UpdateState::Done) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{:#?}",
            updates.read(|file| file.clone())
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// FINDINGS A-250, as 8.x's deploys saw it: worker1's core update answers `restarting`, and its
/// orchestrator restarts itself for the new binary; the follow's read reaches it then and is never
/// answered, while worker1 settles. That read waits the restart's short bound, leaves no hold, and
/// is asked again at once: the second read finds worker1 settled, its config goes, and the update
/// closes. The long bound, a hold or the follow's cadence (30 s each here) would have kept it open
/// far past the 5 s the test gives it.
#[tokio::test]
async fn a_read_lost_in_a_restart_is_asked_again_at_once() {
    let bounds = Bounds {
        read: Duration::from_secs(30),
        restart_read: Duration::from_millis(200),
        flight_check: Duration::from_secs(30),
        settle: Duration::from_secs(60),
        ..quick()
    };
    let updates = Updates::load(&temp_state("installation-update-restart-lost-read"));
    let mesh = FakeMesh::new(vec![motherbee_on_target(), worker_behind()]);
    let id = run_into_restart(&updates, &mesh, &bounds).await;
    // worker1 settles while the follow's first read is lost.
    mesh.set_snapshot("worker1", worker_settled_config_behind());
    mesh.lose_reads("worker1", 1);

    let dyn_mesh: Arc<dyn UpdateMesh> = mesh.clone();
    let background = tokio::spawn(run_background(Arc::clone(&updates), dyn_mesh, bounds));
    // The follow's first look: 3 s after the core's answer on 8.x, its cadence here 30 s.
    updates.look_now("worker1");
    wait_done(&updates, &id, Duration::from_secs(5)).await;
    background.abort();

    // The run's read before the restart, the lost one, and the one answered.
    assert_eq!(
        mesh.read_bounds_of("worker1"),
        vec![bounds.read, bounds.restart_read, bounds.restart_read]
    );
    assert_eq!(mesh.lost_reads_left("worker1"), 0);
    let done = updates.read(|file| file.closed[0].clone());
    let worker = &done.hives["worker1"];
    let core = worker.core.outcome.as_ref().expect("core");
    assert_eq!(
        (core.status, core.by),
        (OutcomeStatus::Settled, RunKind::Explicit)
    );
    assert!(core.settled_ms.is_some());
    assert_eq!(worker.config.state, Standing::OnTarget);
    assert_eq!(mesh.most_at_once.load(Ordering::SeqCst), 1);
}

/// FINDINGS A-250 changes nothing outside a restart: a core update that did not answer in time is
/// followed, but not as a restart (the hive may be slow). Its read lost waits the long bound, keeps
/// the hive unread for another (the next look reads nothing), and is not asked again at once. Once
/// its core update restarts it, a read waits the short bound, passes a hold it finds, and one lost
/// is asked again at once, the hive let go meanwhile; the read that is answered ends the hold.
#[tokio::test]
async fn a_read_outside_a_restart_keeps_the_long_bound_and_the_hold() {
    let bounds = Bounds {
        read: Duration::from_secs(1),
        restart_read: Duration::from_millis(50),
        ..quick()
    };
    let updates = Updates::load(&temp_state("installation-update-slow-hive"));
    let mesh = FakeMesh::new(vec![motherbee_on_target(), worker_behind()]);
    mesh.behave("worker1", Behaves::TimesOutButApplies);
    let dyn_mesh: Arc<dyn UpdateMesh> = mesh.clone();
    let motherbee = mesh.snapshot("motherbee");
    let RunEnd::Update { update, .. } = run_explicit(
        &updates,
        &dyn_mesh,
        "http",
        None,
        &[],
        &motherbee,
        &target(),
        &bounds,
        Duration::from_secs(10),
    )
    .await
    else {
        panic!("an update opened");
    };
    let id = update.id;
    let worker = entry_of(&updates, "worker1");
    assert_eq!(
        worker.core.outcome.as_ref().map(|o| o.status),
        Some(OutcomeStatus::NotAnswered)
    );
    assert!(worker.in_flight() && !worker.in_restart());

    // Its read is lost: the long bound, then the hold; not asked again at once.
    mesh.lose_reads("worker1", 1);
    follow(
        &updates,
        dyn_mesh.as_ref(),
        &id,
        "worker1",
        &target(),
        &bounds,
    )
    .await;
    assert_eq!(mesh.read_bounds_of("worker1").last(), Some(&bounds.read));
    assert_ne!(updates.due_at("worker1"), 0, "not looked at at once");
    let reads = mesh.reads.load(Ordering::SeqCst);
    follow(
        &updates,
        dyn_mesh.as_ref(),
        &id,
        "worker1",
        &target(),
        &bounds,
    )
    .await;
    assert_eq!(mesh.reads.load(Ordering::SeqCst), reads, "held: not read");

    // Its core update restarts it: the short bound, past the hold; lost, asked again at once, and
    // the hive let go.
    let now = now_epoch_ms();
    updates.record(
        &id,
        "worker1",
        Category::Core,
        Standing::InFlight,
        Some("its services restart onto the new release".to_string()),
        Some(Outcome::new(
            OutcomeStatus::Restarting,
            RunKind::Explicit,
            now,
            now,
        )),
        None,
    );
    assert!(lock(&updates.quiet_until)
        .get("worker1")
        .is_some_and(|until| *until > now_epoch_ms()));
    mesh.lose_reads("worker1", 1);
    follow(
        &updates,
        dyn_mesh.as_ref(),
        &id,
        "worker1",
        &target(),
        &bounds,
    )
    .await;
    assert_eq!(mesh.reads.load(Ordering::SeqCst), reads + 1);
    assert_eq!(
        mesh.read_bounds_of("worker1").last(),
        Some(&bounds.restart_read)
    );
    assert_eq!(updates.due_at("worker1"), 0, "looked at again at once");
    assert!(updates.holder("worker1").is_none(), "one read a look");

    // Answered: the hold ends, and the follow takes worker1 to its settle and its config.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !entry_of(&updates, "worker1").on_target() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{:#?}",
            entry_of(&updates, "worker1")
        );
        follow(
            &updates,
            dyn_mesh.as_ref(),
            &id,
            "worker1",
            &target(),
            &bounds,
        )
        .await;
    }
    assert!(!lock(&updates.quiet_until).contains_key("worker1"));
}

/// FINDINGS A-250 keeps one read of a hive at a time, everywhere (A-204, A-205): while the follow
/// asks a restarting hive again at once after each read its restart lost, an explicit run reads
/// the same hive over and over. No read of it ever runs beside another, and the update closes once
/// the hive answers.
#[tokio::test]
async fn a_read_asked_again_never_runs_beside_another_of_its_hive() {
    // The follow's cadence stays quick's: the first read answered, the follow's or the run's,
    // shows the core still restarting.
    let bounds = Bounds {
        read: Duration::from_secs(30),
        restart_read: Duration::from_millis(100),
        settle: Duration::from_secs(60),
        ..quick()
    };
    let updates = Updates::load(&temp_state("installation-update-restart-one-read"));
    let mesh = FakeMesh::new(vec![motherbee_on_target(), worker_behind()]);
    let id = run_into_restart(&updates, &mesh, &bounds).await;
    // Its next four reads are lost, whoever reads; an answer takes 20 ms. The update closes only
    // after them: the first answer shows the core restarting, the next one settled.
    mesh.lose_reads("worker1", 4);
    *lock(&mesh.read_delay) = Duration::from_millis(20);

    let dyn_mesh: Arc<dyn UpdateMesh> = mesh.clone();
    let background = tokio::spawn(run_background(
        Arc::clone(&updates),
        Arc::clone(&dyn_mesh),
        bounds,
    ));
    updates.look_now("worker1");
    // An explicit run's reads of worker1 meanwhile: its first read of every hive, its pass's.
    let run_reads = async {
        for _ in 0..6 {
            let _ = updates
                .read_versions(dyn_mesh.as_ref(), "worker1", &bounds)
                .await;
        }
    };
    tokio::join!(run_reads, wait_done(&updates, &id, Duration::from_secs(10)));
    background.abort();

    assert_eq!(mesh.most_at_once.load(Ordering::SeqCst), 1);
    assert_eq!(mesh.lost_reads_left("worker1"), 0);
    assert!(mesh
        .read_bounds_of("worker1")
        .iter()
        .skip(1)
        .take(4)
        .all(|bound| *bound == bounds.restart_read));
}

/// A spoke of `role` behind the target as worker1 is in `worker_behind`, or restarted onto it and
/// settled with its config still behind as in `worker_settled_config_behind`.
fn spoke_as(hive: &str, role: &str, worker1: Value) -> Value {
    let mut spoke = worker1;
    spoke["hive_id"] = json!(hive);
    spoke["role"] = json!(role);
    spoke
}

/// FINDINGS A-263, as 0.1.131's deploy saw it: every spoke's core update restarts its
/// orchestrator, and the follow reads each spoke until it settles. egress1's read reaches its
/// orchestrator in its few seconds off the router and is never answered, while ingress1 and
/// worker1 settle. The background task reads every hive at once, each read bounded as before, so
/// ingress1 and worker1 are read, marked settled and given their config within the bound of
/// egress1's lost read, beside it. Until 0.1.137 it read one hive at a time, and they waited that
/// whole bound behind it. As before, egress1's lost read is asked again at once (FINDINGS A-250), a
/// hive is read once at a time, the update closes, and its results come in the hives' order.
#[tokio::test]
async fn the_follow_reads_every_hive_at_once() {
    let bounds = Bounds {
        read: Duration::from_secs(30),
        restart_read: Duration::from_secs(2),
        flight_check: Duration::from_secs(30),
        settle: Duration::from_secs(60),
        ..quick()
    };
    let spokes = [
        ("egress1", "egress"),
        ("ingress1", "ingress"),
        ("worker1", "worker"),
    ];
    let updates = Updates::load(&temp_state("installation-update-reads-at-once"));
    let mesh = FakeMesh::new(
        std::iter::once(motherbee_on_target())
            .chain(
                spokes
                    .iter()
                    .map(|(hive, role)| spoke_as(hive, role, worker_behind())),
            )
            .collect(),
    );
    let dyn_mesh: Arc<dyn UpdateMesh> = mesh.clone();
    let motherbee = mesh.snapshot("motherbee");
    let RunEnd::Update { update, .. } = run_explicit(
        &updates,
        &dyn_mesh,
        "http",
        Some("deploy 0.1.137"),
        &[],
        &motherbee,
        &target_of(&motherbee),
        &bounds,
        Duration::from_secs(10),
    )
    .await
    else {
        panic!("an update opened");
    };
    for (hive, role) in spokes {
        let entry = &update.hives[hive];
        assert!(entry.in_restart(), "{hive}: {entry:#?}");
        // Each spoke settles by its next read; egress1's is lost in its restart.
        mesh.set_snapshot(hive, spoke_as(hive, role, worker_settled_config_behind()));
    }
    mesh.lose_reads("egress1", 1);

    let background = tokio::spawn(run_background(
        Arc::clone(&updates),
        Arc::clone(&dyn_mesh),
        bounds,
    ));
    let started = tokio::time::Instant::now();
    for (hive, _) in spokes {
        updates.look_now(hive);
    }
    // The update's record of `hive`, open or closed by then.
    let on_target = |hive: &str| {
        updates.read(|file| {
            file.open
                .iter()
                .chain(file.closed.iter())
                .find(|record| record.id == update.id)
                .is_some_and(|record| record.hives[hive].on_target())
        })
    };
    // ingress1 and worker1 within the bound of egress1's lost read, beside it.
    while !(on_target("ingress1") && on_target("worker1")) {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:#?}",
            updates.read(|file| file.clone())
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < bounds.restart_read,
        "ingress1 and worker1 waited {elapsed:?}, behind egress1's lost read"
    );
    assert!(
        !on_target("egress1"),
        "its read is lost, and asked again once its bound is out"
    );

    // egress1's lost read is asked again at once, and the update closes.
    wait_done(&updates, &update.id, Duration::from_secs(10)).await;
    background.abort();
    assert_eq!(
        mesh.read_bounds_of("egress1"),
        vec![bounds.read, bounds.restart_read, bounds.restart_read]
    );
    assert_eq!(mesh.lost_reads_left("egress1"), 0);
    assert_eq!(mesh.most_at_once.load(Ordering::SeqCst), 1);
    let done = updates.read(|file| file.closed[0].clone());
    assert_eq!(
        done.hives.keys().map(String::as_str).collect::<Vec<_>>(),
        ["egress1", "ingress1", "motherbee", "worker1"]
    );
    for (hive, _) in spokes {
        let entry = &done.hives[hive];
        let core = entry.core.outcome.as_ref().expect("core");
        assert_eq!(
            (core.status, core.by),
            (OutcomeStatus::Settled, RunKind::Explicit),
            "{hive}"
        );
        assert_eq!(entry.config.state, Standing::OnTarget, "{hive}");
    }
}

/// F2 (b) and (c): each hive is looked at by where it stands (in flight every few seconds; one that
/// did not answer, or was not read, through shared memory, read only once its orchestrator shows;
/// one left as it is now and then), and an action on it makes the background task look at once.
#[tokio::test]
async fn each_hive_is_looked_at_by_where_it_stands() {
    let in_flight = HiveEntry {
        core: CategoryEntry {
            state: Standing::InFlight,
            ..CategoryEntry::default()
        },
        ..HiveEntry::default()
    };
    assert_eq!(look_at(&in_flight), Look::Follow);
    assert_eq!(look_at(&HiveEntry::default()), Look::AnswerCheck);
    let excluded = HiveEntry {
        core: CategoryEntry {
            state: Standing::Excluded,
            exclusion: Some(Exclusion::RolledBack),
            ..CategoryEntry::default()
        },
        config: CategoryEntry {
            state: Standing::OnTarget,
            ..CategoryEntry::default()
        },
        ..HiveEntry::default()
    };
    assert_eq!(look_at(&excluded), Look::Slow);
    let bounds = quick();
    assert_eq!(Look::Slow.period(&bounds), bounds.slow_check);
    assert_eq!(Look::AnswerCheck.period(&bounds), bounds.retry_check);
    assert_eq!(Look::Follow.period(&bounds), bounds.flight_check);

    // An unknown hive whose orchestrator does not show in shared memory is not read.
    let updates = Updates::load(&temp_state("installation-update-cadence"));
    let mesh = FakeMesh::new(vec![motherbee_on_target(), worker_behind()]);
    let dyn_mesh: Arc<dyn UpdateMesh> = mesh.clone();
    let (id, _) = updates.change(|file| {
        let opened = file.open_or_resume(&target(), "http", None, now_epoch_ms());
        file.refresh_hives(
            &opened.0,
            &["motherbee".to_string(), "worker1".to_string()],
            Vec::new(),
        );
        opened
    });
    mesh.set_down("worker1", true);
    let reads = mesh.reads.load(Ordering::SeqCst);
    recheck(
        &updates,
        dyn_mesh.as_ref(),
        &id,
        "worker1",
        &target(),
        &bounds,
        Look::AnswerCheck,
    )
    .await;
    assert_eq!(mesh.reads.load(Ordering::SeqCst), reads);

    // A tick looks at what is due and says when the next one is: the slow hive in 10 min.
    mesh.set_down("worker1", false);
    updates.record(
        &id,
        "motherbee",
        Category::Core,
        Standing::OnTarget,
        None,
        None,
        None,
    );
    updates.record(
        &id,
        "motherbee",
        Category::Config,
        Standing::OnTarget,
        None,
        None,
        None,
    );
    updates.record_with(
        &id,
        "worker1",
        Category::Core,
        Standing::Excluded,
        Some(Exclusion::HandEdited),
        Some("why".to_string()),
        None,
        Some("what moves it".to_string()),
    );
    updates.record(
        &id,
        "worker1",
        Category::Config,
        Standing::OnTarget,
        None,
        None,
        None,
    );
    let next = tick(&updates, &dyn_mesh, &bounds)
        .await
        .expect("a next due");
    assert!(next >= now_epoch_ms() + bounds.slow_check.as_millis() as u64 - 1_000);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let due = updates.due_at("worker1");
    assert!(due > now_epoch_ms() + 60_000);
    // A per-hive update on it: due at once.
    look_after_action(&updates, Some("worker1"));
    assert_eq!(updates.due_at("worker1"), 0);
    // A reader of the update: only hives not looked at lately.
    updates.look_again("worker1", bounds.slow_check);
    updates.look_soon_all(Duration::from_secs(30));
    assert!(updates.due_at("worker1") > now_epoch_ms());
    updates.look_soon_all(Duration::ZERO);
    assert_eq!(updates.due_at("worker1"), 0);
}

/// F2 (d) end to end: an explicit run over a hive the operator rolled back applies its config and
/// leaves its core, the update is held and the answer says so (200); moving the hive back (its
/// per-hive update) and looking at it closes the update done.
#[tokio::test]
async fn a_rolled_back_hive_holds_the_update_until_it_moves() {
    let updates = Updates::load(&temp_state("installation-update-held"));
    let mesh = FakeMesh::new(vec![
        motherbee_on_target(),
        on_target("worker1"),
        egress_rolled_back((2, SHA_V2)),
    ]);
    let RunEnd::Update { update, .. } = explicit_run(&updates, &mesh, &[]).await else {
        panic!("an update opened");
    };
    assert_eq!(update.state, UpdateState::Held);
    assert_eq!(update.held_hives(), Some(vec!["egress1".to_string()]));
    assert_eq!(
        mesh.sent_to("egress1"),
        vec![json!({"category": "config", "manifest_version": 3, "manifest_hash": SHA_V3})]
    );
    let (status, body) = run_answer(
        "update_installation",
        RunEnd::Update {
            update: update.clone(),
            opening: Opening::Opened,
        },
        &UpdateRequest::default(),
        Duration::ZERO,
    );
    let body: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(status, 200);
    assert_eq!(body["status"], json!("ok"));
    assert_eq!(body["payload"]["held"], json!(["egress1"]));
    assert_eq!(updates.summary()["held"], json!(["egress1"]));

    // The operator moves egress1 back with its per-hive update; a look at it closes the update.
    mesh.set_snapshot("egress1", {
        let mut moved = egress_behind_config();
        moved["config"]["applied"] = json!({"version": 3, "sha256": SHA_V3});
        moved
    });
    let dyn_mesh: Arc<dyn UpdateMesh> = mesh.clone();
    recheck(
        &updates,
        dyn_mesh.as_ref(),
        &update.id,
        "egress1",
        &target(),
        &quick(),
        Look::Slow,
    )
    .await;
    assert_eq!(updates.settle_state(), Some(UpdateState::Done));
}

/// F5: the motherbee holds another target than the open update's (a downgrade): the update is
/// marked stale, the automatic step spends nothing and sends nothing, and the follow sends no
/// config; once the motherbee holds the target again, it acts.
#[tokio::test]
async fn a_stale_target_acts_on_nothing() {
    let updates = Updates::load(&temp_state("installation-update-stale"));
    let mesh = FakeMesh::new(vec![motherbee_on_target(), egress_behind_config()]);
    mesh.set_down("egress1", true);
    let RunEnd::Update { update, .. } = explicit_run(&updates, &mesh, &[]).await else {
        panic!("an update opened");
    };
    // The motherbee goes back to an older release.
    let mut older = motherbee_on_target();
    older["core"]["manifest_hash"] = json!(H_OLD);
    mesh.set_snapshot("motherbee", older);
    mesh.set_down("egress1", false);
    let dyn_mesh: Arc<dyn UpdateMesh> = mesh.clone();
    automatic_step(
        &updates,
        dyn_mesh.as_ref(),
        &update.id,
        "egress1",
        &target(),
        &quick(),
    )
    .await;
    assert!(mesh.sent_to("egress1").is_empty());
    let egress = entry_of(&updates, "egress1");
    assert!(egress.automatic.is_none(), "the attempt is not spent");
    let open = updates.read(|file| file.open.clone()).expect("open");
    let stale = open.stale_target.as_ref().expect("stale");
    assert_eq!(
        stale
            .target
            .core
            .as_ref()
            .map(|core| core.manifest_hash.as_str()),
        Some(H_OLD)
    );
    assert!(record_view(&open)["stale_target"].is_object());
    assert_eq!(updates.summary()["stale_target"], json!(true));

    // Back on the target's release: it acts again.
    mesh.set_snapshot("motherbee", motherbee_on_target());
    automatic_step(
        &updates,
        dyn_mesh.as_ref(),
        &update.id,
        "egress1",
        &target(),
        &quick(),
    )
    .await;
    assert_eq!(mesh.sent_to("egress1").len(), 1);
    assert!(updates
        .read(|file| file.current().cloned())
        .expect("current")
        .stale_target
        .is_none());
}

/// F6: a core update that does not answer in time is followed through /versions: one the hive took
/// settles as usual; one its record never shows is not_answered at the settle bound, for the
/// automatic step.
#[tokio::test]
async fn a_core_update_that_times_out_is_followed() {
    // The settle bound: long for the hive that took it (it settles by its second read, whatever
    // the machine's load), short for the one that never shows it.
    for (behaves, settles, settle) in [
        (Behaves::TimesOutButApplies, true, Duration::from_secs(30)),
        (Behaves::TimesOut, false, Duration::from_millis(200)),
    ] {
        let bounds = Bounds { settle, ..quick() };
        let updates = Updates::load(&temp_state("installation-update-timeout"));
        let mesh = FakeMesh::new(vec![motherbee_on_target(), worker_behind()]);
        mesh.behave("worker1", behaves);
        let dyn_mesh: Arc<dyn UpdateMesh> = mesh.clone();
        let motherbee = mesh.snapshot("motherbee");
        let RunEnd::Update { update, .. } = run_explicit(
            &updates,
            &dyn_mesh,
            "http",
            None,
            &[],
            &motherbee,
            &target(),
            &bounds,
            Duration::from_secs(10),
        )
        .await
        else {
            panic!("an update opened");
        };
        let worker = &update.hives["worker1"];
        assert_eq!(worker.core.state, Standing::InFlight, "settles: {settles}");
        assert_eq!(
            worker.core.outcome.as_ref().map(|o| o.status),
            Some(OutcomeStatus::NotAnswered)
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        while entry_of(&updates, "worker1").in_flight() {
            assert!(tokio::time::Instant::now() < deadline);
            follow(
                &updates,
                dyn_mesh.as_ref(),
                &update.id,
                "worker1",
                &target(),
                &bounds,
            )
            .await;
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        let worker = entry_of(&updates, "worker1");
        if settles {
            assert_eq!(worker.core.state, Standing::OnTarget, "{worker:#?}");
            assert_eq!(worker.config.state, Standing::OnTarget, "{worker:#?}");
        } else {
            assert_eq!(worker.core.state, Standing::Pending);
            assert_eq!(
                worker.core.outcome.as_ref().map(|o| o.status),
                Some(OutcomeStatus::NotAnswered)
            );
            assert!(worker.awaits_automatic_step());
        }
    }
}

/// F6: a hive that answers again with its core update under way is followed, and the automatic
/// step is not spent on it; its config goes once the core settled, by the background task.
#[tokio::test]
async fn a_core_found_under_way_is_followed_without_the_automatic_attempt() {
    let updates = Updates::load(&temp_state("installation-update-under-way"));
    let mesh = FakeMesh::new(vec![motherbee_on_target(), worker_behind()]);
    mesh.set_down("worker1", true);
    let RunEnd::Update { update, .. } = explicit_run(&updates, &mesh, &[]).await else {
        panic!("an update opened");
    };
    assert!(entry_of(&updates, "worker1").awaits_automatic_step());
    // It comes back restarting onto the release (its update went through after all).
    let mut restarting = worker_behind();
    restarting["core"]["last_update"] =
        json!({"phase": "restarting", "started_at_ms": "900", "updated": ["sy-orchestrator"]});
    mesh.set_snapshot("worker1", restarting);
    mesh.set_down("worker1", false);
    let dyn_mesh: Arc<dyn UpdateMesh> = mesh.clone();
    automatic_step(
        &updates,
        dyn_mesh.as_ref(),
        &update.id,
        "worker1",
        &target(),
        &quick(),
    )
    .await;
    let worker = entry_of(&updates, "worker1");
    assert_eq!(worker.core.state, Standing::InFlight);
    assert!(worker.automatic.is_none(), "the attempt is kept");
    assert!(mesh.sent_to("worker1").is_empty());
    assert_eq!(updates.due_at("worker1"), 0, "looked at at once");
    follow(
        &updates,
        dyn_mesh.as_ref(),
        &update.id,
        "worker1",
        &target(),
        &quick(),
    )
    .await;
    let worker = entry_of(&updates, "worker1");
    assert_eq!(worker.core.state, Standing::OnTarget, "{worker:#?}");
    assert_eq!(worker.config.state, Standing::OnTarget, "{worker:#?}");
    assert_eq!(
        worker.config.outcome.as_ref().map(|o| o.by),
        Some(RunKind::Background)
    );
    assert!(worker.automatic.is_none());
}

/// F7: an explicit run never skips a hive silently: one a task of this update acts on is recorded
/// as left to it, naming the task. One a task only reads is waited for, and so is one a task of a
/// closed update holds, which sends nothing more: leaving the hive to it would strand it. The run
/// then acts on it.
#[tokio::test]
async fn a_hive_another_task_holds_is_recorded() {
    let updates = Updates::load(&temp_state("installation-update-held-by"));
    let mut worker2 = worker_behind();
    worker2["hive_id"] = json!("worker2");
    let mesh = FakeMesh::new(vec![motherbee_on_target(), worker_behind(), worker2]);
    let id = open_update(&updates, "http", &["motherbee", "worker1", "worker2"]);
    // This update's follow, applying worker1's config: worker1 is left to it.
    let holder = updates
        .claim("worker1", "applying its config", &id)
        .expect("claimed");
    let dyn_mesh: Arc<dyn UpdateMesh> = mesh.clone();
    run_hive(
        &updates,
        dyn_mesh.as_ref(),
        &id,
        "worker1",
        &target(),
        &[],
        None,
        tokio::time::Instant::now(),
        &quick(),
    )
    .await;
    assert!(mesh.sent_to("worker1").is_empty());
    let worker = entry_of(&updates, "worker1");
    assert!(worker
        .core
        .reason
        .as_deref()
        .is_some_and(|why| why.contains("applying its config") && why.contains("of this update")));
    assert!(worker
        .core
        .next
        .as_deref()
        .is_some_and(|next| next.contains("update_installation again")));
    drop(holder);

    // The automatic step of a superseded update holding worker1, and a task that only reads
    // worker2: each is waited for, and the run acts on the hive once it lets go.
    let (the_target, bounds) = (target(), quick());
    for (hive, what, update) in [
        ("worker1", "the automatic step", "upd-older"),
        ("worker2", READING, id.as_str()),
    ] {
        let holder = updates.claim(hive, what, update).expect("claimed");
        let let_go = AtomicBool::new(false);
        let run = run_hive(
            &updates,
            dyn_mesh.as_ref(),
            &id,
            hive,
            &the_target,
            &[],
            None,
            tokio::time::Instant::now() + Duration::from_millis(100),
            &bounds,
        );
        tokio::join!(
            let_go_after(holder, Duration::from_millis(50), &let_go),
            run
        );
        assert_eq!(
            mesh.sent_to(hive).first(),
            Some(&json!({"category": "core", "manifest_version": 0, "manifest_hash": H_NEW})),
            "{hive}"
        );
    }
}

/// F8: one explicit run at a time; a second is told which one runs, and since when.
#[test]
fn one_explicit_run_at_a_time() {
    let updates = Updates::load(&temp_state("installation-update-running"));
    let first = updates.start_run("http").expect("the first run");
    updates.note_run_update("upd-1");
    let running = updates.start_run("executor").err().expect("refused");
    assert_eq!(running.origin, "http");
    let (status, body) = run_under_way("update_installation", &running);
    assert_eq!(status, 409);
    let body: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(body["error_code"], json!("INSTALLATION_UPDATE_RUNNING"));
    assert_eq!(body["payload"]["running"]["update"], json!("upd-1"));
    drop(first);
    assert!(updates.start_run("executor").is_ok());
}

/// F9: a core that did not settle stays pending while its record is the one it saw then (no warning
/// every bound); a new record makes it in flight again; and an explicit run sends its core update
/// again.
#[tokio::test]
async fn a_core_that_did_not_settle_stays_pending_until_its_record_changes() {
    let updates = Updates::load(&temp_state("installation-update-not-settled"));
    let stuck = snapshot(
        "worker1",
        "worker",
        core(
            Some(("update", H_NEW)),
            H_NEW,
            json!({"phase": "restarting", "started_at_ms": "700"}),
            Value::Null,
        ),
        config("current", 3, SHA_V3, Some(Some((3, SHA_V3)))),
    );
    let mesh = FakeMesh::new(vec![motherbee_on_target(), stuck.clone()]);
    let (id, _) = updates.change(|file| {
        let opened = file.open_or_resume(&target(), "http", None, now_epoch_ms());
        file.refresh_hives(
            &opened.0,
            &["motherbee".to_string(), "worker1".to_string()],
            Vec::new(),
        );
        opened
    });
    let mut not_settled = Outcome::new(OutcomeStatus::Error, RunKind::Explicit, 1, 2);
    not_settled.error_code = Some("CORE_NOT_SETTLED".to_string());
    not_settled.record_seen = Some(core_record_signature(&stuck["core"]));
    updates.record(
        &id,
        "worker1",
        Category::Core,
        Standing::Pending,
        Some("its core update was not over within the bound".to_string()),
        Some(not_settled),
        Some("run update_installation again".to_string()),
    );
    // The record does not change: a recheck leaves it pending (FakeMesh's read would settle a
    // restarting core, so the hive is read from a copy that stays).
    struct Frozen(Value);
    #[async_trait]
    impl UpdateMesh for Frozen {
        async fn versions(&self, _hive: &str, _timeout: Duration) -> Result<Value, String> {
            Ok(self.0.clone())
        }
        async fn update(&self, _: &str, _: Value, _: Duration) -> Result<Value, String> {
            Ok(json!({"status": "ok", "phase": "restarting"}))
        }
        fn orchestrator_connected(&self, _: &str) -> Option<bool> {
            Some(true)
        }
        fn hives(&self) -> Result<(Vec<String>, Vec<Value>), String> {
            Ok((
                vec!["motherbee".to_string(), "worker1".to_string()],
                Vec::new(),
            ))
        }
        fn motherbee(&self) -> &str {
            "motherbee"
        }
    }
    let frozen = Frozen(stuck.clone());
    recheck(
        &updates,
        &frozen,
        &id,
        "worker1",
        &target(),
        &quick(),
        Look::Slow,
    )
    .await;
    assert_eq!(entry_of(&updates, "worker1").core.state, Standing::Pending);
    // A new record (another start): in flight again.
    let mut moved = stuck.clone();
    moved["core"]["last_update"]["started_at_ms"] = json!("800");
    recheck(
        &updates,
        &Frozen(moved),
        &id,
        "worker1",
        &target(),
        &quick(),
        Look::Slow,
    )
    .await;
    assert_eq!(entry_of(&updates, "worker1").core.state, Standing::InFlight);

    // An explicit run over the same unchanged record sends the core update again.
    updates.record(
        &id,
        "worker1",
        Category::Core,
        Standing::Pending,
        None,
        Some({
            let mut outcome = Outcome::new(OutcomeStatus::Error, RunKind::Explicit, 1, 2);
            outcome.error_code = Some("CORE_NOT_SETTLED".to_string());
            outcome.record_seen = Some(core_record_signature(&stuck["core"]));
            outcome
        }),
        None,
    );
    let dyn_mesh: Arc<dyn UpdateMesh> = mesh.clone();
    run_hive(
        &updates,
        dyn_mesh.as_ref(),
        &id,
        "worker1",
        &target(),
        &[],
        Some(stuck),
        tokio::time::Instant::now(),
        &quick(),
    )
    .await;
    assert_eq!(
        mesh.sent_to("worker1").first(),
        Some(&json!({"category": "core", "manifest_version": 0, "manifest_hash": H_NEW}))
    );
}

/// The answers stay well inside one mesh frame (128 KiB) at the history's bound: 20 closed
/// updates and the open one, for an installation three times 8.x's, every text at its longest.
#[test]
fn the_answers_stay_well_inside_a_mesh_frame() {
    let long = "x".repeat(2 * TEXT_MAX);
    let hives: Vec<String> = (0..12).map(|n| format!("worker{n:02}")).collect();
    let mut file = UpdatesFile::default();
    for n in 0..=(HISTORY_KEPT as u64) {
        let target = Target {
            config: Some(ConfigTarget {
                version: n + 1,
                sha256: format!("{n:064}"),
            }),
            config_note: Some(cut(&long)),
            core_note: Some(cut(&long)),
            ..target()
        };
        let (id, _) = file.open_or_resume(&target, "executor", Some(&long), 1_000 + n);
        file.refresh_hives(
            &id,
            &hives,
            vec![json!({"hive": "half", "next": cut(&long)})],
        );
        let open = file.open.as_mut().expect("open");
        open.stale_target = Some(StaleTarget {
            target: target.clone(),
            since_ms: 1,
        });
        for entry in open.hives.values_mut() {
            for category in [Category::Core, Category::Config] {
                let mut outcome = Outcome::new(OutcomeStatus::Error, RunKind::Automatic, 1, 2);
                outcome.error_code = Some("CORE_UPDATE_ROLLED_BACK".to_string());
                outcome.detail = Some(cut(&long));
                outcome.restarted = Some(vec![
                    "sy-cognition".to_string(),
                    "sy-frontdesk-gov".to_string(),
                ]);
                outcome.previous_update = Some("1731160000000".to_string());
                outcome.record_seen = Some("1731160000000|1731160000001".to_string());
                outcome.seen = true;
                *entry.category_mut(category) = CategoryEntry {
                    state: Standing::Pending,
                    reason: Some(cut(&long)),
                    exclusion: None,
                    outcome: Some(outcome),
                    next: Some(cut(&long)),
                };
            }
            entry.automatic = Some(AutomaticAttempt {
                started_at_ms: 1,
                finished_at_ms: Some(2),
                result: Some("core pending, config pending".to_string()),
            });
        }
    }
    assert_eq!(file.closed.len(), HISTORY_KEPT);
    let updates = Updates::load(&temp_state("installation-update-size"));
    *lock(&updates.file) = file;
    let reply = super::super::installation_answer(
        "get_installation_update",
        200,
        "ok",
        updates.view(true),
        None,
    )
    .1;
    assert!(reply.len() < 96 * 1024, "{} bytes", reply.len());
    let summary = updates.summary().to_string();
    assert!(summary.len() < 8 * 1024, "{} bytes", summary.len());
}

// ---------------------------------------------------------------------------------------------
// set_installation_ai's apply and the background reads (FINDINGS A-205, A-204)
// ---------------------------------------------------------------------------------------------

/// A hive on the target's core, its config a version behind; its role is its name's (`worker1`, a
/// worker).
fn config_behind(hive: &str) -> Value {
    let mut behind = egress_behind_config();
    behind["hive_id"] = json!(hive);
    behind["role"] = json!(hive.trim_end_matches(|c: char| c.is_ascii_digit()));
    behind
}

/// An update opened on the target with these hives, all unknown.
fn open_update(updates: &Updates, origin: &str, hives: &[&str]) -> String {
    let hives: Vec<String> = hives.iter().map(|hive| hive.to_string()).collect();
    updates.change(|file| {
        let (id, _) = file.open_or_resume(&target(), origin, None, now_epoch_ms());
        file.refresh_hives(&id, &hives, Vec::new());
        id
    })
}

/// `set_installation_ai`'s apply of the target's version on `hive`, as the fake mesh answers it.
async fn apply_target_version(mesh: &FakeMesh, hive: &str) -> (InstallationApplied, Duration) {
    let started = std::time::Instant::now();
    let answer = mesh
        .update(
            hive,
            json!({"category": "config", "manifest_version": 3, "manifest_hash": SHA_V3}),
            Duration::from_secs(1),
        )
        .await
        .expect("its answer");
    (
        InstallationApplied {
            outcome: InstallationApplyOutcome::Ok(answer),
            attempts: 1,
        },
        started.elapsed(),
    )
}

/// A task holding a hive that lets it go after `after`, saying so in `let_go` first.
async fn let_go_after(holder: Claim<'_>, after: Duration, let_go: &AtomicBool) {
    tokio::time::sleep(after).await;
    let_go.store(true, Ordering::SeqCst);
    drop(holder);
}

/// FINDINGS A-205: `set_installation_ai`'s apply holds each hive while it applies it: a background
/// read neither reads nor records the hive meanwhile. A task holding a hive, one that reads it or
/// one that acts on it, is waited for, and the hive applied once it lets go, with its own time
/// left; one that holds it until the apply would have less than that (one call, never under 10 s)
/// before the change's ceiling keeps it, and the change leaves the hive to it, sending nothing and
/// saying so in the record.
#[tokio::test]
async fn the_change_s_apply_holds_each_hive_it_applies() {
    let updates = Updates::load(&temp_state("installation-update-change-holds"));
    let mesh = FakeMesh::new(vec![
        motherbee_on_target(),
        config_behind("worker1"),
        config_behind("worker2"),
        config_behind("ingress1"),
        config_behind("egress1"),
    ]);
    let dyn_mesh: Arc<dyn UpdateMesh> = mesh.clone();
    let id = open_update(
        &updates,
        "set_installation_ai",
        &["motherbee", "worker1", "worker2", "ingress1", "egress1"],
    );
    // The change's ceiling, 30 s away: the apply waits for a holder up to 10 s before it, its own
    // time (one call of the quick bounds is less than the 10 s floor).
    let (the_target, bounds, deadline) = (
        target(),
        quick(),
        tokio::time::Instant::now() + Duration::from_secs(30),
    );
    let call = bounds.update_call;
    let (updates_ref, mesh_ref, dyn_ref, id_ref, target_ref, bounds_ref) = (
        &*updates,
        &*mesh,
        dyn_mesh.as_ref(),
        id.as_str(),
        &the_target,
        &bounds,
    );

    // A background read of worker1 while the change applies it: no read, nothing recorded.
    let reads = mesh.reads.load(Ordering::SeqCst);
    let change = apply_change_on_hive(
        &updates,
        &id,
        "worker1",
        &the_target,
        deadline,
        call,
        move |_within| async move {
            recheck(
                updates_ref,
                dyn_ref,
                id_ref,
                "worker1",
                target_ref,
                bounds_ref,
                Look::AnswerCheck,
            )
            .await;
            assert_eq!(mesh_ref.reads.load(Ordering::SeqCst), reads);
            let holder = updates_ref.holder("worker1").expect("held");
            assert_eq!(holder.what, "set_installation_ai's apply");
            assert_eq!(holder.update, id_ref);
            apply_target_version(mesh_ref, "worker1").await
        },
    )
    .await;
    assert!(matches!(change, ChangeApply::Applied(..)), "{change:?}");
    assert!(
        updates.holder("worker1").is_none(),
        "released once recorded"
    );
    let worker = entry_of(&updates, "worker1");
    assert_eq!(worker.config.state, Standing::OnTarget);
    assert_eq!(
        worker.config.outcome.as_ref().map(|o| (o.status, o.by)),
        Some((OutcomeStatus::Ok, RunKind::Explicit))
    );
    // Read after the change, it is on the target, both categories.
    recheck(
        &updates,
        dyn_mesh.as_ref(),
        &id,
        "worker1",
        &the_target,
        &bounds,
        Look::AnswerCheck,
    )
    .await;
    let worker = entry_of(&updates, "worker1");
    assert!(worker.on_target(), "{worker:#?}");

    // A task that reads ingress1 (a background read), and one that acts on egress1 (the automatic
    // step of the update the change superseded): each is waited for, and the change applies the
    // hive once it lets go, with its own time left.
    for (hive, what, update) in [
        ("ingress1", READING, id.as_str()),
        ("egress1", "the automatic step", "upd-older"),
    ] {
        let holder = updates.claim(hive, what, update).expect("claimed");
        let let_go = AtomicBool::new(false);
        let let_go_ref = &let_go;
        let change = apply_change_on_hive(
            &updates,
            &id,
            hive,
            &the_target,
            deadline,
            call,
            move |within| async move {
                assert!(let_go_ref.load(Ordering::SeqCst), "{hive}: it let go first");
                assert!(within >= Duration::from_secs(10), "{hive}: {within:?}");
                apply_target_version(mesh_ref, hive).await
            },
        );
        let ((), change) = tokio::join!(
            let_go_after(holder, Duration::from_millis(50), &let_go),
            change
        );
        assert!(
            matches!(change, ChangeApply::Applied(..)),
            "{hive}: {change:?}"
        );
        assert_eq!(
            entry_of(&updates, hive).config.state,
            Standing::OnTarget,
            "{hive}"
        );
    }

    // A task that lets worker2 go only when the apply would have less than its own time left before
    // the ceiling (here 10.3 s away, the task letting go after 1 s) keeps it: the change stops
    // waiting 10 s before the ceiling, leaves worker2 to it, sends it nothing, and the record says
    // so. Applied then, it would read not_answered though it applies the version.
    let actor = updates
        .claim("worker2", "the automatic step", "upd-older")
        .expect("claimed");
    let let_go = AtomicBool::new(false);
    let started = tokio::time::Instant::now();
    let ceiling = started + Duration::from_millis(10_300);
    let change = async {
        let change =
            apply_change_on_hive(
                &updates,
                &id,
                "worker2",
                &the_target,
                ceiling,
                call,
                move |_within| async move {
                    unreachable!("nothing is sent to a hive another task holds")
                },
            )
            .await;
        (change, tokio::time::Instant::now())
    };
    let ((), (change, left_at)) =
        tokio::join!(let_go_after(actor, Duration::from_secs(1), &let_go), change);
    let waited = left_at - started;
    assert!(
        waited >= Duration::from_millis(300) && waited < Duration::from_secs(1),
        "it waits until 10 s before the ceiling, not for the task: {waited:?}"
    );
    let ChangeApply::LeftToTask(why) = change else {
        panic!("left to the task: {change:?}");
    };
    assert!(
        why.contains("the automatic step of the superseded update upd-older"),
        "{why}"
    );
    assert!(mesh.sent_to("worker2").is_empty());
    let worker2 = entry_of(&updates, "worker2");
    assert_eq!(worker2.config.reason.as_deref(), Some(why.as_str()));
    assert!(worker2
        .config
        .next
        .as_deref()
        .is_some_and(|next| next.contains("update_installation again")));
}

/// FINDINGS A-205: a background read that began before a hive's last outcome shows the hive as it
/// was before that outcome. The outcome stands for its category (the change's apply, answered while
/// the read was under way); the read is recorded for the other category.
#[tokio::test]
async fn a_read_older_than_the_hive_s_last_outcome_is_not_recorded() {
    // A read that says it began, and answers once the test lets it: the hive as it was then.
    struct Gated {
        snapshot: Value,
        began: tokio::sync::Notify,
        answer: tokio::sync::Notify,
    }
    #[async_trait]
    impl UpdateMesh for Gated {
        async fn versions(&self, _hive: &str, _timeout: Duration) -> Result<Value, String> {
            self.began.notify_one();
            self.answer.notified().await;
            Ok(self.snapshot.clone())
        }
        async fn update(&self, _: &str, _: Value, _: Duration) -> Result<Value, String> {
            unreachable!()
        }
        fn orchestrator_connected(&self, _: &str) -> Option<bool> {
            Some(true)
        }
        fn hives(&self) -> Result<(Vec<String>, Vec<Value>), String> {
            Ok((
                vec!["motherbee".to_string(), "worker1".to_string()],
                Vec::new(),
            ))
        }
        fn motherbee(&self) -> &str {
            "motherbee"
        }
    }
    let gated = || Gated {
        snapshot: config_behind("worker1"),
        began: tokio::sync::Notify::new(),
        answer: tokio::sync::Notify::new(),
    };
    let updates = Updates::load(&temp_state("installation-update-stale-read"));
    let id = open_update(&updates, "set_installation_ai", &["motherbee", "worker1"]);
    let (the_target, bounds) = (target(), quick());
    let before = gated();
    let look = recheck(
        &updates,
        &before,
        &id,
        "worker1",
        &the_target,
        &bounds,
        Look::AnswerCheck,
    );
    let meanwhile = async {
        before.began.notified().await;
        // worker1 applies the version, and the apply is recorded, while the read is under way.
        tokio::time::sleep(Duration::from_millis(5)).await;
        let now = now_epoch_ms();
        let mut applied = Outcome::new(OutcomeStatus::Ok, RunKind::Explicit, now, now);
        applied.attempts = 1;
        updates.record(
            &id,
            "worker1",
            Category::Config,
            Standing::OnTarget,
            None,
            Some(applied.clone()),
            None,
        );
        before.answer.notify_one();
        applied
    };
    let ((), applied) = tokio::join!(look, meanwhile);
    let worker = entry_of(&updates, "worker1");
    assert_eq!(worker.config.state, Standing::OnTarget, "{worker:#?}");
    assert_eq!(worker.config.outcome.as_ref(), Some(&applied));
    assert_eq!(worker.core.state, Standing::OnTarget, "{worker:#?}");

    // A read that begins after that outcome is recorded as usual: this one shows the version not
    // applied, and the config is pending again.
    tokio::time::sleep(Duration::from_millis(5)).await;
    let after = gated();
    after.answer.notify_one();
    recheck(
        &updates,
        &after,
        &id,
        "worker1",
        &the_target,
        &bounds,
        Look::Slow,
    )
    .await;
    assert_eq!(
        entry_of(&updates, "worker1").config.state,
        Standing::Pending
    );
}

/// FINDINGS A-204: each hive of `set_installation_ai`'s apply is recorded as its own apply ends,
/// from its own start to its own end: its elapsed is its own (the answer's timings_ms.hives), never
/// the slowest hive's.
#[tokio::test]
async fn the_change_records_each_hive_s_own_time() {
    let updates = Updates::load(&temp_state("installation-update-change-times"));
    let id = open_update(
        &updates,
        "set_installation_ai",
        &["motherbee", "worker1", "egress1"],
    );
    let (the_target, deadline) = (
        target(),
        tokio::time::Instant::now() + Duration::from_secs(10),
    );
    let taking = |delay: Duration| {
        move |_within: Duration| async move {
            let started = std::time::Instant::now();
            tokio::time::sleep(delay).await;
            (
                InstallationApplied {
                    outcome: InstallationApplyOutcome::Ok(
                        json!({"status": "ok", "restarted": [], "unchanged": []}),
                    ),
                    attempts: 1,
                },
                started.elapsed(),
            )
        }
    };
    let before = now_epoch_ms();
    let (fast, slow) = tokio::join!(
        apply_change_on_hive(
            &updates,
            &id,
            "worker1",
            &the_target,
            deadline,
            quick().update_call,
            taking(Duration::from_millis(10)),
        ),
        apply_change_on_hive(
            &updates,
            &id,
            "egress1",
            &the_target,
            deadline,
            quick().update_call,
            taking(Duration::from_millis(300)),
        ),
    );
    let own = |change: &ChangeApply| match change {
        ChangeApply::Applied(_, elapsed) => elapsed.as_millis() as u64,
        other => panic!("applied: {other:?}"),
    };
    let (fast_ms, slow_ms) = (own(&fast), own(&slow));
    let recorded = |hive: &str| {
        entry_of(&updates, hive)
            .config
            .outcome
            .expect("its outcome")
    };
    let (worker, egress) = (recorded("worker1"), recorded("egress1"));
    assert_eq!(worker.elapsed_ms(), fast_ms, "{worker:?}");
    assert_eq!(egress.elapsed_ms(), slow_ms, "{egress:?}");
    assert!(slow_ms >= 300, "{slow_ms}");
    assert!(worker.elapsed_ms() <= egress.elapsed_ms());
    assert!(worker.at_ms <= egress.at_ms, "{worker:?} {egress:?}");
    assert!(worker.started_at_ms >= before && egress.started_at_ms >= before);
    // The view shows each one's own.
    let view = record_view(&updates.read(|file| file.open.clone()).expect("open"));
    assert_eq!(
        view["hives"]["worker1"]["config"]["outcome"]["elapsed_ms"],
        json!(fast_ms)
    );
}

/// FINDINGS A-204: a read that finds a category on the target, its last outcome saying otherwise,
/// records that it saw it there, as the category's own status on the target (`settled` for a core,
/// `ok` for a config), by the background task, with the read's times: the gate's egress1, rolled
/// back, left alone by the automatic step, then moved back by its own updates. A category whose
/// last outcome says on the target keeps it.
#[tokio::test]
async fn a_read_records_a_hive_it_sees_on_the_target_after_another_outcome() {
    let updates = Updates::load(&temp_state("installation-update-seen"));
    let mesh = FakeMesh::new(vec![motherbee_on_target(), egress_rolled_back((2, SHA_V2))]);
    mesh.set_down("egress1", true);
    let RunEnd::Update { update, .. } = explicit_run(&updates, &mesh, &[]).await else {
        panic!("an update opened");
    };
    let dyn_mesh: Arc<dyn UpdateMesh> = mesh.clone();
    mesh.set_down("egress1", false);
    automatic_step(
        &updates,
        dyn_mesh.as_ref(),
        &update.id,
        "egress1",
        &target(),
        &quick(),
    )
    .await;
    let egress = entry_of(&updates, "egress1");
    for slot in [&egress.core, &egress.config] {
        assert_eq!(
            slot.outcome.as_ref().map(|o| (o.status, o.by)),
            Some((OutcomeStatus::Excluded, RunKind::Automatic))
        );
    }

    // The operator moves it back, its core and its config, with its per-hive updates; SY.admin
    // looks at it.
    mesh.set_snapshot("egress1", {
        let mut moved = egress_behind_config();
        moved["config"]["applied"] = json!({"version": 3, "sha256": SHA_V3});
        moved
    });
    let read_from = now_epoch_ms();
    recheck(
        &updates,
        dyn_mesh.as_ref(),
        &update.id,
        "egress1",
        &target(),
        &quick(),
        Look::Slow,
    )
    .await;
    let egress = entry_of(&updates, "egress1");
    for (slot, status) in [
        (&egress.core, OutcomeStatus::Settled),
        (&egress.config, OutcomeStatus::Ok),
    ] {
        assert_eq!((slot.state, slot.exclusion), (Standing::OnTarget, None));
        let seen = slot.outcome.as_ref().expect("what the read saw");
        assert_eq!(
            (seen.status, seen.by, seen.attempts, seen.seen),
            (status, RunKind::Background, 0, true)
        );
        assert!(seen.started_at_ms >= read_from && seen.at_ms >= seen.started_at_ms);
        assert!(
            seen.detail
                .as_deref()
                .is_some_and(|detail| detail.contains("said excluded (automatic)")),
            "{seen:?}"
        );
    }

    // Read again: its outcomes say on the target now, and stay as they are.
    recheck(
        &updates,
        dyn_mesh.as_ref(),
        &update.id,
        "egress1",
        &target(),
        &quick(),
        Look::Slow,
    )
    .await;
    let again = entry_of(&updates, "egress1");
    assert_eq!(
        (&again.core.outcome, &again.config.outcome),
        (&egress.core.outcome, &egress.config.outcome)
    );
    assert_eq!(updates.settle_state(), Some(UpdateState::Done));
}

/// FINDINGS A-204, in every read that decides: the automatic step, an explicit run's pass and the
/// follow's config step record a category they find on the target, its last outcome saying
/// otherwise, as seen there, `by` their own kind, never keeping the old outcome; one whose last
/// outcome says on the target keeps it.
#[tokio::test]
async fn every_read_records_a_hive_it_sees_on_the_target() {
    let updates = Updates::load(&temp_state("installation-update-seen-everywhere"));
    // egress1 did not answer the run, and comes back with the target applied after all (P-13);
    // worker1, in error in this update, was moved by its per-hive updates; worker2's core restarts
    // onto the release, and its copy, sync_pending at the run's bound, was applied meanwhile.
    let mut worker2 = config_behind("worker2");
    worker2["core"]["last_update"] =
        json!({"phase": "restarting", "started_at_ms": "900", "updated": ["sy-orchestrator"]});
    worker2["config"]["applied"] = json!({"version": 3, "sha256": SHA_V3});
    let mesh = FakeMesh::new(vec![
        motherbee_on_target(),
        on_target("egress1"),
        on_target("worker1"),
        worker2,
    ]);
    let dyn_mesh: Arc<dyn UpdateMesh> = mesh.clone();
    let id = open_update(
        &updates,
        "http",
        &["motherbee", "egress1", "worker1", "worker2"],
    );
    let last = |hive: &str, category: Category, status: OutcomeStatus, state: Standing| {
        let now = now_epoch_ms();
        let mut outcome = Outcome::new(status, RunKind::Explicit, now, now);
        outcome.error_code = (status == OutcomeStatus::Error).then(|| "RESTART_FAILED".to_string());
        updates.record(&id, hive, category, state, None, Some(outcome), None);
    };
    for category in [Category::Core, Category::Config] {
        last(
            "egress1",
            category,
            OutcomeStatus::NotAnswered,
            Standing::Pending,
        );
        last("worker1", category, OutcomeStatus::Error, Standing::Pending);
    }
    last(
        "worker2",
        Category::Core,
        OutcomeStatus::Restarting,
        Standing::InFlight,
    );
    last(
        "worker2",
        Category::Config,
        OutcomeStatus::SyncPending,
        Standing::Pending,
    );
    let (the_target, bounds) = (target(), quick());

    automatic_step(
        &updates,
        dyn_mesh.as_ref(),
        &id,
        "egress1",
        &the_target,
        &bounds,
    )
    .await;
    run_hive(
        &updates,
        dyn_mesh.as_ref(),
        &id,
        "worker1",
        &the_target,
        &[],
        None,
        tokio::time::Instant::now() + Duration::from_secs(1),
        &bounds,
    )
    .await;
    // The follow: worker2's core restarting, then settled; its config step after it.
    for _ in 0..2 {
        follow(
            &updates,
            dyn_mesh.as_ref(),
            &id,
            "worker2",
            &the_target,
            &bounds,
        )
        .await;
    }

    for (hive, by, said) in [
        (
            "egress1",
            RunKind::Automatic,
            "said not_answered (explicit)",
        ),
        ("worker1", RunKind::Explicit, "said error (explicit)"),
    ] {
        let entry = entry_of(&updates, hive);
        for (slot, status) in [
            (&entry.core, OutcomeStatus::Settled),
            (&entry.config, OutcomeStatus::Ok),
        ] {
            assert_eq!(slot.state, Standing::OnTarget, "{hive}");
            let seen = slot.outcome.as_ref().expect("what the read saw");
            assert_eq!(
                (seen.status, seen.by, seen.seen),
                (status, by, true),
                "{hive}"
            );
            assert!(
                seen.detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains(said)),
                "{hive}: {seen:?}"
            );
        }
    }
    let worker2 = entry_of(&updates, "worker2");
    let core = worker2.core.outcome.as_ref().expect("its core");
    assert_eq!(
        (worker2.core.state, core.status, core.seen),
        (Standing::OnTarget, OutcomeStatus::Settled, false),
        "the follow's own settle stands: {core:?}"
    );
    let config = worker2.config.outcome.as_ref().expect("its config");
    assert_eq!(
        (worker2.config.state, config.status, config.by, config.seen),
        (
            Standing::OnTarget,
            OutcomeStatus::Ok,
            RunKind::Explicit,
            true
        )
    );
    for hive in ["egress1", "worker1", "worker2"] {
        assert!(mesh.sent_to(hive).is_empty(), "{hive}");
    }
    // The view says seen only of an outcome that is one.
    let view = record_view(&updates.read(|file| file.open.clone()).expect("open"));
    assert_eq!(
        view["hives"]["worker1"]["core"]["outcome"]["seen"],
        json!(true)
    );
    assert!(view["hives"]["worker2"]["core"]["outcome"]
        .get("seen")
        .is_none());
}

/// A hive's next look follows the open update's cadence: one the superseded update read every
/// 10 min, which the new run records not_answered, gets its 30 s check of shared memory from the
/// new update's first look, not 10 min after the old update's last.
#[tokio::test]
async fn a_new_update_looks_at_each_hive_by_its_own_cadence() {
    let bounds = Bounds {
        retry_check: Duration::from_secs(30),
        slow_check: Duration::from_secs(600),
        ..quick()
    };
    let updates = Updates::load(&temp_state("installation-update-own-cadence"));
    let mesh = FakeMesh::new(vec![motherbee_on_target(), worker_behind()]);
    let dyn_mesh: Arc<dyn UpdateMesh> = mesh.clone();

    // The old update, for the file's version 2: worker1 left in error, read every 10 min.
    let older = Target {
        config: Some(ConfigTarget {
            version: 2,
            sha256: SHA_V2.to_string(),
        }),
        ..target()
    };
    let old = updates.change(|file| {
        let (id, _) = file.open_or_resume(&older, "http", None, now_epoch_ms());
        file.refresh_hives(
            &id,
            &["motherbee".to_string(), "worker1".to_string()],
            Vec::new(),
        );
        id
    });
    for category in [Category::Core, Category::Config] {
        let mut error = Outcome::new(OutcomeStatus::Error, RunKind::Explicit, 1, 2);
        error.error_code = Some("RESTART_FAILED".to_string());
        updates.record(
            &old,
            "worker1",
            category,
            Standing::Pending,
            None,
            Some(error),
            None,
        );
        updates.record(
            &old,
            "motherbee",
            category,
            Standing::OnTarget,
            None,
            None,
            None,
        );
    }
    tick(&updates, &dyn_mesh, &bounds).await;
    let ten_minutes = now_epoch_ms() + bounds.slow_check.as_millis() as u64 - 60_000;
    assert!(
        updates.due_at("worker1") > ten_minutes,
        "on the 10 min cadence"
    );
    // The read the look started ends before the new run.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while updates.holder("worker1").is_some() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // A run for the motherbee's version 3 supersedes it, and worker1 does not answer it.
    mesh.set_down("worker1", true);
    let RunEnd::Update { opening, .. } = explicit_run(&updates, &mesh, &[]).await else {
        panic!("an update opened");
    };
    assert_eq!(opening, Opening::Superseded(old.clone()));
    assert!(entry_of(&updates, "worker1").awaits_automatic_step());

    // The new update's first look: worker1's check comes in 30 s, not when the old cadence said.
    tick(&updates, &dyn_mesh, &bounds).await;
    let due = updates.due_at("worker1");
    assert!(
        due <= now_epoch_ms() + bounds.retry_check.as_millis() as u64,
        "{due}"
    );
}

/// A task whose update is closed (superseded, or done) sends nothing more: an explicit run's pass
/// still going after the run answered, the automatic step (before its attempt, and between its
/// core and its config), a config apply (the follow's). A send already under way when its update
/// closes finishes as before, its outcome not recorded.
#[tokio::test]
async fn a_task_of_a_closed_update_sends_nothing_more() {
    let updates = Updates::load(&temp_state("installation-update-closed-sends"));
    let mesh = FakeMesh::new(vec![
        motherbee_on_target(),
        worker_behind(),
        config_behind("egress1"),
    ]);
    let dyn_mesh: Arc<dyn UpdateMesh> = mesh.clone();
    let hives = ["motherbee", "worker1", "egress1"];
    let old = open_update(&updates, "http", &hives);
    // egress1 did not answer it: its automatic step would finish it.
    for category in [Category::Core, Category::Config] {
        let not_answered = Outcome::new(OutcomeStatus::NotAnswered, RunKind::Explicit, 1, 2);
        updates.record(
            &old,
            "egress1",
            category,
            Standing::Pending,
            None,
            Some(not_answered),
            None,
        );
    }
    // A change of the installation file supersedes it.
    let newer = Target {
        config: Some(ConfigTarget {
            version: 4,
            sha256: "dddd".to_string(),
        }),
        ..target()
    };
    updates.change(|file| file.open_or_resume(&newer, "set_installation_ai", None, now_epoch_ms()));

    // The old update's pass on worker1 (its core behind) and on egress1 (its config behind),
    // egress1's automatic step and a config apply: each may read; none sends.
    for hive in ["worker1", "egress1"] {
        run_hive(
            &updates,
            dyn_mesh.as_ref(),
            &old,
            hive,
            &target(),
            &[],
            None,
            tokio::time::Instant::now() + Duration::from_secs(1),
            &quick(),
        )
        .await;
    }
    automatic_step(
        &updates,
        dyn_mesh.as_ref(),
        &old,
        "egress1",
        &target(),
        &quick(),
    )
    .await;
    config_update(
        &updates,
        dyn_mesh.as_ref(),
        &old,
        "egress1",
        &target(),
        RunKind::Background,
        &quick(),
    )
    .await;
    assert!(mesh.sent_to("worker1").is_empty());
    assert!(mesh.sent_to("egress1").is_empty());

    // A config apply under way when its update closes: it finishes, and the closed update keeps
    // the send as it stood.
    struct Answering {
        began: tokio::sync::Notify,
        answer: tokio::sync::Notify,
        sent: AtomicUsize,
    }
    #[async_trait]
    impl UpdateMesh for Answering {
        async fn versions(&self, _hive: &str, _timeout: Duration) -> Result<Value, String> {
            unreachable!("a config apply reads nothing")
        }
        async fn update(&self, _: &str, _: Value, _: Duration) -> Result<Value, String> {
            self.sent.fetch_add(1, Ordering::SeqCst);
            self.began.notify_one();
            self.answer.notified().await;
            Ok(json!({"status": "ok", "restarted": [], "unchanged": []}))
        }
        fn orchestrator_connected(&self, _: &str) -> Option<bool> {
            Some(true)
        }
        fn hives(&self) -> Result<(Vec<String>, Vec<Value>), String> {
            Ok((vec!["motherbee".to_string()], Vec::new()))
        }
        fn motherbee(&self) -> &str {
            "motherbee"
        }
    }
    let answering = Answering {
        began: tokio::sync::Notify::new(),
        answer: tokio::sync::Notify::new(),
        sent: AtomicUsize::new(0),
    };
    let current = open_update(&updates, "http", &hives);
    let the_target = target();
    let bounds = quick();
    let apply = config_update(
        &updates,
        &answering,
        &current,
        "egress1",
        &the_target,
        RunKind::Explicit,
        &bounds,
    );
    let meanwhile = async {
        answering.began.notified().await;
        updates.change(|file| {
            file.open_or_resume(&newer, "set_installation_ai", None, now_epoch_ms())
        });
        answering.answer.notify_one();
    };
    tokio::join!(apply, meanwhile);
    assert_eq!(answering.sent.load(Ordering::SeqCst), 1);
    let closed = updates
        .read(|file| {
            file.closed
                .iter()
                .find(|update| update.id == current)
                .cloned()
        })
        .expect("closed");
    assert_eq!(closed.state, UpdateState::Superseded);
    assert_eq!(
        closed.hives["egress1"]
            .config
            .outcome
            .as_ref()
            .map(|o| o.status),
        Some(OutcomeStatus::Sent)
    );

    // The automatic step's update closes after its attempt is recorded, while its core update is
    // under way: the core is refused, and its config, which goes after a core that failed (one
    // version everywhere), is not sent.
    struct Refusing {
        snapshots: HashMap<String, Value>,
        began: tokio::sync::Notify,
        answer: tokio::sync::Notify,
        sent: Mutex<Vec<Value>>,
    }
    #[async_trait]
    impl UpdateMesh for Refusing {
        async fn versions(&self, hive: &str, _timeout: Duration) -> Result<Value, String> {
            self.snapshots
                .get(hive)
                .cloned()
                .ok_or_else(|| "no such hive".to_string())
        }
        async fn update(&self, _: &str, payload: Value, _: Duration) -> Result<Value, String> {
            lock(&self.sent).push(payload.clone());
            if payload["category"] == json!("core") {
                self.began.notify_one();
                self.answer.notified().await;
                return Ok(json!({
                    "status": "error",
                    "error_code": "UPDATE_FAILED",
                    "message": "a core binary does not match its manifest",
                }));
            }
            Ok(json!({"status": "ok", "restarted": [], "unchanged": []}))
        }
        fn orchestrator_connected(&self, _: &str) -> Option<bool> {
            Some(true)
        }
        fn hives(&self) -> Result<(Vec<String>, Vec<Value>), String> {
            Ok((
                vec!["motherbee".to_string(), "worker1".to_string()],
                Vec::new(),
            ))
        }
        fn motherbee(&self) -> &str {
            "motherbee"
        }
    }
    let refusing = Refusing {
        snapshots: [motherbee_on_target(), worker_behind()]
            .into_iter()
            .map(|snapshot| {
                (
                    snapshot["hive_id"].as_str().expect("hive").to_string(),
                    snapshot,
                )
            })
            .collect(),
        began: tokio::sync::Notify::new(),
        answer: tokio::sync::Notify::new(),
        sent: Mutex::new(Vec::new()),
    };
    let fresh = open_update(&updates, "http", &["motherbee", "worker1"]);
    for category in [Category::Core, Category::Config] {
        let not_answered = Outcome::new(OutcomeStatus::NotAnswered, RunKind::Explicit, 1, 2);
        updates.record(
            &fresh,
            "worker1",
            category,
            Standing::Pending,
            None,
            Some(not_answered),
            None,
        );
    }
    let step = automatic_step(&updates, &refusing, &fresh, "worker1", &the_target, &bounds);
    let meanwhile = async {
        refusing.began.notified().await;
        updates.change(|file| {
            file.open_or_resume(&newer, "set_installation_ai", None, now_epoch_ms())
        });
        refusing.answer.notify_one();
    };
    tokio::join!(step, meanwhile);
    let sent = lock(&refusing.sent).clone();
    assert_eq!(
        sent,
        vec![json!({"category": "core", "manifest_version": 0, "manifest_hash": H_NEW})],
        "the core update under way, and no config after it"
    );
    let closed = updates
        .read(|file| {
            file.closed
                .iter()
                .find(|update| update.id == fresh)
                .cloned()
        })
        .expect("closed");
    assert!(
        closed.hives["worker1"].automatic.is_some(),
        "the attempt was recorded before the update closed"
    );
}

/// A slow read of one hive holds up no other hive (FINDINGS A-263): the background task's re-read
/// of worker1 goes beside worker2's, which has not answered, and an explicit run's pass on worker1
/// is never left to a read of another hive. At a run's open every hive not read yet gets such a
/// re-read; until 0.1.137 they were read one at a time, each queued behind the read under way.
#[tokio::test]
async fn a_slow_read_of_one_hive_holds_up_no_pass_on_another() {
    // worker2's read answers once the test lets it, every other one at once; a config update
    // applies its version.
    struct SlowWorker2 {
        snapshots: Mutex<HashMap<String, Value>>,
        began: tokio::sync::Notify,
        answer: tokio::sync::Notify,
        sent: Mutex<Vec<(String, Value)>>,
    }
    #[async_trait]
    impl UpdateMesh for SlowWorker2 {
        async fn versions(&self, hive: &str, _timeout: Duration) -> Result<Value, String> {
            if hive == "worker2" {
                self.began.notify_one();
                self.answer.notified().await;
            }
            lock(&self.snapshots)
                .get(hive)
                .cloned()
                .ok_or_else(|| "no such hive".to_string())
        }
        async fn update(&self, hive: &str, payload: Value, _: Duration) -> Result<Value, String> {
            lock(&self.sent).push((hive.to_string(), payload.clone()));
            if let Some(snapshot) = lock(&self.snapshots).get_mut(hive) {
                snapshot["config"]["applied"] = json!({
                    "version": payload["manifest_version"],
                    "sha256": payload["manifest_hash"],
                });
            }
            Ok(json!({"status": "ok", "restarted": [], "unchanged": []}))
        }
        fn orchestrator_connected(&self, _: &str) -> Option<bool> {
            Some(true)
        }
        fn hives(&self) -> Result<(Vec<String>, Vec<Value>), String> {
            Ok((
                vec![
                    "motherbee".to_string(),
                    "worker1".to_string(),
                    "worker2".to_string(),
                ],
                Vec::new(),
            ))
        }
        fn motherbee(&self) -> &str {
            "motherbee"
        }
    }
    let mesh = SlowWorker2 {
        snapshots: Mutex::new(
            [
                motherbee_on_target(),
                config_behind("worker1"),
                config_behind("worker2"),
            ]
            .into_iter()
            .map(|snapshot| {
                (
                    snapshot["hive_id"].as_str().expect("hive").to_string(),
                    snapshot,
                )
            })
            .collect(),
        ),
        began: tokio::sync::Notify::new(),
        answer: tokio::sync::Notify::new(),
        sent: Mutex::new(Vec::new()),
    };
    let updates = Updates::load(&temp_state("installation-update-slow-read"));
    let id = open_update(&updates, "http", &["motherbee", "worker1", "worker2"]);
    let (the_target, bounds) = (target(), quick());
    // The background task's re-reads of worker2, whose read is slow, and of worker1 beside it.
    let slow = recheck(
        &updates,
        &mesh,
        &id,
        "worker2",
        &the_target,
        &bounds,
        Look::AnswerCheck,
    );
    let beside = recheck(
        &updates,
        &mesh,
        &id,
        "worker1",
        &the_target,
        &bounds,
        Look::AnswerCheck,
    );
    let run = async {
        mesh.began.notified().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        // worker1 was read while worker2's read has not answered, and let go.
        let worker1 = entry_of(&updates, "worker1");
        assert_eq!(worker1.config.state, Standing::Pending, "{worker1:#?}");
        assert!(updates.holder("worker1").is_none(), "its re-read let it go");
        // The run's pass on worker1 goes at once.
        let pass = run_hive(
            &updates,
            &mesh,
            &id,
            "worker1",
            &the_target,
            &[],
            None,
            tokio::time::Instant::now() + Duration::from_secs(1),
            &bounds,
        );
        tokio::time::timeout(Duration::from_secs(2), pass)
            .await
            .expect("the pass waits for no read of another hive");
        mesh.answer.notify_one();
    };
    tokio::join!(slow, beside, run);
    assert_eq!(
        lock(&mesh.sent).clone(),
        vec![(
            "worker1".to_string(),
            json!({"category": "config", "manifest_version": 3, "manifest_hash": SHA_V3})
        )]
    );
    assert!(entry_of(&updates, "worker1").on_target());
}

/// A-219 (docs/edge-egress-nat-spec.md §15.1): an egress switch is not started while a task of the
/// update acts on that worker, since its core update restarts the worker's orchestrator; a task that
/// only reads the worker holds nothing back.
#[test]
fn an_egress_switch_waits_only_for_a_task_that_acts_on_its_worker() {
    let updates = Updates::load(&temp_state("installation-update-acting-on"));
    assert!(updates.acting_on("worker1").is_none());
    {
        let _read = updates.claim("worker1", READING, "upd-1").expect("claimed");
        assert!(updates.acting_on("worker1").is_none(), "a read");
    }
    let _core = updates
        .claim("worker1", "the automatic step", "upd-1")
        .expect("claimed");
    let holder = updates.acting_on("worker1").expect("acting on worker1");
    assert_eq!(
        (holder.what.as_str(), holder.update.as_str()),
        ("the automatic step", "upd-1")
    );
    assert!(updates.acting_on("worker2").is_none());
}
