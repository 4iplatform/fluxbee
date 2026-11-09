//! The installation's update (docs/hive-config-update-v1.md §6, "As built (stage 4)"): the decision
//! per hive and category as a table, the state on disk, the explicit run and the background task
//! against a fake mesh, and the size of the answers.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

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

/// The decision per hive and category, as a table, for both kinds of run (§6 and the brief's
/// exclusions).
#[test]
fn the_decision_per_hive_and_category() {
    let explicit = RunKind::Explicit;
    let automatic = RunKind::Automatic;

    // On the target: nothing to do, whoever runs.
    for kind in [explicit, automatic] {
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
    // the hive, and still applies its config; the automatic step leaves it alone, both categories.
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
    assert_eq!(decision.config, Step::Act);
    if let Step::Excluded { next, .. } = &decision.core {
        assert!(next.contains("POST /hives/egress1/update"), "{next}");
        assert!(next.contains("include_rolled_back"), "{next}");
    }
    assert_eq!(
        decide_for(&rolled_back, explicit, &["egress1"]).core,
        Step::Act
    );
    let decision = decide_for(&rolled_back, automatic, &["egress1"]);
    assert!(is_excluded(
        &decision.core,
        "automatic step leaves it alone"
    ));
    assert!(is_excluded(
        &decision.config,
        "automatic step leaves it alone"
    ));
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
}

/// One update at a time: the same target resumes it, another closes it as superseded, kept in the
/// history, which keeps the last 20, newest first; an update is done once every hive is on it.
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
    assert_eq!(file.close_if_done(4_000), None);
    for entry in file.open.as_mut().expect("open").hives.values_mut() {
        entry.core.state = Standing::OnTarget;
        entry.config.state = Standing::OnTarget;
    }
    assert_eq!(file.close_if_done(4_000), Some(second.clone()));
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

/// The state is on disk, written atomically: SY.admin's start resumes the open update (a config
/// apply left in flight unknown again); a file that does not parse is moved aside.
#[test]
fn the_state_survives_a_restart() {
    let state_dir = temp_state("installation-update-restart");
    let updates = Updates::load(&state_dir);
    let (id, _) = updates.change(|file| {
        let opened = file.open_or_resume(&target(), "http", Some("deploy 0.1.123"), now_epoch_ms());
        file.refresh_hives(
            &opened.0,
            &["motherbee".to_string(), "worker1".to_string()],
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
    assert!(updates.record(
        &id,
        "worker1",
        Category::Config,
        Standing::InFlight,
        Some("its config update is being applied".to_string()),
        None,
        None
    ));
    // Its core update answered restarting: followed after the restart.
    let mut outcome = Outcome::new(OutcomeStatus::Restarting, RunKind::Explicit, 10, 20);
    outcome.previous_update = Some("100".to_string());
    outcome.answered_ms = Some(5);
    assert!(updates.record(
        &id,
        "worker1",
        Category::Core,
        Standing::InFlight,
        None,
        Some(outcome),
        None
    ));
    // egress1's core update was being sent: no answer recorded.
    updates.change(|file| {
        file.refresh_hives(
            &id,
            &[
                "motherbee".to_string(),
                "worker1".to_string(),
                "egress1".to_string(),
            ],
            Vec::new(),
        )
    });
    assert!(updates.record(
        &id,
        "egress1",
        Category::Core,
        Standing::InFlight,
        None,
        None,
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
    assert_eq!(look_at(worker), Look::Follow);
    // What a previous process sent with no answer recorded: not_answered, for the automatic step.
    assert_eq!(worker.config.state, Standing::Unknown);
    assert_eq!(
        worker.config.outcome.as_ref().map(|o| o.status),
        Some(OutcomeStatus::NotAnswered)
    );
    let egress = &open.hives["egress1"];
    assert_eq!(egress.core.state, Standing::Unknown);
    assert_eq!(
        egress.core.outcome.as_ref().map(|o| o.status),
        Some(OutcomeStatus::NotAnswered)
    );
    assert_eq!(look_at(egress), Look::Automatic);
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
/// reads as this update's.
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
            open: vec![]
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
            open: vec![]
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
    // The same record as before the update: the update has not written its own yet.
    assert_eq!(
        watch_core(&rolled_back, H_NEW, Some("500")),
        CoreWatch::Waiting {
            installed: false,
            open: vec![]
        }
    );
    // On the target, something left open.
    let open = on_target_with(core(
        Some(("update", H_NEW)),
        H_NEW,
        json!({"phase": "done", "status": "ok", "started_at_ms": "500", "left_out": ["sy-old"],
               "updated": ["sy-orchestrator"], "settled": {"settled": false, "pending": ["sy-new"], "errors": []}}),
        Value::Null,
    ));
    assert!(matches!(
        watch_core(&open, H_NEW, None),
        CoreWatch::Waiting { installed: true, open } if open.len() == 2
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
        })
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
}

#[async_trait]
impl UpdateMesh for FakeMesh {
    async fn versions(&self, hive: &str, _timeout: Duration) -> Result<Value, String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
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
        if lock(&self.behaves).get(hive).copied() == Some(Behaves::Refuses) {
            return Ok(
                json!({"status": "error", "error_code": "RESTART_FAILED", "message": "sy-cognition did not come back"}),
            );
        }
        let mut snapshots = lock(&self.snapshots);
        let snapshot = snapshots.get_mut(hive).ok_or("no such hive")?;
        match payload["category"].as_str() {
            Some("core") => {
                snapshot["core"]["last_update"] = json!({"phase": "restarting", "started_at_ms": "900", "updated": ["sy-orchestrator"]});
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

    fn hives(&self) -> (Vec<String>, Vec<Value>) {
        (self.hives.clone(), Vec::new())
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
        read: Duration::from_millis(500),
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
    // its config goes.
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
    let state = updates.read(|file| file.clone());
    let done = state
        .closed
        .first()
        .cloned()
        .unwrap_or_else(|| state.open.clone().expect("open"));
    let worker = &done.hives["worker1"];
    assert_eq!(worker.core.state, Standing::OnTarget);
    let core_outcome = worker.core.outcome.as_ref().expect("core");
    assert_eq!(core_outcome.status, OutcomeStatus::Settled);
    assert!(core_outcome.installed_ms.is_some() && core_outcome.settled_ms.is_some());
    assert_eq!(worker.config.state, Standing::OnTarget);
    assert_eq!(
        worker
            .config
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.restarted.clone()),
        Some(vec!["sy-cognition".to_string()])
    );
    updates.close_if_done();
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
        assert!(category
            .next
            .as_deref()
            .is_some_and(|next| next.contains("automatic step")));
    }
    assert_eq!(look_at(egress), Look::Automatic);

    // Still down: the cheap read of shared memory says so, and nothing is asked.
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
    let state = updates.read(|file| file.clone());
    let record = state
        .open
        .clone()
        .unwrap_or_else(|| state.closed[0].clone());
    let egress = &record.hives["egress1"];
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
    updates.close_if_done();
    assert_eq!(updates.state_of(&update.id), Some(UpdateState::Done));
}

/// Once per hive and target: an automatic attempt that fails is left for the operator, and never
/// tried again; and the automatic step never acts on an excluded hive.
#[tokio::test]
async fn the_automatic_step_is_once_and_never_for_an_excluded_hive() {
    let updates = Updates::load(&temp_state("installation-update-once"));
    let rolled_back = snapshot(
        "worker1",
        "worker",
        core(
            Some(("core_rollback", H_OLD)),
            H_NEW,
            settled_update(),
            Value::Null,
        ),
        config("current", 3, SHA_V3, Some(Some((2, SHA_V2)))),
    );
    let mesh = FakeMesh::new(vec![
        motherbee_on_target(),
        egress_behind_config(),
        rolled_back,
    ]);
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
    assert_eq!(look_at(&egress), Look::Recheck);
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
    // The update stays open: a rolled-back hive is not on the target.
    assert_eq!(updates.state_of(&id), Some(UpdateState::Open));

    // An explicit run that names it moves its core.
    let include = vec!["worker1".to_string()];
    explicit_run(&updates, &mesh, &include).await;
    assert_eq!(
        mesh.sent_to("worker1").first(),
        Some(&json!({"category": "core", "manifest_version": 0, "manifest_hash": H_NEW}))
    );
}

/// The background task's tick: it refreshes the hives, spawns what each needs, and the update
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
        for entry in file.open.as_mut().expect("open").hives.values_mut() {
            for category in [Category::Core, Category::Config] {
                let mut outcome = Outcome::new(OutcomeStatus::Error, RunKind::Automatic, 1, 2);
                outcome.error_code = Some("CORE_UPDATE_ROLLED_BACK".to_string());
                outcome.detail = Some(cut(&long));
                outcome.restarted = Some(vec![
                    "sy-cognition".to_string(),
                    "sy-frontdesk-gov".to_string(),
                ]);
                outcome.previous_update = Some("1731160000000".to_string());
                *entry.category_mut(category) = CategoryEntry {
                    state: Standing::Pending,
                    reason: Some(cut(&long)),
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
    let updates = Updates {
        path: temp_state("installation-update-size").join("installation-update.json"),
        file: Mutex::new(file),
        busy: Mutex::new(HashSet::new()),
        due: Mutex::new(HashMap::new()),
        wake: tokio::sync::Notify::new(),
    };
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
