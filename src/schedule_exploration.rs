//! Exhaustive, bounded actor-chain exploration; no partial-order reduction.
use crate::schedule::{
    self, ActorDeclaration, Adapter, Cancellation, Identity, Policy, Schedule, Step,
};
use crate::schedule_binding::ReplayResult;
use crate::{State, Value};
use serde_json::{json, Value as Json};
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::{Duration, Instant};
pub const PROFILE: &str = "mirrorrust.finite-checkpoint-exploration/v1";
#[derive(Clone)]
pub struct ActorChain {
    pub actor: ActorDeclaration,
    pub checkpoints: Vec<String>,
}
#[derive(Clone)]
pub struct FiniteSpace {
    pub identity: Identity,
    pub actors: Vec<ActorChain>,
    pub inputs: Vec<Json>,
    pub max_preemptions: usize,
    pub require_model_comparison: bool,
    pub base_variables: Vec<String>,
    pub instrumentation_variables: Vec<String>,
}
#[derive(Clone)]
pub struct ExplorationLimits {
    pub max_enumerated_schedules: usize,
    pub max_runs: usize,
    pub time_budget: Duration,
    pub max_evidence_bytes: usize,
}
impl Default for ExplorationLimits {
    fn default() -> Self {
        Self {
            max_enumerated_schedules: 4096,
            max_runs: 4096,
            time_budget: Duration::from_secs(30),
            max_evidence_bytes: 16 * 1_048_576,
        }
    }
}
pub struct ExplorationSample {
    pub execution: Json,
    pub comparison: Option<Json>,
}
pub type ExplorationRunner =
    Box<dyn FnMut(&Schedule, &Cancellation) -> Result<ExplorationSample, String>>;
