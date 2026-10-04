use mirrorrust::schedule::*;
use mirrorrust::schedule_binding::BindingSession;
use mirrorrust::schedule_exploration::*;
use mirrorrust::{State, Value};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
fn identity() -> Identity {
    Identity {
        model_semantic_digest: "1".repeat(64),
        mapping_sha256: "2".repeat(64),
        implementation_sha256: "3".repeat(64),
    }
}
fn schedule() -> Schedule {
    Schedule::new(
        identity(),
        json!({}),
        ["read", "write", "$done"]
            .iter()
            .map(|c| Step::new("a", c))
            .collect(),
    )
}
fn adapter(
    acquired: Arc<AtomicUsize>,
    disposed: Arc<AtomicUsize>,
    fail_cleanup: bool,
    gate: Option<Arc<(Mutex<bool>, Condvar)>>,
) -> Adapter {
    Adapter {
        identity: identity(),
        actors: vec![ActorDeclaration {
            actor: "a".into(),
            operation: "increment".into(),
        }],
        checkpoints: vec!["read".into(), "write".into()],
        factory: Arc::new(move |_| {
            acquired.fetch_add(1, Ordering::SeqCst);
            let value = Arc::new(AtomicUsize::new(0));
            let worker = value.clone();
            let gate = gate.clone();
            let disposed = disposed.clone();
            Ok(Program {
                workers: vec![Worker {
                    actor: "a".into(),
                    execute: Box::new(move |hook| {
                        if let Some(gate) = gate {
                            let mut ready = gate.0.lock().unwrap();
                            while !*ready {
                                ready = gate.1.wait(ready).unwrap();
                            }
                        }
                        worker.store(1, Ordering::SeqCst);
                        hook.arrive("read")?;
                        worker.store(2, Ordering::SeqCst);
                        hook.arrive("write")?;
                        Ok(())
                    }),
                }],
                observe: Box::new(move || Ok(json!({"count":value.load(Ordering::SeqCst)}))),
                teardown: Box::new(move || {
                    disposed.fetch_add(1, Ordering::SeqCst);
                    if fail_cleanup {
                        Err("teardown failure".into())
                    } else {
                        Ok(())
                    }
                }),
            })
        }),
    }
}
fn space() -> FiniteSpace {
    FiniteSpace {
        identity: identity(),
        actors: vec![ActorChain {
            actor: ActorDeclaration {
                actor: "a".into(),
                operation: "increment".into(),
            },
            checkpoints: vec!["read".into(), "write".into(), "$done".into()],
        }],
        inputs: vec![json!({}), json!({"second":true})],
        max_preemptions: 0,
        require_model_comparison: false,
        base_variables: vec!["count".into()],
        instrumentation_variables: vec![],
    }
}
#[test]
fn binding_generations_are_fresh_and_disposal_is_once() {
    let (a, d) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let mut session = BindingSession::new(
        schedule(),
        adapter(a.clone(), d.clone(), false, None),
        Policy::default(),
        Cancellation::default(),
    );
    assert_eq!(a.load(Ordering::SeqCst), 0);
    assert!(session.observation().is_err());
    for _ in 0..2 {
        session.initialize().unwrap();
        assert_eq!(session.observation().unwrap()["count"], 0);
        for step in schedule().steps {
            session.advance(&step).unwrap();
        }
        assert_eq!(session.observation().unwrap()["count"], 2);
    }
    session.dispose().unwrap();
    session.dispose().unwrap();
    let r = session.receipt();
    assert_eq!(r["initializations"], 2);
    assert_eq!(r["disposals"], 1);
    assert_ne!(
        r["executions"][0]["executionId"],
        r["executions"][1]["executionId"]
    );
    assert!(session.fully_completed());
    assert_eq!(a.load(Ordering::SeqCst), 2);
    assert_eq!(d.load(Ordering::SeqCst), 2);
    assert!(session.initialize().is_err());
}
#[test]
fn incomplete_previous_binding_execution_blocks_acquisition() {
    let (a, d) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let mut session = BindingSession::new(
        schedule(),
        adapter(a.clone(), d.clone(), false, None),
        Policy::default(),
        Cancellation::default(),
    );
    session.initialize().unwrap();
    session.advance(&Step::new("a", "read")).unwrap();
    assert_eq!(
        session.initialize().unwrap_err().code,
        "schedule_previous_incomplete"
    );
    session.dispose().unwrap();
    assert_eq!(a.load(Ordering::SeqCst), 1);
    assert_eq!(d.load(Ordering::SeqCst), 1);
    assert!(!session.fully_completed());
}
#[test]
fn binding_disposal_failure_is_stable() {
    let d = Arc::new(AtomicUsize::new(0));
    let mut session = BindingSession::new(
        schedule(),
        adapter(Arc::new(AtomicUsize::new(0)), d.clone(), true, None),
        Policy::default(),
        Cancellation::default(),
    );
    session.initialize().unwrap();
    for step in schedule().steps {
        session.advance(&step).unwrap();
    }
    let first = session.dispose().unwrap_err();
    assert_eq!(session.dispose().unwrap_err(), first);
    assert_eq!(d.load(Ordering::SeqCst), 1);
    assert_eq!(
        session.receipt()["executions"][0]["cleanup"],
        "teardown_failed"
    );
    assert!(!session.fully_completed());
}
#[test]
fn retry_updates_retained_cleanup_without_converting_failed_replay() {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let d = Arc::new(AtomicUsize::new(0));
    let mut session = BindingSession::new(
        schedule(),
        adapter(
            Arc::new(AtomicUsize::new(0)),
            d.clone(),
            false,
            Some(gate.clone()),
        ),
        Policy {
            execution_timeout: Duration::from_millis(100),
            cleanup_timeout: Duration::from_millis(5),
            ..Policy::default()
        },
        Cancellation::default(),
    );
    session.initialize().unwrap();
    assert!(session.advance(&Step::new("a", "read")).is_err());
    assert!(session.dispose().is_err());
    assert_eq!(session.receipt()["executions"][0]["cleanup"], "incomplete");
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    std::thread::sleep(Duration::from_millis(10));
    session.retry_cleanup().unwrap();
    assert_eq!(session.receipt()["executions"][0]["cleanup"], "confirmed");
    assert_eq!(session.receipt()["executions"][0]["outcome"], "timed_out");
    assert!(!session.fully_completed());
    assert_eq!(d.load(Ordering::SeqCst), 1);
}
#[test]
fn invalid_exploration_declarations_never_acquire() {
    for which in 0..4 {
        let a = Arc::new(AtomicUsize::new(0));
        let mut runner = local_exploration_runner(
            adapter(a.clone(), Arc::new(AtomicUsize::new(0)), false, None),
            Policy::default(),
        );
        let mut s = space();
        match which {
            0 => {
                s.actors[0].checkpoints.pop();
            }
            1 => s.inputs.push(json!({})),
            2 => s.base_variables.push("count".into()),
            _ => s.instrumentation_variables.push("count".into()),
        };
        let r = explore_finite(
            &s,
            &mut runner,
            &ExplorationLimits::default(),
            &Cancellation::default(),
        );
        assert_eq!(r["status"], "invalid_declaration");
        assert_eq!(a.load(Ordering::SeqCst), 0);
    }
}
#[test]
fn exploration_checks_schedule_freshness_events_and_projection() {
    for mode in 0..4 {
        let a = Arc::new(AtomicUsize::new(0));
        let mut base = local_exploration_runner(
            adapter(a.clone(), Arc::new(AtomicUsize::new(0)), false, None),
            Policy::default(),
        );
        let mut first = None;
        let mut runner: ExplorationRunner = Box::new(move |p, c| {
            let mut sample = base(p, c)?;
            match mode {
                0 => {
                    sample.execution["schedule"]["identity"]["mappingSha256"] =
                        json!("f".repeat(64))
                }
                1 => {
                    let id = first.get_or_insert_with(|| sample.execution["executionId"].clone());
                    sample.execution["executionId"] = id.clone();
                }
                2 => {
                    sample.execution["events"].as_array_mut().unwrap().pop();
                }
                _ => sample.execution["observations"][0]["state"]["extra"] = json!(1),
            }
            Ok(sample)
        });
        let r = explore_finite(
            &space(),
            &mut runner,
            &ExplorationLimits::default(),
            &Cancellation::default(),
        );
        assert_eq!(r["complete"], false);
        assert!(
            r["categories"]["runner_or_evidence_failed"]
                .as_u64()
                .unwrap()
                > 0
        );
        if mode == 0 {
            assert_eq!(a.load(Ordering::SeqCst), 1);
        }
    }
}
#[test]
fn unknown_cleanup_halts_and_late_runner_never_earns_complete() {
    let a = Arc::new(AtomicUsize::new(0));
    let mut base = local_exploration_runner(
        adapter(a.clone(), Arc::new(AtomicUsize::new(0)), false, None),
        Policy::default(),
    );
    let mut runner: ExplorationRunner = Box::new(move |p, c| {
        let mut sample = base(p, c)?;
        sample.execution["cleanup"] = json!("incomplete");
        sample.execution["remainingActors"] = json!(["a"]);
        Ok(sample)
    });
    let r = explore_finite(
        &space(),
        &mut runner,
        &ExplorationLimits::default(),
        &Cancellation::default(),
    );
    assert_eq!(r["stopReason"], "unconfirmed_cleanup");
    assert_eq!(a.load(Ordering::SeqCst), 1);
    let mut base = local_exploration_runner(
        adapter(
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            false,
            None,
        ),
        Policy::default(),
    );
    let mut runner: ExplorationRunner = Box::new(move |p, c| {
        std::thread::sleep(Duration::from_millis(15));
        base(p, c)
    });
    let mut s = space();
    s.inputs.truncate(1);
    let r = explore_finite(
        &s,
        &mut runner,
        &ExplorationLimits {
            time_budget: Duration::from_millis(10),
            ..ExplorationLimits::default()
        },
        &Cancellation::default(),
    );
    assert_eq!(r["complete"], false);
    assert_eq!(r["stopReason"], "time_budget");
}
#[test]
fn model_counterexample_survives_separate_cleanup_failure() {
    let a = Arc::new(AtomicUsize::new(0));
    let mut base = local_exploration_runner(
        adapter(a.clone(), Arc::new(AtomicUsize::new(0)), false, None),
        Policy::default(),
    );
    let mut runner: ExplorationRunner = Box::new(move |p, c| {
        let mut sample = base(p, c)?;
        sample.execution["cleanup"] = json!("teardown_failed");
        sample.execution["passed"] = json!(false);
        sample.comparison = Some(
            json!({"schema":"mirrors.scheduled-comparison/v1","comparison":"step_mismatch","peerTerminal":"step_mismatch","passed":false,"client":{"kind":"step_mismatch"},"binding":{"executions":[sample.execution]}}),
        );
        Ok(sample)
    });
    let mut s = space();
    s.require_model_comparison = true;
    let r = explore_finite(
        &s,
        &mut runner,
        &ExplorationLimits::default(),
        &Cancellation::default(),
    );
    assert_eq!(r["firstCounterexample"]["category"], "model_mismatch");
    assert_eq!(r["firstFailure"]["category"], "cleanup_failed");
    assert_eq!(r["stopReason"], "unconfirmed_cleanup");
    assert_eq!(r["complete"], false);
    assert_eq!(a.load(Ordering::SeqCst), 1);
}
#[test]
fn canonical_coverage_preserves_types_and_ordinary_keys() {
    let n = |v: i64| Value::Int(v.into());
    let text = |v: &str| Value::Str(v.into());
    let state = |v| State::from([("v".into(), v)]);
    assert_eq!(
        canonical_state(&state(Value::Set(vec![n(2), n(1), n(2)]))).unwrap(),
        canonical_state(&state(Value::Set(vec![n(1), n(2)]))).unwrap()
    );
    assert_eq!(
        canonical_state(&state(Value::Map(vec![
            (text("b"), n(2)),
            (text("a"), n(1))
        ])))
        .unwrap(),
        canonical_state(&state(Value::Map(vec![
            (text("a"), n(1)),
            (text("b"), n(2))
        ])))
        .unwrap()
    );
    assert_ne!(
        canonical_state(&state(Value::Map(vec![(n(1), n(1))]))).unwrap(),
        canonical_state(&state(Value::Map(vec![(text("1"), n(1))]))).unwrap()
    );
    assert!(canonical_state(&state(Value::Map(vec![(n(1), n(1)), (n(1), n(2))]))).is_err());
    assert!(canonical_state(&state(Value::Map(vec![(n(1), n(1)), (text("1"), n(2))]))).is_err());
    let fields = BTreeMap::from([("__proto__".into(), n(7)), ("tag".into(), text("ordinary"))]);
    assert!(canonical_state(&state(Value::Record(fields)))
        .unwrap()
        .contains("__proto__"));
    assert_ne!(
        canonical_state(&state(Value::Seq(vec![n(1), n(2)]))).unwrap(),
        canonical_state(&state(Value::Seq(vec![n(2), n(1)]))).unwrap()
    );
}
