use crate::protocol::{
    decode_mirror_message, encode_client_message, encode_state, ApalacheConfig, ClientMessage,
    MirrorMessage, SpecResult, State, TraceGenerationConfig,
};
use crate::transport::{spawn_mirror, Transport};
use crate::Error;
use serde_json::json;

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
    let mut t = spawn_mirror(bin_path)?;
    t.send(&encode_client_message(&ClientMessage::Register {
        apalache_config,
        trace_config,
    }))?;
    main_loop(t, compute)
}

pub fn run_client_with_traces(
    bin_path: &str,
    apalache_config: ApalacheConfig,
    trace_paths: Vec<String>,
    compute: impl StateComputer,
) -> Result<(), Error> {
    let mut t = spawn_mirror(bin_path)?;
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
) -> Result<(), Error> {
    let mut t = spawn_mirror(bin_path)?;
    t.send(&encode_client_message(&ClientMessage::RegisterTraceGen {
        apalache_config,
        trace_config,
        dest_path: dest_path.to_string(),
    }))?;
    gen_traces_loop(t)
}

fn recv(t: &mut Transport) -> Result<MirrorMessage, Error> {
    match t.recv()? {
        Some(line) => decode_mirror_message(&line),
        None => Err(Error::TransportClosed),
    }
}

fn encode_report_state(state: &State) -> String {
    serde_json::to_string(&json!({
        "proto_step": "report_state",
        "state": encode_state(state),
    }))
    .expect("report_state serialization cannot fail")
}

fn main_loop(mut t: Transport, mut compute: impl StateComputer) -> Result<(), Error> {
    let result = run_main_loop(&mut t, &mut compute);
    let _ = t.close();
    result
}

fn run_main_loop(t: &mut Transport, compute: &mut impl StateComputer) -> Result<(), Error> {
    match recv(t)? {
        MirrorMessage::SpecValidated { result: SpecResult::Valid } => {}
        MirrorMessage::SpecValidated { result: SpecResult::Invalid(s) } => {
            return Err(Error::SpecInvalid(s))
        }
        MirrorMessage::ProtocolError { error } => return Err(Error::ProtocolError(error)),
        MirrorMessage::RegisterError { error } => return Err(Error::RegisterFailed(error)),
        other => {
            return Err(Error::UnexpectedMessage(format!(
                "expected spec_validated, got {other:?}"
            )))
        }
    }

    let mut state: State = State::new();
    let mut last_param: State = State::new();
    let mut last_action = String::new();

    loop {
        match recv(t)? {
            MirrorMessage::InitialState { action, state: from_mirror } => {
                last_action = action.clone();
                state = compute.compute(&action, &from_mirror, &State::new());
                t.send(&encode_report_state(&state))?;
            }
            MirrorMessage::NextStep { action, parameters } => {
                last_action = action.clone();
                let next = compute.compute(&action, &parameters, &state);
                last_param = parameters;
                state = next;
                t.send(&encode_report_state(&state))?;
            }
            MirrorMessage::StepOk => {}
            MirrorMessage::AllStepsDone => return Ok(()),
            MirrorMessage::StepMismatch { action, expected, actual } => {
                return Err(Error::StepMismatch {
                    action: action.unwrap_or(last_action),
                    params: last_param,
                    expected,
                    actual,
                })
            }
            MirrorMessage::ProtocolError { error } => return Err(Error::ProtocolError(error)),
            MirrorMessage::RegisterError { error } => return Err(Error::RegisterFailed(error)),
            other => {
                return Err(Error::UnexpectedMessage(format!("unexpected message: {other:?}")))
            }
        }
    }
}

fn gen_traces_loop(mut t: Transport) -> Result<(), Error> {
    let result = run_gen_traces_loop(&mut t);
    let _ = t.close();
    result
}

fn run_gen_traces_loop(t: &mut Transport) -> Result<(), Error> {
    match recv(t)? {
        MirrorMessage::GenTracesDone { .. } => Ok(()),
        MirrorMessage::ProtocolError { error } => Err(Error::ProtocolError(error)),
        MirrorMessage::RegisterError { error } => Err(Error::RegisterFailed(error)),
        other => Err(Error::UnexpectedMessage(format!(
            "expected gen_traces_done, got {other:?}"
        ))),
    }
}