fn require(ok: bool, message: &str) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(message.into())
    }
}
fn canonical_value(value: &Value, depth: usize, nodes: &mut usize) -> Result<Json, String> {
    *nodes += 1;
    require(
        depth <= 32 && *nodes <= 65_536,
        "canonical value exceeds structure bound",
    )?;
    Ok(match value {
        Value::Null => json!(["null"]),
        Value::Int(n) => json!(["integer", n.to_string()]),
        Value::Bool(v) => json!(["boolean", v]),
        Value::Str(v) => json!(["string", v]),
        Value::Unserializable(v) => json!(["unserializable", v]),
        Value::Variant(tag, v) => json!(["variant", tag, canonical_value(v, depth + 1, nodes)?]),
        Value::Record(fields) => json!([
            "record",
            fields
                .iter()
                .map(|(k, v)| Ok(json!([k, canonical_value(v, depth + 1, nodes)?])))
                .collect::<Result<Vec<_>, String>>()?
        ]),
        Value::Seq(items) | Value::Tuple(items) => json!([
            if matches!(value, Value::Seq(_)) {
                "sequence"
            } else {
                "tuple"
            },
            items
                .iter()
                .map(|v| canonical_value(v, depth + 1, nodes))
                .collect::<Result<Vec<_>, _>>()?
        ]),
        Value::Set(items) => {
            let mut unique = BTreeMap::new();
            for v in items {
                let item = canonical_value(v, depth + 1, nodes)?;
                unique.insert(item.to_string(), item);
            }
            json!(["set", unique.into_values().collect::<Vec<_>>()])
        }
        Value::Map(items) => {
            let mut entries = BTreeMap::new();
            let mut kind = None;
            for (k, v) in items {
                let actual = match k {
                    Value::Int(_) => "int",
                    Value::Str(_) => "str",
                    _ => return Err("unsupported canonical map key".into()),
                };
                if let Some(prior) = kind {
                    require(prior == actual, "mixed canonical map keys")?;
                }
                kind = Some(actual);
                let key = canonical_value(k, depth + 1, nodes)?;
                let encoded = key.to_string();
                let item = json!([key, canonical_value(v, depth + 1, nodes)?]);
                require(
                    entries.insert(encoded, item).is_none(),
                    "duplicate canonical map key",
                )?;
            }
            json!(["map", entries.into_values().collect::<Vec<_>>()])
        }
    })
}
pub fn canonical_state(state: &State) -> Result<String, String> {
    let mut nodes = 0;
    let fields = state
        .iter()
        .map(|(k, v)| Ok(json!([k, canonical_value(v, 0, &mut nodes)?])))
        .collect::<Result<Vec<_>, String>>()?;
    let text = json!(["mirrors.canonical-state/v1", fields]).to_string();
    require(
        text.len() <= 1_048_576,
        "canonical state exceeds byte bound",
    )?;
    Ok(text)
}
/// The runner retains its most recent execution, including unjoined threads.
/// Keep it alive until application-owned waits are released; dropping it joins.
pub fn local_exploration_runner(adapter: Adapter, policy: Policy) -> ExplorationRunner {
    let mut owned: Option<schedule::Execution> = None;
    Box::new(move |candidate, cancel| {
        if owned
            .as_ref()
            .is_some_and(|run| run.report().cleanup == schedule::Cleanup::Incomplete)
        {
            return Err("previous execution still owns workers".into());
        }
        owned = Some(schedule::run_schedule(
            candidate.clone(),
            adapter.clone(),
            policy.clone(),
            cancel.clone(),
        ));
        Ok(ExplorationSample {
            execution: owned.as_ref().unwrap().receipt(),
            comparison: None,
        })
    })
}
pub fn comparison_sample(result: &ReplayResult) -> Result<ExplorationSample, String> {
    let rows = result.evidence["binding"]["executions"]
        .as_array()
        .ok_or("missing binding records")?;
    require(
        rows.len() == 1,
        "exploration comparison requires one execution",
    )?;
    Ok(ExplorationSample {
        execution: rows[0].clone(),
        comparison: Some(result.evidence.clone()),
    })
}
struct Enumeration {
    shapes: Vec<Vec<Step>>,
    exhausted: bool,
    reason: String,
}
struct Enumerator<'a> {
    actors: &'a [ActorChain],
    positions: Vec<usize>,
    current: Vec<Step>,
    length: usize,
    bound: usize,
    cap: usize,
    deadline: Instant,
    cancel: &'a Cancellation,
    result: Enumeration,
}
impl Enumerator<'_> {
    fn visit(&mut self, prior: Option<usize>, preemptions: usize) {
        if !self.result.exhausted {
            return;
        }
        if self.cancel.is_cancelled() || Instant::now() >= self.deadline {
            self.result.exhausted = false;
            self.result.reason = if self.cancel.is_cancelled() {
                "cancelled"
            } else {
                "time_budget"
            }
            .into();
            return;
        }
        if self.current.len() == self.length {
            if self.result.shapes.len() == self.cap {
                self.result.exhausted = false;
                self.result.reason = "enumeration_limit".into();
            } else {
                self.result.shapes.push(self.current.clone());
            }
            return;
        }
        for i in 0..self.actors.len() {
            if self.positions[i] == self.actors[i].checkpoints.len() {
                continue;
            }
            let extra =
                usize::from(prior.is_some_and(|p| {
                    p != i && self.positions[p] < self.actors[p].checkpoints.len()
                }));
            if preemptions + extra > self.bound {
                continue;
            }
            let at = self.positions[i];
            self.positions[i] += 1;
            self.current.push(Step::new(
                &self.actors[i].actor.actor,
                &self.actors[i].checkpoints[at],
            ));
            self.visit(Some(i), preemptions + extra);
            self.current.pop();
            self.positions[i] -= 1;
            if !self.result.exhausted {
                return;
            }
        }
    }
}
fn validate_success(execution: &Json, candidate: &Schedule) -> Result<(), String> {
    require(
        execution["scheduleCompleted"] == true && execution["outcome"] == "completed",
        "false schedule completion",
    )?;
    let events = execution["events"].as_array().ok_or("missing events")?;
    require(
        events.len() == candidate.steps.len() * 2,
        "wrong event denominator",
    )?;
    for (i, step) in candidate.steps.iter().enumerate() {
        for offset in 0..2 {
            let event = &events[i * 2 + offset];
            require(
                event["ordinal"] == i * 2 + offset
                    && event["step"] == i
                    && event["actor"] == step.actor
                    && event["kind"] == if offset == 0 { "permit" } else { "arrival" },
                "event identity/order differs",
            )?;
            if offset == 1 {
                require(event["checkpoint"] == step.checkpoint, "arrival differs")?;
            }
        }
    }
    require(
        execution["observations"]
            .as_array()
            .is_some_and(|o| o.len() == candidate.steps.len() + 1),
        "wrong observation denominator",
    )
}
fn category(execution: &Json, comparison: Option<&Json>) -> Result<String, String> {
    let mut mismatch = false;
    let mut comparison_failed = false;
    if let Some(c) = comparison {
        require(
            c["schema"] == "mirrors.scheduled-comparison/v1"
                && c["binding"]["executions"] == json!([execution]),
            "comparison is not bound to exact execution",
        )?;
        if c["comparison"] == "step_mismatch" {
            require(
                c["peerTerminal"] == "step_mismatch"
                    && c["passed"] == false
                    && c["client"]["kind"] == "step_mismatch",
                "invalid model mismatch",
            )?;
            mismatch = true;
        } else {
            comparison_failed = c["comparison"] != "matched" || c["passed"] != true;
        }
    }
    if execution["cleanup"] != "confirmed" || execution["remainingActors"] != json!([]) {
        return Ok("cleanup_failed".into());
    }
    if mismatch {
        return Ok("model_mismatch".into());
    }
    if comparison_failed {
        return Ok("comparison_failed".into());
    }
    Ok(if execution["passed"] == true {
        "passed"
    } else {
        match execution["outcome"].as_str() {
            Some("timed_out") => "timed_out",
            Some("cancelled") => "cancelled",
            Some("unexpected_checkpoint" | "invalid_schedule" | "incompatible_identity") => {
                "schedule_failed"
            }
            _ => "execution_failed",
        }
    }
    .into())
}
pub fn explore_finite(
    space: &FiniteSpace,
    runner: &mut ExplorationRunner,
    limits: &ExplorationLimits,
    cancel: &Cancellation,
) -> Json {
    let start = Instant::now();
    let mut output = json!({"schema":"mirrors.finite-exploration/v1","profile":PROFILE,"complete":false,"enumerationExhausted":false,"denominatorKnown":false,"eligibleRuns":null,"attemptedRuns":0,"completedRuns":0,"runs":[],"states":[],"transitions":[],"categories":{},"firstCounterexample":null,"firstFailure":null,"por":"disabled","projection":{"baseVariables":space.base_variables,"instrumentationVariables":space.instrumentation_variables},"comparison":"not_requested","comparisonRequired":space.require_model_comparison,"comparisonRuns":0});
    let result = (|| -> Result<(), String> {
        require(
            !space.actors.is_empty()
                && space.actors.len() <= 8
                && !space.inputs.is_empty()
                && space.inputs.len() <= 64,
            "finite actor/input bound exceeded",
        )?;
        require(
            space.max_preemptions <= 64
                && limits.max_enumerated_schedules > 0
                && limits.max_enumerated_schedules <= 4096
                && limits.max_runs <= 65_536
                && !limits.time_budget.is_zero()
                && limits.time_budget <= Duration::from_secs(86_400)
                && limits.max_evidence_bytes > 0
                && limits.max_evidence_bytes <= 64 * 1_048_576,
            "invalid exploration limits",
        )?;
        let mut actors = space.actors.clone();
        actors.sort_by(|a, b| a.actor.actor.cmp(&b.actor.actor));
        let mut declarations = BTreeMap::new();
        let mut operations = BTreeSet::new();
        let mut check = Schedule::new(space.identity.clone(), Json::Null, vec![]);
        for a in &actors {
            require(
                schedule::identifier(&a.actor.actor)
                    && schedule::identifier(&a.actor.operation)
                    && declarations
                        .insert(a.actor.actor.clone(), a.actor.operation.clone())
                        .is_none()
                    && operations.insert(a.actor.operation.clone()),
                "duplicate/invalid actor or operation",
            )?;
            require(
                !a.checkpoints.is_empty() && a.checkpoints.last().unwrap() == schedule::COMPLETION,
                "actor chain requires completion",
            )?;
            for (i, c) in a.checkpoints.iter().enumerate() {
                require(
                    if i + 1 == a.checkpoints.len() {
                        c == schedule::COMPLETION
                    } else {
                        schedule::identifier(c)
                    },
                    "invalid checkpoint chain",
                )?;
                check.steps.push(Step::new(&a.actor.actor, c));
            }
        }
        require(check.steps.len() <= 64, "exploration step bound exceeded")?;
        let mut input_keys = BTreeSet::new();
        for input in &space.inputs {
            check.inputs = input.clone();
            schedule::encode_schedule(&check)?;
            require(
                input_keys.insert(input.to_string()),
                "duplicate finite input",
            )?;
        }
        let mut variables = BTreeSet::new();
        let mut ignored = BTreeSet::new();
        require(
            !space.base_variables.is_empty()
                && space.base_variables.len() <= 1024
                && space.instrumentation_variables.len() <= 1024,
            "bounded base variables required",
        )?;
        for v in &space.base_variables {
            require(
                schedule::identifier(v) && variables.insert(v.clone()),
                "invalid base variable",
            )?;
        }
        for v in &space.instrumentation_variables {
            require(
                schedule::identifier(v) && !variables.contains(v) && ignored.insert(v.clone()),
                "invalid instrumentation projection",
            )?;
        }
        let deadline = start + limits.time_budget;
        let mut enumerator = Enumerator {
            actors: &actors,
            positions: vec![0; actors.len()],
            current: vec![],
            length: check.steps.len(),
            bound: space.max_preemptions,
            cap: limits.max_enumerated_schedules,
            deadline,
            cancel,
            result: Enumeration {
                shapes: vec![],
                exhausted: true,
                reason: String::new(),
            },
        };
        enumerator.visit(None, 0);
        let enumeration = enumerator.result;
        let eligible = enumeration.shapes.len() * space.inputs.len();
        output["denominatorKnown"] = json!(enumeration.exhausted);
        output["eligibleRunsLowerBound"] = json!(eligible);
        if enumeration.exhausted {
            output["eligibleRuns"] = json!(eligible);
        }
        output["enumeratedScheduleShapes"] = json!(enumeration.shapes.len());
        output["inputAssignments"] = json!(space.inputs.len());
        output["declaredInputs"] = json!(space.inputs);
        output["maxPreemptions"] = json!(space.max_preemptions);
        output["limits"] = json!({"maxEnumeratedSchedules":limits.max_enumerated_schedules,"maxRuns":limits.max_runs,"timeBudgetMs":limits.time_budget.as_millis(),"maxEvidenceBytes":limits.max_evidence_bytes});
        let (
            mut attempted,
            mut completed,
            mut evidence_bytes,
            mut coverage_bytes,
            mut comparison_runs,
        ) = (0usize, 0usize, 0usize, 0usize, 0usize);
        let mut stop_reason = enumeration.reason;
        let mut halt = false;
        let mut states = BTreeMap::<String, usize>::new();
        let mut edges = BTreeMap::<(usize, usize), usize>::new();
        let mut execution_ids = BTreeSet::new();
        'outer: for steps in enumeration.shapes {
            for (input_index, input) in space.inputs.iter().enumerate() {
                if cancel.is_cancelled()
                    || Instant::now() >= deadline
                    || attempted >= limits.max_runs
                {
                    stop_reason = if cancel.is_cancelled() {
                        "cancelled"
                    } else if Instant::now() >= deadline {
                        "time_budget"
                    } else {
                        "run_limit"
                    }
                    .into();
                    halt = true;
                    break 'outer;
                }
                let candidate = Schedule::new(space.identity.clone(), input.clone(), steps.clone());
                let mut row =
                    json!({"ordinal":attempted,"inputIndex":input_index,"schedule":candidate});
                attempted += 1;
                let mut cleanup_known = false;
                let evaluated = catch_unwind(AssertUnwindSafe(|| -> Result<String, String> {
                    let sample = runner(&candidate, cancel)?;
                    let execution = sample.execution;
                    row["execution"] = execution.clone();
                    if let Some(c) = &sample.comparison {
                        row["comparison"] = c.clone();
                    }
                    require(
                        execution["schema"] == "mirrors.checkpoint-execution/v1"
                            && execution["schedule"] == row["schedule"],
                        "execution not bound to exact schedule",
                    )?;
                    let actors = execution["actors"]
                        .as_array()
                        .ok_or("missing execution actors")?;
                    let mut actual = BTreeMap::new();
                    for a in actors {
                        let a: ActorDeclaration =
                            serde_json::from_value(a.clone()).map_err(|e| e.to_string())?;
                        require(
                            actual.insert(a.actor, a.operation).is_none(),
                            "duplicate execution actor",
                        )?;
                    }
                    require(
                        actual == declarations,
                        "execution actor declarations differ",
                    )?;
                    let id = execution["executionId"]
                        .as_str()
                        .ok_or("missing execution id")?;
                    require(
                        id.len() == 32
                            && id
                                .bytes()
                                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                            && execution_ids.insert(id.to_owned()),
                        "stale/invalid execution id",
                    )?;
                    cleanup_known = execution["cleanup"] == "confirmed"
                        && execution["remainingActors"] == json!([]);
                    let mut category = category(&execution, sample.comparison.as_ref())?;
                    if sample
                        .comparison
                        .as_ref()
                        .is_some_and(|c| c["comparison"] == "step_mismatch")
                        && output["firstCounterexample"].is_null()
                    {
                        output["firstCounterexample"] = json!({"ordinal":attempted-1,"category":"model_mismatch","schedule":candidate});
                    }
                    if space.require_model_comparison && sample.comparison.is_none() {
                        category = "comparison_missing".into();
                    }
                    if sample.comparison.is_some() {
                        comparison_runs += 1;
                        output["comparison"] = json!("requested");
                    }
                    if category == "passed" {
                        validate_success(&execution, &candidate)?;
                        let mut prior = None;
                        for (i, observation) in execution["observations"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .enumerate()
                        {
                            require(
                                observation["afterSteps"] == i,
                                "observation position differs",
                            )?;
                            let mut state: State =
                                serde_json::from_value(observation["state"].clone())
                                    .map_err(|e| e.to_string())?;
                            require(
                                state.len() == variables.len() + ignored.len(),
                                "projection omits/adds variables",
                            )?;
                            for v in &ignored {
                                require(
                                    state.remove(v).is_some(),
                                    "instrumentation variable absent",
                                )?;
                            }
                            for v in &variables {
                                require(state.contains_key(v), "base variable absent")?;
                            }
                            let key = canonical_state(&state)?;
                            let id = if let Some(id) = states.get(&key) {
                                *id
                            } else {
                                coverage_bytes += key.len() + 128;
                                require(
                                    coverage_bytes <= 8 * 1_048_576,
                                    "coverage byte bound exceeded",
                                )?;
                                let id = states.len();
                                output["states"].as_array_mut().unwrap().push(json!({"id":id,"canonical":serde_json::from_str::<Json>(&key).unwrap()}));
                                states.insert(key, id);
                                id
                            };
                            if let Some(from) = prior {
                                let edge = (from, id);
                                if !edges.contains_key(&edge) {
                                    coverage_bytes += 128;
                                    require(
                                        coverage_bytes <= 8 * 1_048_576,
                                        "coverage byte bound exceeded",
                                    )?;
                                }
                                *edges.entry(edge).or_default() += 1;
                            }
                            prior = Some(id);
                        }
                        completed += 1;
                    } else if category == "model_mismatch"
                        && output["firstCounterexample"].is_null()
                    {
                        output["firstCounterexample"] =
                            json!({"ordinal":attempted-1,"category":category,"schedule":candidate});
                    }
                    Ok(category)
                }));
                let category = match evaluated {
                    Ok(Ok(c)) => c,
                    Ok(Err(e)) => {
                        if e == "coverage byte bound exceeded" {
                            halt = true;
                            stop_reason = "coverage_limit".into();
                        }
                        row["error"] = json!(e.chars().take(1024).collect::<String>());
                        "runner_or_evidence_failed".into()
                    }
                    Err(_) => {
                        row["error"] = json!("runner panicked");
                        "runner_or_evidence_failed".into()
                    }
                };
                row["category"] = json!(category);
                if category != "passed" && output["firstFailure"].is_null() {
                    output["firstFailure"] =
                        json!({"ordinal":attempted-1,"category":category,"schedule":candidate});
                }
                let count = output["categories"][&category].as_u64().unwrap_or(0) + 1;
                output["categories"][&category] = json!(count);
                let length = row.to_string().len();
                if evidence_bytes + length > limits.max_evidence_bytes {
                    output["runs"].as_array_mut().unwrap().push(json!({"ordinal":attempted-1,"category":category,"evidenceOmitted":"byte_budget"}));
                    stop_reason = "evidence_limit".into();
                    halt = true;
                    break 'outer;
                }
                evidence_bytes += length;
                output["runs"].as_array_mut().unwrap().push(row);
                if halt {
                    break 'outer;
                }
                if !cleanup_known {
                    stop_reason = "unconfirmed_cleanup".into();
                    halt = true;
                    break 'outer;
                }
                if Instant::now() >= deadline {
                    stop_reason = "time_budget".into();
                    halt = true;
                    break 'outer;
                }
            }
        }
        output["transitions"] = json!(edges
            .iter()
            .map(|((from, to), count)| json!({"from":from,"to":to,"count":count}))
            .collect::<Vec<_>>());
        output["comparisonRuns"] = json!(comparison_runs);
        output["attemptedRuns"] = json!(attempted);
        output["completedRuns"] = json!(completed);
        let exhausted = enumeration.exhausted && attempted == eligible;
        let complete = exhausted && completed == eligible && !halt;
        output["enumerationExhausted"] = json!(exhausted);
        output["complete"] = json!(complete);
        if stop_reason.is_empty() {
            stop_reason = if complete {
                "complete"
            } else {
                "non_pass_runs"
            }
            .into();
        }
        output["stopReason"] = json!(stop_reason);
        output["status"] = json!(if complete { "complete" } else { "incomplete" });
        Ok(())
    })();
    if let Err(error) = result {
        output["status"] = json!("invalid_declaration");
        output["stopReason"] = json!("invalid_declaration");
        output["error"] = json!(error.chars().take(1024).collect::<String>());
    }
    output["elapsedMs"] = json!(start.elapsed().as_millis());
    output
}
