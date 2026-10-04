//! Cooperative, owned OS-thread scheduling at application-declared checkpoints.
//! The coordinator controls intervals, not instructions or external effects.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const PROFILE: &str = "mirrorrust.cooperative-checkpoints/v1";
pub const SCHEMA: &str = "mirrors.checkpoint-schedule/v1";
pub const COMPLETION: &str = "$done";
static GENERATION: AtomicU64 = AtomicU64::new(1);
thread_local! { static IN_CALLBACK: Cell<bool> = const { Cell::new(false) }; }
struct Callback(bool);
impl Callback {
    fn enter() -> Self {
        Self(IN_CALLBACK.with(|v| v.replace(true)))
    }
}
impl Drop for Callback {
    fn drop(&mut self) {
        IN_CALLBACK.with(|v| v.set(self.0));
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Identity {
    pub model_semantic_digest: String,
    pub mapping_sha256: String,
    pub implementation_sha256: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub actor: String,
    pub checkpoint: String,
}
impl Step {
    pub fn new(actor: &str, checkpoint: &str) -> Self {
        Self {
            actor: actor.into(),
            checkpoint: checkpoint.into(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    pub schema: String,
    pub profile: String,
    pub identity: Identity,
    pub inputs: Json,
    pub steps: Vec<Step>,
}
impl Schedule {
    pub fn new(identity: Identity, inputs: Json, steps: Vec<Step>) -> Self {
        Self {
            schema: SCHEMA.into(),
            profile: PROFILE.into(),
            identity,
            inputs,
            steps,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActorDeclaration {
    pub actor: String,
    pub operation: String,
}
#[derive(Clone, Debug)]
pub struct Policy {
    pub max_actors: usize,
    pub max_steps: usize,
    pub execution_timeout: Duration,
    pub cleanup_timeout: Duration,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            max_actors: 64,
            max_steps: 65_536,
            execution_timeout: Duration::from_secs(2),
            cleanup_timeout: Duration::from_secs(1),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Completed,
    InvalidSchedule,
    IncompatibleIdentity,
    Cancelled,
    TimedOut,
    UnexpectedCheckpoint,
    UncontrolledActor,
    ApplicationFailed,
    ObservationFailed,
    ResourceFailed,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Cleanup {
    NotStarted,
    Confirmed,
    Incomplete,
    TeardownFailed,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub outcome: Outcome,
    pub detail: String,
    pub schedule_completed: bool,
    pub cleanup: Cleanup,
    pub remaining_actors: Vec<String>,
    pub generation: u64,
    pub execution_id: String,
    pub events: Vec<Json>,
    pub observations: Vec<Json>,
    pub cleanup_attempts: Vec<Json>,
}
impl Report {
    pub fn passed(&self) -> bool {
        self.outcome == Outcome::Completed
            && self.schedule_completed
            && self.cleanup == Cleanup::Confirmed
    }
}
#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}
pub type WorkerFunction = Box<dyn FnOnce(&Checkpoint) -> Result<(), String> + Send + 'static>;
pub struct Worker {
    pub actor: String,
    pub execute: WorkerFunction,
}
pub struct Program {
    pub workers: Vec<Worker>,
    pub observe: Box<dyn FnMut() -> Result<Json, String> + Send>,
    pub teardown: Box<dyn FnMut() -> Result<(), String> + Send>,
}
pub type ProgramFactory = Arc<dyn Fn(&Json) -> Result<Program, String> + Send + Sync>;
#[derive(Clone)]
pub struct Adapter {
    pub identity: Identity,
    pub actors: Vec<ActorDeclaration>,
    pub checkpoints: Vec<String>,
    pub factory: ProgramFactory,
}

pub fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-./".contains(&c))
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn identity_valid(i: &Identity) -> bool {
    digest(&i.model_semantic_digest)
        && digest(&i.mapping_sha256)
        && digest(&i.implementation_sha256)
}
fn budget(d: Duration) -> bool {
    !d.is_zero() && d <= Duration::from_secs(86_400)
}
pub fn parse_schedule(text: &str) -> Result<Schedule, String> {
    let value = crate::json::parse(
        text,
        crate::json::Limits {
            max_bytes: 8 * 1_048_576,
            max_depth: 32,
            max_nodes: 600_000,
            reject_duplicate_keys: true,
        },
    )?;
    let schedule: Schedule = serde_json::from_value(value).map_err(|e| e.to_string())?;
    if schedule.schema != SCHEMA
        || schedule.profile != PROFILE
        || !identity_valid(&schedule.identity)
        || schedule.steps.len() > 65_536
        || schedule.steps.iter().any(|s| {
            !identifier(&s.actor) || (s.checkpoint != COMPLETION && !identifier(&s.checkpoint))
        })
    {
        return Err("invalid schedule schema, profile, identity or steps".into());
    }
    if serde_json::to_vec(&schedule.inputs)
        .map_err(|e| e.to_string())?
        .len()
        > 65_535
    {
        return Err("inputs exceed bound".into());
    }
    Ok(schedule)
}
pub fn encode_schedule(schedule: &Schedule) -> Result<String, String> {
    let text = serde_json::to_string(schedule).map_err(|e| e.to_string())?;
    parse_schedule(&text)?;
    Ok(text)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Starting,
    Parked,
    Running,
    Finished,
}
struct Actor {
    declaration: ActorDeclaration,
    phase: Phase,
    checkpoint: String,
    thread: Option<ThreadId>,
}
struct State {
    actors: Vec<Actor>,
    stopping: bool,
    report: Report,
    evidence_bytes: usize,
}
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    checkpoints: BTreeSet<String>,
}
fn lock(shared: &Shared) -> MutexGuard<'_, State> {
    shared.state.lock().unwrap_or_else(|e| e.into_inner())
}
fn fail(state: &mut State, outcome: Outcome, detail: impl Into<String>) {
    if state.report.outcome == Outcome::Completed {
        state.report.outcome = outcome;
        state.report.detail = detail.into();
    }
    state.stopping = true;
}
fn retain_event(state: &mut State, kind: &str, step: usize, actor: usize) {
    let a = &state.actors[actor];
    let row = json!({"ordinal":state.report.events.len(),"kind":kind,"step":step,"actor":a.declaration.actor,"operation":a.declaration.operation,"checkpoint":a.checkpoint});
    state.evidence_bytes += row.to_string().len();
    if state.evidence_bytes > 8 * 1_048_576 {
        fail(
            state,
            Outcome::ResourceFailed,
            "execution evidence exceeds bound",
        );
    } else {
        state.report.events.push(row);
    }
}
pub struct Checkpoint {
    shared: Arc<Shared>,
    actor: usize,
}
impl Checkpoint {
    pub fn stop_requested(&self) -> bool {
        lock(&self.shared).stopping
    }
    pub fn arrive(&self, checkpoint: &str) -> Result<(), String> {
        let mut s = lock(&self.shared);
        if s.actors[self.actor].thread != Some(thread::current().id())
            || s.actors[self.actor].phase != Phase::Running
        {
            fail(
                &mut s,
                Outcome::UncontrolledActor,
                "checkpoint called outside owned running actor",
            );
            self.shared.changed.notify_all();
            return Err("uncontrolled actor".into());
        }
        if s.stopping {
            return Err("execution cancelled".into());
        }
        if !identifier(checkpoint) || !self.shared.checkpoints.contains(checkpoint) {
            fail(
                &mut s,
                Outcome::UnexpectedCheckpoint,
                "undeclared checkpoint",
            );
            self.shared.changed.notify_all();
            return Err("unexpected checkpoint".into());
        }
        s.actors[self.actor].checkpoint = checkpoint.into();
        s.actors[self.actor].phase = Phase::Parked;
        self.shared.changed.notify_all();
        while !s.stopping && s.actors[self.actor].phase != Phase::Running {
            s = self
                .shared
                .changed
                .wait(s)
                .unwrap_or_else(|e| e.into_inner());
        }
        if s.stopping {
            Err("execution cancelled".into())
        } else {
            Ok(())
        }
    }
}

pub struct Execution {
    schedule: Schedule,
    declarations: Vec<ActorDeclaration>,
    shared: Arc<Shared>,
    program: Option<Program>,
    handles: Vec<Option<JoinHandle<()>>>,
    policy: Policy,
    cancellation: Cancellation,
    deadline: Instant,
    owner: ThreadId,
    next: usize,
    closed: bool,
}
impl Execution {
    pub fn report(&self) -> Report {
        lock(&self.shared).report.clone()
    }
    pub fn receipt(&self) -> Json {
        let r = self.report();
        let mut value = serde_json::to_value(&r).expect("report is JSON");
        value["schema"] = json!("mirrors.checkpoint-execution/v1");
        value["schedule"] = json!(self.schedule);
        value["actors"] = json!(self.declarations);
        value["checkpoints"] = json!(self.shared.checkpoints);
        value["policy"] = json!({"maxActors":self.policy.max_actors,"maxSteps":self.policy.max_steps,"executionTimeoutMs":self.policy.execution_timeout.as_millis(),"cleanupTimeoutMs":self.policy.cleanup_timeout.as_millis()});
        value["coverage"] = json!("recorded-schedule-only");
        value["passed"] = json!(r.passed());
        value["actorThreads"] = json!(lock(&self.shared)
            .actors
            .iter()
            .map(|a| (
                a.declaration.actor.clone(),
                a.thread.map(|t| format!("{t:?}"))
            ))
            .collect::<BTreeMap<_, _>>());
        value
    }
    fn check_owner(&self) -> Result<(), String> {
        if self.owner != thread::current().id() || IN_CALLBACK.with(Cell::get) {
            Err("scheduler reentry or wrong controller thread".into())
        } else {
            Ok(())
        }
    }
    fn wait_for(&self, deadline: Instant, predicate: impl Fn(&State) -> bool) -> bool {
        let mut s = lock(&self.shared);
        loop {
            if s.stopping {
                return false;
            }
            if self.cancellation.is_cancelled() {
                fail(&mut s, Outcome::Cancelled, "caller cancellation");
                self.shared.changed.notify_all();
                return false;
            }
            if Instant::now() >= deadline {
                fail(&mut s, Outcome::TimedOut, "execution deadline exceeded");
                self.shared.changed.notify_all();
                return false;
            }
            if predicate(&s) {
                return true;
            }
            let wait = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(10));
            s = self
                .shared
                .changed
                .wait_timeout(s, wait)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
    fn observe(&mut self) {
        let mut s = lock(&self.shared);
        if s.stopping {
            return;
        }
        if s.actors
            .iter()
            .any(|a| a.phase == Phase::Running || a.phase == Phase::Starting)
        {
            fail(&mut s, Outcome::ObservationFailed, "actors not quiescent");
            return;
        }
        let _callback = Callback::enter();
        let result = catch_unwind(AssertUnwindSafe(|| {
            (self.program.as_mut().expect("admitted program").observe)()
        }));
        if self.cancellation.is_cancelled() {
            fail(&mut s, Outcome::Cancelled, "cancelled during observation");
            self.shared.changed.notify_all();
            return;
        }
        if Instant::now() >= self.deadline {
            fail(
                &mut s,
                Outcome::TimedOut,
                "execution deadline exceeded during observation",
            );
            self.shared.changed.notify_all();
            return;
        }
        match result {
            Ok(Ok(value)) => {
                let text = value.to_string();
                let checked = crate::json::parse(
                    &text,
                    crate::json::Limits {
                        max_bytes: 65_535,
                        max_depth: 28,
                        max_nodes: 65_536,
                        reject_duplicate_keys: true,
                    },
                );
                if checked.is_err() || s.evidence_bytes + text.len() > 8 * 1_048_576 {
                    fail(
                        &mut s,
                        Outcome::ObservationFailed,
                        "observation evidence bound exceeded",
                    );
                } else {
                    s.evidence_bytes += text.len();
                    s.report
                        .observations
                        .push(json!({"afterSteps":self.next,"state":value}));
                }
            }
            Ok(Err(e)) => fail(&mut s, Outcome::ObservationFailed, e),
            Err(_) => fail(&mut s, Outcome::ObservationFailed, "observer panicked"),
        }
        if s.stopping {
            self.shared.changed.notify_all();
        }
    }
    pub fn advance(&mut self, requested: &Step) -> Result<Report, String> {
        self.check_owner()?;
        if self.closed {
            return Err("execution already closed".into());
        }
        let mut selected = None;
        {
            let mut s = lock(&self.shared);
            if s.report.outcome != Outcome::Completed {
                return Ok(s.report.clone());
            }
            if self.cancellation.is_cancelled() {
                fail(&mut s, Outcome::Cancelled, "caller cancellation");
            } else if Instant::now() >= self.deadline {
                fail(&mut s, Outcome::TimedOut, "execution deadline exceeded");
            } else if self.schedule.steps.get(self.next) != Some(requested) {
                fail(
                    &mut s,
                    Outcome::InvalidSchedule,
                    "callback differs from admitted interval",
                );
            } else {
                let index = s
                    .actors
                    .iter()
                    .position(|a| a.declaration.actor == requested.actor)
                    .expect("admitted actor");
                if s.actors[index].phase != Phase::Parked {
                    fail(&mut s, Outcome::UnexpectedCheckpoint, "actor is not parked");
                } else {
                    retain_event(&mut s, "permit", self.next, index);
                    if !s.stopping {
                        s.actors[index].phase = Phase::Running;
                        selected = Some(index);
                    }
                }
            }
            self.shared.changed.notify_all();
        }
        if let Some(index) = selected {
            if self.wait_for(self.deadline, |s| s.actors[index].phase != Phase::Running) {
                let finished;
                {
                    let mut s = lock(&self.shared);
                    retain_event(&mut s, "arrival", self.next, index);
                    if s.actors[index].checkpoint != requested.checkpoint {
                        let actual = s.actors[index].checkpoint.clone();
                        fail(
                            &mut s,
                            Outcome::UnexpectedCheckpoint,
                            format!("expected {}, arrived at {actual}", requested.checkpoint),
                        );
                    }
                    finished = s.actors[index].phase == Phase::Finished;
                }
                if finished {
                    if let Some(handle) = self.handles[index].take() {
                        if handle.join().is_err() {
                            fail(
                                &mut lock(&self.shared),
                                Outcome::ApplicationFailed,
                                "worker exit panicked",
                            );
                        }
                    }
                }
                if self.report().outcome == Outcome::Completed {
                    self.next += 1;
                    self.observe();
                }
            }
        }
        if self.report().outcome != Outcome::Completed {
            return self.cleanup(self.policy.cleanup_timeout);
        }
        if self.next == self.schedule.steps.len() {
            lock(&self.shared).report.schedule_completed = true;
            return self.cleanup(self.policy.cleanup_timeout);
        }
        Ok(self.report())
    }
    pub fn finish(&mut self) -> Result<Report, String> {
        self.check_owner()?;
        if self.closed {
            return Ok(self.report());
        }
        if self.next != self.schedule.steps.len() {
            fail(
                &mut lock(&self.shared),
                Outcome::InvalidSchedule,
                "execution finished before all intervals",
            );
        }
        self.cleanup(self.policy.cleanup_timeout)
    }
    pub fn cleanup(&mut self, timeout: Duration) -> Result<Report, String> {
        self.check_owner()?;
        if !budget(timeout) {
            return Err("invalid cleanup budget".into());
        }
        if self.closed {
            return Ok(self.report());
        }
        {
            let mut s = lock(&self.shared);
            if s.report.outcome == Outcome::Completed && !s.report.schedule_completed {
                fail(
                    &mut s,
                    Outcome::Cancelled,
                    "cleanup requested before completion",
                );
            }
            s.stopping = true;
            self.shared.changed.notify_all();
        }
        let deadline = Instant::now() + timeout;
        while self.handles.iter().flatten().any(|h| !h.is_finished()) && Instant::now() < deadline {
            let s = lock(&self.shared);
            let _guard = self
                .shared
                .changed
                .wait_timeout(s, Duration::from_millis(2))
                .unwrap_or_else(|e| e.into_inner());
        }
        for h in &mut self.handles {
            if h.as_ref().is_some_and(|h| h.is_finished()) {
                let _ = h.take().expect("finished handle").join();
            }
        }
        let remaining: Vec<String> = self
            .handles
            .iter()
            .enumerate()
            .filter(|(_, h)| h.is_some())
            .map(|(i, _)| self.declarations[i].actor.clone())
            .collect();
        if !remaining.is_empty() {
            let mut s = lock(&self.shared);
            s.report.cleanup = Cleanup::Incomplete;
            s.report.remaining_actors = remaining.clone();
            s.report
                .cleanup_attempts
                .push(json!({"cleanup":"incomplete","remainingActors":remaining}));
            return Ok(s.report.clone());
        }
        let teardown = if let Some(program) = self.program.as_mut() {
            let _callback = Callback::enter();
            catch_unwind(AssertUnwindSafe(|| (program.teardown)()))
                .unwrap_or_else(|_| Err("teardown panicked".into()))
        } else {
            Ok(())
        };
        self.closed = true;
        let mut s = lock(&self.shared);
        s.report.remaining_actors.clear();
        s.report.cleanup = if self.program.is_none() {
            Cleanup::NotStarted
        } else if teardown.is_ok() {
            Cleanup::Confirmed
        } else {
            Cleanup::TeardownFailed
        };
        let cleanup = s.report.cleanup.clone();
        s.report
            .cleanup_attempts
            .push(json!({"cleanup":cleanup,"remainingActors":[],"error":teardown.err()}));
        Ok(s.report.clone())
    }
}
impl Drop for Execution {
    fn drop(&mut self) {
        {
            let mut s = lock(&self.shared);
            s.stopping = true;
            self.shared.changed.notify_all();
        }
        for h in &mut self.handles {
            if let Some(h) = h.take() {
                let _ = h.join();
            }
        }
        if !self.closed {
            if let Some(p) = self.program.as_mut() {
                let _callback = Callback::enter();
                let _ = catch_unwind(AssertUnwindSafe(|| (p.teardown)()));
            }
        }
    }
}
fn admit(schedule: &Schedule, adapter: &Adapter, policy: &Policy) -> Result<(), (Outcome, String)> {
    let invalid = |s: &str| (Outcome::InvalidSchedule, s.to_owned());
    encode_schedule(schedule).map_err(|e| (Outcome::InvalidSchedule, e))?;
    if schedule.identity != adapter.identity || !identity_valid(&adapter.identity) {
        return Err((
            Outcome::IncompatibleIdentity,
            "adapter identity differs".into(),
        ));
    }
    if policy.max_actors == 0
        || policy.max_actors > 64
        || policy.max_steps == 0
        || policy.max_steps > 65_536
        || !budget(policy.execution_timeout)
        || !budget(policy.cleanup_timeout)
    {
        return Err(invalid("invalid policy bounds"));
    }
    if adapter.actors.is_empty()
        || adapter.actors.len() > policy.max_actors
        || schedule.steps.len() > policy.max_steps
    {
        return Err(invalid("actor/step bounds exceeded"));
    }
    let mut actors = BTreeSet::new();
    let mut operations = BTreeSet::new();
    for a in &adapter.actors {
        if !identifier(&a.actor)
            || !identifier(&a.operation)
            || !actors.insert(a.actor.clone())
            || !operations.insert(a.operation.clone())
        {
            return Err(invalid("duplicate/invalid actor or operation"));
        }
    }
    let mut checkpoints = BTreeSet::new();
    for c in &adapter.checkpoints {
        if !identifier(c) || !checkpoints.insert(c) {
            return Err(invalid("duplicate/invalid checkpoint"));
        }
    }
    let mut done = BTreeSet::new();
    for step in &schedule.steps {
        if !actors.contains(&step.actor) || done.contains(&step.actor) {
            return Err(invalid("unknown actor or interval after completion"));
        }
        if step.checkpoint == COMPLETION {
            done.insert(step.actor.clone());
        } else if !checkpoints.contains(&step.checkpoint) {
            return Err(invalid("undeclared checkpoint"));
        }
    }
    if done != actors {
        return Err(invalid("every actor requires one terminal interval"));
    }
    Ok(())
}
pub fn start_schedule(
    schedule: Schedule,
    adapter: Adapter,
    policy: Policy,
    cancellation: Cancellation,
) -> Execution {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst);
    let seed = format!(
        "{}:{generation}:{:?}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)
    );
    let id = format!("{:x}", Sha256::digest(seed.as_bytes()))[..32].to_owned();
    let report = Report {
        outcome: Outcome::Completed,
        detail: String::new(),
        schedule_completed: false,
        cleanup: Cleanup::NotStarted,
        remaining_actors: vec![],
        generation,
        execution_id: id,
        events: vec![],
        observations: vec![],
        cleanup_attempts: vec![],
    };
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            actors: vec![],
            stopping: false,
            report,
            evidence_bytes: 0,
        }),
        changed: Condvar::new(),
        checkpoints: adapter.checkpoints.iter().cloned().collect(),
    });
    let mut run = Execution {
        schedule: schedule.clone(),
        declarations: adapter.actors.clone(),
        shared,
        program: None,
        handles: vec![],
        deadline: Instant::now() + policy.execution_timeout.min(Duration::from_secs(86_400)),
        policy,
        cancellation,
        owner: thread::current().id(),
        next: 0,
        closed: false,
    };
    let error = if IN_CALLBACK.with(Cell::get) {
        Some((
            Outcome::InvalidSchedule,
            "scheduler callback reentry".into(),
        ))
    } else if run.cancellation.is_cancelled() {
        Some((Outcome::Cancelled, "cancelled before acquisition".into()))
    } else {
        admit(&schedule, &adapter, &run.policy).err()
    };
    if let Some((kind, message)) = error {
        fail(&mut lock(&run.shared), kind, message);
        run.closed = true;
        return run;
    }
    let created = {
        let _callback = Callback::enter();
        catch_unwind(AssertUnwindSafe(|| (adapter.factory)(&schedule.inputs)))
            .unwrap_or_else(|_| Err("factory panicked".into()))
    };
    match created {
        Ok(p) => run.program = Some(p),
        Err(e) => {
            fail(&mut lock(&run.shared), Outcome::ApplicationFailed, e);
            run.closed = true;
            return run;
        }
    }
    let workers = std::mem::take(&mut run.program.as_mut().expect("program").workers);
    let mut by_actor: BTreeMap<String, WorkerFunction> = BTreeMap::new();
    for worker in workers {
        if by_actor.insert(worker.actor, worker.execute).is_some() {
            fail(
                &mut lock(&run.shared),
                Outcome::ApplicationFailed,
                "duplicate worker",
            );
        }
    }
    if by_actor.len() != adapter.actors.len()
        || adapter
            .actors
            .iter()
            .any(|a| !by_actor.contains_key(&a.actor))
    {
        fail(
            &mut lock(&run.shared),
            Outcome::ApplicationFailed,
            "factory worker set differs",
        );
    }
    if run.report().outcome != Outcome::Completed {
        let _ = run.cleanup(run.policy.cleanup_timeout);
        return run;
    }
    {
        let mut s = lock(&run.shared);
        s.actors = adapter
            .actors
            .iter()
            .map(|a| Actor {
                declaration: a.clone(),
                phase: Phase::Starting,
                checkpoint: "$start".into(),
                thread: None,
            })
            .collect();
    }
    for (index, a) in adapter.actors.iter().enumerate() {
        let function = by_actor.remove(&a.actor).expect("validated worker");
        let shared = run.shared.clone();
        match thread::Builder::new()
            .name(format!("mirror-{}", a.actor))
            .spawn(move || {
                let checkpoint = Checkpoint {
                    shared: shared.clone(),
                    actor: index,
                };
                let result = catch_unwind(AssertUnwindSafe(|| {
                    {
                        let mut s = lock(&shared);
                        s.actors[index].thread = Some(thread::current().id());
                        s.actors[index].phase = Phase::Parked;
                        shared.changed.notify_all();
                        while !s.stopping && s.actors[index].phase != Phase::Running {
                            s = shared.changed.wait(s).unwrap_or_else(|e| e.into_inner());
                        }
                        if s.stopping {
                            return Ok(());
                        }
                    }
                    let _callback = Callback::enter();
                    function(&checkpoint)
                }));
                let mut s = lock(&shared);
                if !s.stopping {
                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => fail(&mut s, Outcome::ApplicationFailed, e),
                        Err(_) => fail(&mut s, Outcome::ApplicationFailed, "worker panicked"),
                    }
                }
                s.actors[index].phase = Phase::Finished;
                s.actors[index].checkpoint = COMPLETION.into();
                shared.changed.notify_all();
            }) {
            Ok(handle) => run.handles.push(Some(handle)),
            Err(e) => {
                fail(
                    &mut lock(&run.shared),
                    Outcome::ResourceFailed,
                    e.to_string(),
                );
                break;
            }
        }
    }
    if run.wait_for(run.deadline, |s| {
        s.actors.iter().all(|a| a.phase == Phase::Parked)
    }) {
        run.observe();
    }
    if run.report().outcome != Outcome::Completed {
        let _ = run.cleanup(run.policy.cleanup_timeout);
    }
    run
}
pub fn run_schedule(
    schedule: Schedule,
    adapter: Adapter,
    policy: Policy,
    cancellation: Cancellation,
) -> Execution {
    let mut run = start_schedule(schedule, adapter, policy, cancellation);
    let steps = run.schedule.steps.clone();
    for step in steps {
        if run.report().outcome != Outcome::Completed || run.closed {
            break;
        }
        let _ = run.advance(&step);
    }
    run
}
