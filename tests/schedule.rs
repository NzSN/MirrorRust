use mirrorrust::schedule::*;
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

fn identity() -> Identity {
    Identity {
        model_semantic_digest: "1".repeat(64),
        mapping_sha256: "2".repeat(64),
        implementation_sha256: "3".repeat(64),
    }
}
fn plan(order: &[&str]) -> Schedule {
    Schedule::new(
        identity(),
        json!({}),
        order
            .iter()
            .map(|s| {
                let (a, c) = s.split_once(':').unwrap();
                Step::new(a, c)
            })
            .collect(),
    )
}
fn serial() -> Schedule {
    plan(&[
        "a:read", "a:write", "a:$done", "b:read", "b:write", "b:$done",
    ])
}
fn overlap() -> Schedule {
    plan(&[
        "a:read", "b:read", "a:write", "b:write", "a:$done", "b:$done",
    ])
}
#[derive(Default)]
struct Stats {
    acquired: AtomicUsize,
    entered: AtomicUsize,
    teardown: AtomicUsize,
}
fn adapter(stats: Arc<Stats>, mode: &str) -> Adapter {
    let mode = mode.to_owned();
    Adapter {
        identity: identity(),
        actors: vec![
            ActorDeclaration {
                actor: "a".into(),
                operation: "increment-a".into(),
            },
            ActorDeclaration {
                actor: "b".into(),
                operation: "increment-b".into(),
            },
        ],
        checkpoints: vec!["read".into(), "write".into()],
        factory: Arc::new(move |_| {
            stats.acquired.fetch_add(1, Ordering::SeqCst);
            let state = Arc::new(AtomicUsize::new(0));
            let mut workers = vec![];
            for actor in ["a", "b"] {
                let state = state.clone();
                let stats = stats.clone();
                let mode = mode.clone();
                workers.push(Worker {
                    actor: actor.into(),
                    execute: Box::new(move |hook| {
                        stats.entered.fetch_add(1, Ordering::SeqCst);
                        if mode == "panic" {
                            panic!("deliberate worker panic");
                        }
                        if mode == "early" {
                            return Ok(());
                        }
                        if mode == "foreign" {
                            std::thread::scope(|scope| {
                                scope
                                    .spawn(|| hook.arrive("read"))
                                    .join()
                                    .unwrap()
                                    .unwrap_err();
                            });
                            return Ok(());
                        }
                        let saved = state.load(Ordering::SeqCst);
                        hook.arrive(if mode == "unexpected" {
                            "write"
                        } else if mode == "unknown" {
                            "other"
                        } else {
                            "read"
                        })?;
                        state.store(saved + 1, Ordering::SeqCst);
                        hook.arrive("write")?;
                        Ok(())
                    }),
                });
            }
            let mode_observe = mode.clone();
            let stats = stats.clone();
            let mode_drop = mode.clone();
            Ok(Program {
                workers,
                observe: Box::new(move || {
                    if mode_observe == "observer" {
                        return Err("observer failed".into());
                    }
                    if mode_observe == "large" {
                        return Ok(json!("x".repeat(65_536)));
                    }
                    Ok(json!({"count":state.load(Ordering::SeqCst)}))
                }),
                teardown: Box::new(move || {
                    stats.teardown.fetch_add(1, Ordering::SeqCst);
                    if mode_drop == "teardown" {
                        Err("teardown failed".into())
                    } else {
                        Ok(())
                    }
                }),
            })
        }),
    }
}
#[test]
fn actual_threads_retain_stack_across_interleavings() {
    for (p, expected) in [(serial(), 2), (overlap(), 1)] {
        let stats = Arc::new(Stats::default());
        let run = run_schedule(
            p.clone(),
            adapter(stats.clone(), "ok"),
            Policy::default(),
            Cancellation::default(),
        );
        assert!(run.report().passed());
        assert_eq!(
            run.report().observations.last().unwrap()["state"]["count"],
            expected
        );
        assert_eq!(stats.entered.load(Ordering::SeqCst), 2);
        assert_eq!(stats.teardown.load(Ordering::SeqCst), 1);
        assert_eq!(run.report().events.len(), 12);
        assert_eq!(run.report().observations.len(), 7);
        assert_ne!(
            run.receipt()["actorThreads"]["a"],
            run.receipt()["actorThreads"]["b"]
        );
        let again = run_schedule(
            p,
            adapter(Arc::new(Stats::default()), "ok"),
            Policy::default(),
            Cancellation::default(),
        );
        assert_eq!(run.report().events, again.report().events);
        assert_eq!(run.report().observations, again.report().observations);
        assert_ne!(run.report().execution_id, again.report().execution_id);
    }
}
#[test]
fn admission_is_before_factory() {
    let stats = Arc::new(Stats::default());
    let mut cases = vec![];
    let mut p = serial();
    p.identity.mapping_sha256 = "f".repeat(64);
    cases.push(p);
    let mut p = serial();
    p.steps[0].actor = "absent".into();
    cases.push(p);
    let mut p = serial();
    p.steps.pop();
    cases.push(p);
    let mut p = serial();
    p.steps.push(Step::new("a", "read"));
    cases.push(p);
    for p in cases {
        let run = start_schedule(
            p,
            adapter(stats.clone(), "ok"),
            Policy::default(),
            Cancellation::default(),
        );
        assert_ne!(run.report().outcome, Outcome::Completed);
        assert_eq!(run.report().cleanup, Cleanup::NotStarted);
    }
    assert_eq!(stats.acquired.load(Ordering::SeqCst), 0);
}
#[test]
fn strict_artifact_decoder_rejects_duplicate_and_unknown_fields() {
    let p = serial();
    let text = encode_schedule(&p).unwrap();
    assert_eq!(parse_schedule(&text).unwrap(), p);
    assert!(
        parse_schedule(&text.replacen("\"schema\":", "\"schema\":\"bad\",\"schema\":", 1)).is_err()
    );
    let mut j = serde_json::to_value(&p).unwrap();
    j["extra"] = json!(true);
    assert!(parse_schedule(&j.to_string()).is_err());
    let mut j = serde_json::to_value(&p).unwrap();
    j["profile"] = json!("mirrorcpp.cooperative-checkpoints/v1");
    assert!(parse_schedule(&j.to_string()).is_err());
    let mut j = serde_json::to_value(&p).unwrap();
    j["identity"]["mappingSha256"] = json!("a".repeat(63));
    assert!(parse_schedule(&j.to_string()).is_err());
    let text = text.replace("\"inputs\":{}", "\"inputs\":{\"x\":1,\"x\":2}");
    assert!(parse_schedule(&text).is_err());
}
#[test]
fn incremental_callbacks_release_only_selected_worker() {
    let stats = Arc::new(Stats::default());
    let mut run = start_schedule(
        serial(),
        adapter(stats.clone(), "ok"),
        Policy::default(),
        Cancellation::default(),
    );
    assert_eq!(stats.entered.load(Ordering::SeqCst), 0);
    assert_eq!(run.report().observations.len(), 1);
    run.advance(&Step::new("a", "read")).unwrap();
    assert_eq!(stats.entered.load(Ordering::SeqCst), 1);
    run.advance(&Step::new("b", "read")).unwrap();
    assert_eq!(run.report().outcome, Outcome::InvalidSchedule);
    assert_eq!(stats.entered.load(Ordering::SeqCst), 1);
    assert_eq!(run.report().cleanup, Cleanup::Confirmed);
    assert!(run.advance(&Step::new("a", "write")).is_err());
    assert_eq!(run.finish().unwrap().outcome, Outcome::InvalidSchedule);
}
#[test]
fn failures_preserve_primary_and_cleanup() {
    for (mode, expected) in [
        ("unexpected", Outcome::UnexpectedCheckpoint),
        ("unknown", Outcome::UnexpectedCheckpoint),
        ("early", Outcome::UnexpectedCheckpoint),
        ("panic", Outcome::ApplicationFailed),
        ("foreign", Outcome::UncontrolledActor),
        ("observer", Outcome::ObservationFailed),
        ("large", Outcome::ObservationFailed),
    ] {
        let stats = Arc::new(Stats::default());
        let run = run_schedule(
            serial(),
            adapter(stats.clone(), mode),
            Policy::default(),
            Cancellation::default(),
        );
        assert_eq!(run.report().outcome, expected, "{mode}");
        assert_eq!(run.report().cleanup, Cleanup::Confirmed, "{mode}");
        assert_eq!(stats.teardown.load(Ordering::SeqCst), 1);
        assert!(run.report().remaining_actors.is_empty());
    }
    let run = run_schedule(
        serial(),
        adapter(Arc::new(Stats::default()), "teardown"),
        Policy::default(),
        Cancellation::default(),
    );
    assert_eq!(run.report().outcome, Outcome::Completed);
    assert_eq!(run.report().cleanup, Cleanup::TeardownFailed);
    assert!(!run.report().passed());
}
#[test]
fn cancellation_before_and_between_intervals() {
    let stats = Arc::new(Stats::default());
    let cancel = Cancellation::default();
    cancel.cancel();
    let run = start_schedule(
        serial(),
        adapter(stats.clone(), "ok"),
        Policy::default(),
        cancel,
    );
    assert_eq!(run.report().outcome, Outcome::Cancelled);
    assert_eq!(stats.acquired.load(Ordering::SeqCst), 0);
    let cancel = Cancellation::default();
    let mut run = start_schedule(
        serial(),
        adapter(stats.clone(), "ok"),
        Policy::default(),
        cancel.clone(),
    );
    run.advance(&Step::new("a", "read")).unwrap();
    cancel.cancel();
    run.advance(&Step::new("a", "write")).unwrap();
    assert_eq!(run.report().outcome, Outcome::Cancelled);
    assert_eq!(run.report().cleanup, Cleanup::Confirmed);
}
#[test]
fn timeout_retains_thread_then_cleanup_can_retry() {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let disposed = Arc::new(AtomicBool::new(false));
    let worker_gate = gate.clone();
    let disposal = disposed.clone();
    let a = Adapter {
        identity: identity(),
        actors: vec![ActorDeclaration {
            actor: "a".into(),
            operation: "block".into(),
        }],
        checkpoints: vec!["read".into()],
        factory: Arc::new(move |_| {
            let gate = worker_gate.clone();
            let disposal = disposal.clone();
            Ok(Program {
                workers: vec![Worker {
                    actor: "a".into(),
                    execute: Box::new(move |hook| {
                        let (mut ready, cv) = (gate.0.lock().unwrap(), &gate.1);
                        while !*ready {
                            ready = cv.wait(ready).unwrap();
                        }
                        drop(ready);
                        hook.arrive("read")
                    }),
                }],
                observe: Box::new(|| Ok(json!({}))),
                teardown: Box::new(move || {
                    disposal.store(true, Ordering::SeqCst);
                    Ok(())
                }),
            })
        }),
    };
    let p = plan(&["a:read", "a:$done"]);
    let mut run = start_schedule(
        p,
        a,
        Policy {
            execution_timeout: Duration::from_millis(50),
            cleanup_timeout: Duration::from_millis(5),
            ..Policy::default()
        },
        Cancellation::default(),
    );
    run.advance(&Step::new("a", "read")).unwrap();
    assert_eq!(run.report().outcome, Outcome::TimedOut);
    assert_eq!(run.report().cleanup, Cleanup::Incomplete);
    assert_eq!(run.report().remaining_actors, vec!["a"]);
    assert!(!disposed.load(Ordering::SeqCst));
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    run.cleanup(Duration::from_secs(1)).unwrap();
    assert_eq!(run.report().outcome, Outcome::TimedOut);
    assert_eq!(run.report().cleanup, Cleanup::Confirmed);
    assert!(disposed.load(Ordering::SeqCst));
    assert_eq!(run.report().cleanup_attempts.len(), 2);
}
#[test]
fn factory_reentry_rejects_nested_acquisition() {
    let count = Arc::new(Stats::default());
    let nested = count.clone();
    let mut outer = adapter(Arc::new(Stats::default()), "ok");
    let factory = outer.factory.clone();
    outer.factory = Arc::new(move |inputs| {
        let child = start_schedule(
            serial(),
            adapter(nested.clone(), "ok"),
            Policy::default(),
            Cancellation::default(),
        );
        assert_eq!(child.report().outcome, Outcome::InvalidSchedule);
        factory(inputs)
    });
    let run = run_schedule(serial(), outer, Policy::default(), Cancellation::default());
    assert!(run.report().passed());
    assert_eq!(count.acquired.load(Ordering::SeqCst), 0);
}
#[test]
fn finished_observation_includes_thread_local_destructor() {
    struct OnExit(Arc<AtomicUsize>);
    impl Drop for OnExit {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    thread_local! {static EXIT:std::cell::RefCell<Option<OnExit>>=const{std::cell::RefCell::new(None)};}
    let value = Arc::new(AtomicUsize::new(0));
    let a = Adapter {
        identity: identity(),
        actors: vec![ActorDeclaration {
            actor: "a".into(),
            operation: "tls".into(),
        }],
        checkpoints: vec![],
        factory: Arc::new(move |_| {
            let worker = value.clone();
            let observe = value.clone();
            Ok(Program {
                workers: vec![Worker {
                    actor: "a".into(),
                    execute: Box::new(move |_| {
                        EXIT.with(|v| *v.borrow_mut() = Some(OnExit(worker.clone())));
                        worker.store(1, Ordering::SeqCst);
                        Ok(())
                    }),
                }],
                observe: Box::new(move || Ok(json!({"count":observe.load(Ordering::SeqCst)}))),
                teardown: Box::new(|| Ok(())),
            })
        }),
    };
    let run = run_schedule(
        plan(&["a:$done"]),
        a,
        Policy::default(),
        Cancellation::default(),
    );
    assert!(run.report().passed());
    assert_eq!(
        run.report().observations.last().unwrap()["state"]["count"],
        2
    );
}

#[test]
fn cancellation_during_final_observation_cannot_pass() {
    let cancel = Cancellation::default();
    let request = cancel.clone();
    let mut a = adapter(Arc::new(Stats::default()), "ok");
    let factory = a.factory.clone();
    a.factory = Arc::new(move |input| {
        let mut p = factory(input)?;
        let mut observe = p.observe;
        let request = request.clone();
        let mut observations = 0;
        p.observe = Box::new(move || {
            observations += 1;
            let value = observe()?;
            if observations == 7 {
                request.cancel();
            }
            Ok(value)
        });
        Ok(p)
    });
    let run = run_schedule(serial(), a, Policy::default(), cancel);
    assert_eq!(run.report().outcome, Outcome::Cancelled);
    assert!(!run.report().passed());
    assert_eq!(run.report().cleanup, Cleanup::Confirmed);
}
