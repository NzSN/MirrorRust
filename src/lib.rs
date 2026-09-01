pub mod client;
pub mod protocol;
pub mod registry;
pub mod spec;
pub mod transport;

use protocol::prettify_json;

pub use protocol::{
    as_int, as_record, as_str, decode_mirror_message, encode_client_message, encode_state,
    get_param, get_param_int, ApalacheConfig, ApalacheSpec, ClientMessage, DiffHint, JobKind,
    JobOutcome, JobPhase, MirrorMessage, PathSegment, SpecResult, State, TraceGenerationConfig,
    Value,
};

pub use client::{
    await_job, cancel_job, preset_client, query_job, run_client, run_client_gen_traces,
    run_client_gen_traces_transport, run_client_gen_traces_with_inline_spec, run_client_validate,
    run_client_validate_transport, run_client_with_inline_spec, run_client_with_traces,
    run_client_with_traces_transport, run_client_with_transport, submit_trace_gen_async,
    submit_validate_async, GenTracesResult, JobAccepted, JobReply, PresetClient, StateComputer,
};

pub use registry::{connect_mirror_from_registry, discover_mirrors, MirrorServiceInfo};
pub use spec::{spec_from_file, spec_from_files};
pub use transport::{
    connect_mirror, connect_tls_mirror, spawn_mirror, validate_protocol_line, TlsOptions,
    Transport, MAX_PROTOCOL_LINE_BYTES,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("spec invalid: {0}")]
    SpecInvalid(String),
    #[error("{0}")]
    ProtocolError(String),
    #[error("register failed: {0}")]
    RegisterFailed(String),
    #[error("step mismatch on action \"{action}\": expected {}, got {}",
            prettify_json(.expected), prettify_json(.actual))]
    StepMismatch {
        action: String,
        params: State,
        expected: State,
        actual: State,
        hints: Vec<DiffHint>,
    },
    #[error("unexpected message: {0}")]
    UnexpectedMessage(String),
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    #[error("spec source: {0}")]
    SpecSource(String),
    #[error("transport closed unexpectedly")]
    TransportClosed,
    #[error("TLS: {0}")]
    Tls(String),
    #[error("registry: {0}")]
    Registry(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
