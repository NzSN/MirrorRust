use crate::protocol::{
    decode_mirror_message, encode_client_message, ApalacheConfig, ApalacheSpec, ClientMessage,
    JobKind, JobOutcome, JobPhase, MirrorMessage, SpecResult, State, TraceGenerationConfig,
};
use crate::transport::{spawn_mirror, Transport};
use crate::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenTracesResult {
    pub itf_trace_paths: Vec<String>,
    pub itf_traces: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobAccepted {
    pub job_id: String,
    pub kind: JobKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobReply {
    Status { job_id: String, phase: JobPhase },
    Result { job_id: String, outcome: JobOutcome },
}

/// Computes the next reported state for each protocol step.
pub trait StateComputer {
    fn compute(&mut self, action: &str, params: &State, prev: &State) -> State;
}

impl<F> StateComputer for F
where
    F: FnMut(&str, &State, &State) -> State,
{
    fn compute(&mut self, action: &str, params: &State, prev: &State) -> State {
        self(action, params, prev)
    }
}

/// Serves a fixed sequence of states in order; panics when exhausted.
#[derive(Debug)]
pub struct PresetClient {
    states: Vec<State>,
    index: usize,
}

impl StateComputer for PresetClient {
    fn compute(&mut self, _action: &str, _params: &State, _prev: &State) -> State {
        if self.index >= self.states.len() {
            panic!("preset_client exhausted");
        }
        let s = self.states[self.index].clone();
        self.index += 1;
        s
    }
}

pub fn preset_client(states: Vec<State>) -> PresetClient {
    PresetClient { states, index: 0 }
}

pub fn run_client(
    bin_path: &str,
    apalache_config: ApalacheConfig,
    trace_config: TraceGenerationConfig,
    compute: impl StateComputer,
) -> Result<(), Error> {
    run_client_with_inline_spec(bin_path, apalache_config, trace_config, compute, None)
}

pub fn run_client_with_inline_spec(
    bin_path: &str,
    apalache_config: ApalacheConfig,
    trace_config: TraceGenerationConfig,
    compute: impl StateComputer,
    spec: Option<ApalacheSpec>,
) -> Result<(), Error> {
    run_client_with_transport(
        spawn_mirror(bin_path)?,
        apalache_config,
        trace_config,
        compute,
        spec,
    )
}

/// Run a generated-trace replay on an already connected transport.
/// The transport is consumed because one physical connection carries exactly
/// one Mirrors session.
pub fn run_client_with_transport(
    mut t: Transport,
    apalache_config: ApalacheConfig,
    trace_config: TraceGenerationConfig,
    compute: impl StateComputer,
    spec: Option<ApalacheSpec>,
) -> Result<(), Error> {
    t.send(&encode_client_message(&ClientMessage::Register {
        apalache_config,
        trace_config,
        spec,
    }))?;
    main_loop(t, compute)
}

pub fn run_client_with_traces(
    bin_path: &str,
    apalache_config: ApalacheConfig,
    trace_paths: Vec<String>,
    compute: impl StateComputer,
) -> Result<(), Error> {
    run_client_with_traces_transport(
        spawn_mirror(bin_path)?,
        apalache_config,
        trace_paths,
        compute,
    )
}

pub fn run_client_with_traces_transport(
    mut t: Transport,
    apalache_config: ApalacheConfig,
    trace_paths: Vec<String>,
    compute: impl StateComputer,
) -> Result<(), Error> {
    t.send(&encode_client_message(&ClientMessage::RegisterTraces {
        apalache_config,
        itf_trace_paths: trace_paths,
    }))?;
    main_loop(t, compute)
}

pub fn run_client_gen_traces(
    bin_path: &str,
    apalache_config: ApalacheConfig,
    dest_path: &str,
    trace_config: TraceGenerationConfig,
) -> Result<GenTracesResult, Error> {
    run_client_gen_traces_with_inline_spec(bin_path, apalache_config, dest_path, trace_config, None)
}

pub fn run_client_gen_traces_with_inline_spec(
    bin_path: &str,
    apalache_config: ApalacheConfig,
    dest_path: &str,
    trace_config: TraceGenerationConfig,
    spec: Option<ApalacheSpec>,
) -> Result<GenTracesResult, Error> {
    run_client_gen_traces_transport(
        spawn_mirror(bin_path)?,
        apalache_config,
        dest_path,
        trace_config,
        spec,
    )
}

pub fn run_client_gen_traces_transport(
    mut t: Transport,
    apalache_config: ApalacheConfig,
    dest_path: &str,
    trace_config: TraceGenerationConfig,
    spec: Option<ApalacheSpec>,
) -> Result<GenTracesResult, Error> {
    t.send(&encode_client_message(&ClientMessage::RegisterTraceGen {
        apalache_config,
        trace_config,
        dest_path: Some(dest_path.to_string()),
        spec,
    }))?;
    gen_traces_loop(t)
}

pub fn run_client_validate(
    bin_path: &str,
    apalache_config: ApalacheConfig,
    bound: u32,
    spec: Option<ApalacheSpec>,
) -> Result<(), Error> {
    if !(1..=100).contains(&bound) {
        return Err(Error::InvalidArgument(
            "validate bound must be in [1, 100]".into(),
        ));
    }
    run_client_validate_transport(spawn_mirror(bin_path)?, apalache_config, bound, spec)
}

pub fn run_client_validate_transport(
    mut t: Transport,
    apalache_config: ApalacheConfig,
    bound: u32,
    spec: Option<ApalacheSpec>,
) -> Result<(), Error> {
    if !(1..=100).contains(&bound) {
        return Err(Error::InvalidArgument(
            "validate bound must be in [1, 100]".into(),
        ));
    }
    t.send(&encode_client_message(&ClientMessage::RegisterValidate {
        apalache_config,
        bound,
        spec,
    }))?;
    let result = match recv(&mut t)? {
        MirrorMessage::SpecValidated {
            result: SpecResult::Valid,
        } => Ok(()),
        MirrorMessage::SpecValidated {
            result: SpecResult::Invalid(detail),
        } => Err(Error::SpecInvalid(detail)),
        MirrorMessage::ProtocolError { error } => Err(Error::ProtocolError(error)),
        MirrorMessage::RegisterError { error } => Err(Error::RegisterFailed(error)),
        other => Err(Error::UnexpectedMessage(format!(
            "expected spec_validated, got {other:?}"
        ))),
    };
    let _ = t.close();
    result
}

/// Submit a validation job over a server-mode TCP or mTLS transport.
pub fn submit_validate_async(
    t: &mut Transport,
    apalache_config: ApalacheConfig,
    bound: u32,
    spec: Option<ApalacheSpec>,
) -> Result<JobAccepted, Error> {
    if !(1..=100).contains(&bound) {
        return Err(Error::InvalidArgument(
            "validate bound must be in [1, 100]".into(),
        ));
    }
    if !t.is_async_capable() {
        return Err(Error::InvalidArgument(
            "async jobs require a TCP or mTLS server-mode transport".into(),
        ));
    }
    send_job(
        t,
        &encode_client_message(&ClientMessage::RegisterValidateAsync {
            apalache_config,
            bound,
            spec,
        }),
    )?;
    match recv_job(t)? {
        MirrorMessage::JobAccepted { job_id, kind } if kind == JobKind::Validate => {
            Ok(JobAccepted { job_id, kind })
        }
        MirrorMessage::RegisterError { error } => Err(Error::RegisterFailed(error)),
        MirrorMessage::ProtocolError { error } => {
            let _ = t.close();
            Err(Error::ProtocolError(error))
        }
        other => {
            let _ = t.close();
            Err(Error::UnexpectedMessage(format!(
                "expected job_accepted, got {other:?}"
            )))
        }
    }
}

pub fn submit_trace_gen_async(
    t: &mut Transport,
    apalache_config: ApalacheConfig,
    trace_config: TraceGenerationConfig,
    dest_path: Option<String>,
    spec: Option<ApalacheSpec>,
) -> Result<JobAccepted, Error> {
    assert_async_capable(t)?;
    send_job(
        t,
        &encode_client_message(&ClientMessage::RegisterTraceGenAsync {
            apalache_config,
            trace_config,
            dest_path,
            spec,
        }),
    )?;
    match recv_job(t)? {
        MirrorMessage::JobAccepted { job_id, kind } if kind == JobKind::GenTraces => {
            Ok(JobAccepted { job_id, kind })
        }
        MirrorMessage::RegisterError { error } => Err(Error::RegisterFailed(error)),
        MirrorMessage::ProtocolError { error } => {
            let _ = t.close();
            Err(Error::ProtocolError(error))
        }
        other => {
            let _ = t.close();
            Err(Error::UnexpectedMessage(format!(
                "expected job_accepted, got {other:?}"
            )))
        }
    }
}

fn assert_async_capable(t: &Transport) -> Result<(), Error> {
    if t.is_async_capable() {
        Ok(())
    } else {
        Err(Error::InvalidArgument(
            "async jobs require a TCP or mTLS server-mode transport".into(),
        ))
    }
}

fn decode_job_reply(
    t: &mut Transport,
    message: MirrorMessage,
    expected_id: &str,
) -> Result<JobReply, Error> {
    match message {
        MirrorMessage::JobStatus { job_id, phase } if job_id == expected_id => {
            Ok(JobReply::Status { job_id, phase })
        }
        MirrorMessage::JobResult { job_id, outcome } if job_id == expected_id => {
            Ok(JobReply::Result { job_id, outcome })
        }
        MirrorMessage::RegisterError { error } => Err(Error::RegisterFailed(error)),
        MirrorMessage::ProtocolError { error } => {
            let _ = t.close();
            Err(Error::ProtocolError(error))
        }
        other => {
            let _ = t.close();
            Err(Error::UnexpectedMessage(format!(
                "expected job_status or job_result, got {other:?}"
            )))
        }
    }
}

pub fn query_job(t: &mut Transport, job_id: &str) -> Result<JobReply, Error> {
    assert_async_capable(t)?;
    send_job(
        t,
        &encode_client_message(&ClientMessage::QueryJob {
            job_id: job_id.to_string(),
        }),
    )?;
    let message = recv_job(t)?;
    decode_job_reply(t, message, job_id)
}

pub fn await_job(
    t: &mut Transport,
    job_id: &str,
    timeout_secs: Option<u64>,
) -> Result<JobReply, Error> {
    assert_async_capable(t)?;
    send_job(
        t,
        &encode_client_message(&ClientMessage::AwaitJob {
            job_id: job_id.to_string(),
            timeout_secs,
        }),
    )?;
    let message = recv_job(t)?;
    decode_job_reply(t, message, job_id)
}

pub fn cancel_job(t: &mut Transport, job_id: &str) -> Result<JobReply, Error> {
    assert_async_capable(t)?;
    send_job(
        t,
        &encode_client_message(&ClientMessage::CancelJob {
            job_id: job_id.to_string(),
        }),
    )?;
    let message = recv_job(t)?;
    decode_job_reply(t, message, job_id)
}

pub(crate) fn recv(t: &mut Transport) -> Result<MirrorMessage, Error> {
    match t.recv()? {
        Some(line) => decode_mirror_message(&line),
        None => Err(Error::TransportClosed),
    }
}

// Any failed exchange poisons this borrowed connection. Preserve the first error.
fn send_job(t: &mut Transport, line: &str) -> Result<(), Error> {
    let result = t.send(line);
    if result.is_err() {
        let _ = t.close();
    }
    result
}

fn recv_job(t: &mut Transport) -> Result<MirrorMessage, Error> {
    let result = recv(t);
    if result.is_err() {
        let _ = t.close();
    }
    result
}

fn encode_report_state(state: &State) -> String {
    encode_client_message(&ClientMessage::ReportState {
        state: state.clone(),
    })
}

fn main_loop(mut t: Transport, mut compute: impl StateComputer) -> Result<(), Error> {
    let result = run_main_loop(&mut t, &mut compute);
    let _ = t.close();
    result
}

fn run_main_loop(t: &mut Transport, compute: &mut impl StateComputer) -> Result<(), Error> {
    match recv(t)? {
        MirrorMessage::SpecValidated {
            result: SpecResult::Valid,
        } => {}
        MirrorMessage::SpecValidated {
            result: SpecResult::Invalid(s),
        } => return Err(Error::SpecInvalid(s)),
        MirrorMessage::ProtocolError { error } => return Err(Error::ProtocolError(error)),
        MirrorMessage::RegisterError { error } => return Err(Error::RegisterFailed(error)),
        other => {
            return Err(Error::UnexpectedMessage(format!(
                "expected spec_validated, got {other:?}"
            )))
        }
    }

    run_stepping_loop(t, |action, params, prev| {
        Ok(compute.compute(action, params, prev))
    })
}

pub(crate) fn run_stepping_loop(
    t: &mut Transport,
    mut compute: impl FnMut(&str, &State, &State) -> Result<State, Error>,
) -> Result<(), Error> {
    let mut state: State = State::new();
    let mut last_param: State = State::new();
    let mut last_action = String::new();

    loop {
        match recv(t)? {
            MirrorMessage::InitialState {
                action,
                state: from_mirror,
            } => {
                last_action = action.clone();
                state = compute(&action, &from_mirror, &State::new())?;
                t.send(&encode_report_state(&state))?;
            }
            MirrorMessage::NextStep { action, parameters } => {
                last_action = action.clone();
                let next = compute(&action, &parameters, &state)?;
                last_param = parameters;
                state = next;
                t.send(&encode_report_state(&state))?;
            }
            MirrorMessage::StepOk => {}
            MirrorMessage::AllStepsDone => return Ok(()),
            MirrorMessage::StepMismatch {
                action,
                expected,
                actual,
                hints,
            } => {
                return Err(Error::StepMismatch {
                    action: action.unwrap_or(last_action),
                    params: last_param,
                    expected,
                    actual,
                    hints,
                })
            }
            MirrorMessage::ProtocolError { error } => return Err(Error::ProtocolError(error)),
            MirrorMessage::RegisterError { error } => return Err(Error::RegisterFailed(error)),
            other => {
                return Err(Error::UnexpectedMessage(format!(
                    "unexpected message: {other:?}"
                )))
            }
        }
    }
}

fn gen_traces_loop(mut t: Transport) -> Result<GenTracesResult, Error> {
    let result = run_gen_traces_loop(&mut t);
    let _ = t.close();
    result
}

fn run_gen_traces_loop(t: &mut Transport) -> Result<GenTracesResult, Error> {
    match recv(t)? {
        MirrorMessage::GenTracesDone {
            itf_trace_paths,
            itf_traces,
        } => Ok(GenTracesResult {
            itf_trace_paths,
            itf_traces,
        }),
        MirrorMessage::ProtocolError { error } => Err(Error::ProtocolError(error)),
        MirrorMessage::RegisterError { error } => Err(Error::RegisterFailed(error)),
        other => Err(Error::UnexpectedMessage(format!(
            "expected gen_traces_done, got {other:?}"
        ))),
    }
}
