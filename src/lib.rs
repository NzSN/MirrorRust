pub mod client;
pub mod protocol;
pub mod transport;

use protocol::prettify_json;

pub use protocol::{
    as_int, as_record, as_str, decode_mirror_message, encode_client_message, encode_state,
    get_param, get_param_int, ApalacheConfig, ClientMessage, MirrorMessage, SpecResult, State,
    TraceGenerationConfig, Value,
};

pub use transport::{spawn_mirror, Transport};

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
    },
    #[error("unexpected message: {0}")]
    UnexpectedMessage(String),
    #[error("transport closed unexpectedly")]
    TransportClosed,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
