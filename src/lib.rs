pub mod protocol;

pub use protocol::{
    as_int, as_record, as_str, encode_client_message, encode_state, get_param, get_param_int,
    ApalacheConfig, ClientMessage, MirrorMessage, SpecResult, State, TraceGenerationConfig, Value,
};
