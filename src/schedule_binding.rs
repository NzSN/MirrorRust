//! Connection-local ownership for generated checkpoint ports and actual replay evidence.
use crate::schedule::{
    self, Adapter, Cancellation, Cleanup, Execution, Outcome, Policy, Schedule, Step,
};
use crate::{
    ApalacheConfig, BindingError, CompiledAdapterSelection, Error, NegotiatedError, Transport,
};
use serde_json::{json, Value as Json};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

pub struct BindingSession {
    schedule: Schedule,
    adapter: Adapter,
    policy: Policy,
    cancel: Cancellation,
    execution: Option<Execution>,
    executions: Vec<Json>,
    receipt_bytes: usize,
    initializations: usize,
    disposals: usize,
    disposed: bool,
    retained: bool,
    receipt_complete: bool,
    disposal: Result<(), BindingError>,
}
fn error(code: &str, message: impl Into<String>) -> BindingError {
    BindingError::new(code, message)
}
impl BindingSession {
    pub fn new(schedule: Schedule, adapter: Adapter, policy: Policy, cancel: Cancellation) -> Self {
        Self {
            schedule,
            adapter,
            policy,
            cancel,
            execution: None,
            executions: vec![],
            receipt_bytes: 0,
            initializations: 0,
            disposals: 0,
            disposed: false,
            retained: false,
            receipt_complete: true,
            disposal: Ok(()),
        }
    }
    fn retain(&mut self) {
        if self.retained {
            return;
        }
        if let Some(execution) = &self.execution {
            let record = execution.receipt();
            let bytes = record.to_string().len();
            if self.receipt_bytes + bytes > 16 * 1_048_576 {
                self.receipt_complete = false;
                self.executions
                    .push(json!({"passed":false,"evidenceError":"receipt_byte_bound_exceeded"}));
            } else {
                self.receipt_bytes += bytes;
                self.executions.push(record);
            }
            self.retained = true;
        }
    }
    pub fn initialize(&mut self) -> Result<(), BindingError> {
        if self.disposed || self.initializations >= 64 {
            return Err(error(
                "schedule_lifecycle_invalid",
                "disposed session or initialization limit",
            ));
        }
        if let Some(execution) = self.execution.as_mut() {
            let report = execution
                .finish()
                .map_err(|e| error("schedule_lifecycle_invalid", e))?;
            self.retain();
            if !report.passed() || !self.receipt_complete {
                return Err(error(
                    "schedule_previous_incomplete",
                    "previous execution incomplete",
                ));
            }
            self.execution = None;
        }
        self.initializations += 1;
        self.retained = false;
        self.execution = Some(schedule::start_schedule(
            self.schedule.clone(),
            self.adapter.clone(),
            self.policy.clone(),
            self.cancel.clone(),
        ));
        let report = self.execution.as_ref().unwrap().report();
        if report.outcome != Outcome::Completed {
            return Err(error(
                "schedule_admission_failed",
                format!("{:?}: {}", report.outcome, report.detail),
            ));
        }
        Ok(())
    }
    pub fn advance(&mut self, step: &Step) -> Result<(), BindingError> {
        if self.disposed {
            return Err(error("schedule_lifecycle_invalid", "session disposed"));
        }
        let execution = self
            .execution
            .as_mut()
            .ok_or_else(|| error("schedule_lifecycle_invalid", "session not initialized"))?;
        let report = execution
            .advance(step)
            .map_err(|e| error("schedule_lifecycle_invalid", e))?;
        if report.outcome != Outcome::Completed {
            return Err(error(
                "schedule_interval_failed",
                format!("{:?}: {}", report.outcome, report.detail),
            ));
        }
        Ok(())
    }
    pub fn observation(&self) -> Result<Json, BindingError> {
        if self.disposed {
            return Err(error("schedule_lifecycle_invalid", "session disposed"));
        }
        let report = self
            .execution
            .as_ref()
            .ok_or_else(|| error("schedule_lifecycle_invalid", "session not initialized"))?
            .report();
        if report.outcome != Outcome::Completed {
            return Err(error("schedule_observation_unavailable", report.detail));
        }
        report
            .observations
            .last()
            .map(|v| v["state"].clone())
            .ok_or_else(|| error("schedule_observation_unavailable", "no actual observation"))
    }
    pub fn dispose(&mut self) -> Result<(), BindingError> {
        if self.disposed {
            return self.disposal.clone();
        }
        self.disposed = true;
        self.disposals += 1;
        if let Some(execution) = self.execution.as_mut() {
            let result = if execution.report().schedule_completed {
                execution.finish()
            } else {
                execution.cleanup(self.policy.cleanup_timeout)
            };
            match result {
                Ok(report) if report.cleanup == Cleanup::Confirmed => {}
                Ok(report) => {
                    self.disposal = Err(error(
                        "schedule_cleanup_failed",
                        format!("{:?}", report.cleanup),
                    ))
                }
                Err(e) => self.disposal = Err(error("schedule_cleanup_failed", e)),
            }
            self.retain();
            if !self.receipt_complete {
                self.disposal = Err(error("schedule_cleanup_failed", "receipt bound exceeded"));
            }
        }
        self.disposal.clone()
    }
    /// Retains an incomplete execution after dispose; callers may release their
    /// application-owned wait and retry cleanup without rewriting the primary failure.
    pub fn retry_cleanup(&mut self) -> Result<(), BindingError> {
        if !self.disposed {
            return Err(error(
                "schedule_lifecycle_invalid",
                "dispose before retrying cleanup",
            ));
        }
        if let Some(execution) = self.execution.as_mut() {
            let report = execution
                .cleanup(self.policy.cleanup_timeout)
                .map_err(|e| error("schedule_cleanup_failed", e))?;
            if report.cleanup != Cleanup::Confirmed {
                return Err(error(
                    "schedule_cleanup_failed",
                    "cleanup remains unconfirmed",
                ));
            }
        }
        if self.retained && !self.executions.is_empty() {
            if let Some(execution) = &self.execution {
                let record = execution.receipt();
                let old = self.executions.last().unwrap().to_string().len();
                let total = self.receipt_bytes.saturating_sub(old) + record.to_string().len();
                if total <= 16 * 1_048_576 {
                    self.receipt_bytes = total;
                    *self.executions.last_mut().unwrap() = record;
                } else {
                    self.receipt_complete = false;
                }
            }
        }
        Ok(())
    }
    pub fn receipt(&self) -> Json {
        let mut records = self.executions.clone();
        let mut complete = self.receipt_complete;
        if !self.retained {
            if let Some(execution) = &self.execution {
                let row = execution.receipt();
                if self.receipt_bytes + row.to_string().len() <= 16 * 1_048_576 {
                    records.push(row);
                } else {
                    complete = false;
                    records.push(
                        json!({"passed":false,"evidenceError":"receipt_byte_bound_exceeded"}),
                    );
                }
            }
        }
        json!({"schema":"mirrors.scheduled-binding/v1","initializations":self.initializations,"disposals":self.disposals,"disposed":self.disposed,"receiptComplete":complete,"executions":records})
    }
    pub fn fully_completed(&self) -> bool {
        self.disposed
            && self.initializations > 0
            && self.disposal.is_ok()
            && self.receipt_complete
            && !self.executions.is_empty()
            && self.executions.iter().all(|r| r["passed"] == true)
    }
}
pub struct ReplayResult {
    pub client_result: Result<(), NegotiatedError>,
    pub evidence: Json,
    pub passed: bool,
}
fn client_evidence(result: &Result<(), NegotiatedError>) -> Json {
    let Err(error) = result else {
        return json!({"status":"succeeded"});
    };
    let mut row = json!({"status":"failed","message":error.to_string(),"orderedHints":[]});
    match error {
        NegotiatedError::Registration { code, .. } => {
            row["kind"] = json!("registration");
            row["code"] = json!(code);
        }
        NegotiatedError::ModelInterface { code, .. } => {
            row["kind"] = json!("model_interface");
            row["code"] = json!(code);
        }
        NegotiatedError::Legacy(Error::StepMismatch {
            expected,
            actual,
            hints,
            ..
        }) => {
            row["kind"] = json!("step_mismatch");
            row["expected"] = serde_json::to_value(expected).unwrap();
            row["actual"] = serde_json::to_value(actual).unwrap();
            row["orderedHints"] = json!(hints.iter().map(|h| format!("{h:?}")).collect::<Vec<_>>());
        }
        NegotiatedError::Legacy(_) => row["kind"] = json!("transport_or_protocol"),
    }
    row
}
/// Uses the existing negotiated runner. No synthetic success verdict is inferred
/// from adapter observations; the actual decoded peer terminal is retained.
pub fn replay_with_traces(
    mut transport: Transport,
    config: ApalacheConfig,
    traces: Vec<String>,
    selection: &mut CompiledAdapterSelection<'_>,
    session: Rc<RefCell<BindingSession>>,
) -> ReplayResult {
    let before = session.borrow().receipt();
    if before["initializations"] != 0 || before["disposals"] != 0 || before["disposed"] != false {
        let result = Err(NegotiatedError::ModelInterface {
            code: "schedule_session_reused".into(),
            message: "scheduled replay requires fresh session".into(),
        });
        let _ = transport.close();
        return ReplayResult {
            evidence: json!({"schema":"mirrors.scheduled-comparison/v1","passed":false,"comparison":"incomplete","peerTerminal":"","peerTerminalRaw":"","client":client_evidence(&result),"binding":before}),
            client_result: result,
            passed: false,
        };
    }
    let terminal = Arc::new(Mutex::new((String::new(), String::new())));
    let observed = terminal.clone();
    transport.observe_received(move |line| {
        let verdict = match crate::decode_mirror_message(line) {
            Ok(crate::MirrorMessage::AllStepsDone) => Some("all_steps_done"),
            Ok(crate::MirrorMessage::StepMismatch { .. }) => Some("step_mismatch"),
            _ => None,
        };
        if let Some(verdict) = verdict {
            *observed.lock().unwrap() = (verdict.into(), line.into());
        }
    });
    let result =
        crate::run_client_with_traces_negotiated_transport(transport, config, traces, selection);
    let (terminal, raw) = terminal.lock().unwrap().clone();
    let comparison = if terminal == "step_mismatch"
        && matches!(
            result,
            Err(NegotiatedError::Legacy(Error::StepMismatch { .. }))
        ) {
        "step_mismatch"
    } else if terminal == "all_steps_done"
        && (result.is_ok()
            || matches!(&result,Err(NegotiatedError::ModelInterface{code,..})if code=="adapter_dispose_failed"))
    {
        "matched"
    } else {
        "incomplete"
    };
    let passed = result.is_ok() && comparison == "matched" && session.borrow().fully_completed();
    let evidence = json!({"schema":"mirrors.scheduled-comparison/v1","passed":passed,"comparison":comparison,"peerTerminal":terminal,"peerTerminalRaw":raw,"client":client_evidence(&result),"binding":session.borrow().receipt()});
    ReplayResult {
        client_result: result,
        evidence,
        passed,
    }
}
