use num_bigint::BigInt;
use num_traits::ToPrimitive;
use serde::ser::{Serialize, SerializeMap, Serializer};
use serde_json::{json, Value as Json};
use std::collections::BTreeMap;

/// Mirrors the Haskell Apalache.Types.Value (tagged representation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Int(BigInt),
    Bool(bool),
    Str(String),
    Set(Vec<Value>),
    Tuple(Vec<Value>),
    Record(State),
    Null,
}

/// A state map: field name -> tagged value.
pub type State = BTreeMap<String, Value>;

/// Encode a Value to the clean ITF JSON form (used on the wire by report_state).
fn encode_value(v: &Value) -> Json {
    match v {
        Value::Int(n) => json!({ "#bigint": n.to_string() }),
        Value::Bool(b) => json!(b),
        Value::Str(s) => json!(s),
        Value::Set(items) => json!({ "#set": items.iter().map(encode_value).collect::<Vec<_>>() }),
        Value::Tuple(items) => json!({ "#tup": items.iter().map(encode_value).collect::<Vec<_>>() }),
        Value::Record(rec) => {
            let mut m = serde_json::Map::new();
            for (k, iv) in rec {
                m.insert(k.clone(), encode_value(iv));
            }
            Json::Object(m)
        }
        Value::Null => Json::Null,
    }
}

/// Encode a State to a clean ITF JSON object.
pub fn encode_state(state: &State) -> Json {
    let mut m = serde_json::Map::new();
    for (k, v) in state {
        m.insert(k.clone(), encode_value(v));
    }
    Json::Object(m)
}

pub fn as_int(v: &Value) -> Option<&BigInt> {
    match v {
        Value::Int(n) => Some(n),
        _ => None,
    }
}

pub fn as_str(v: &Value) -> Option<&str> {
    match v {
        Value::Str(s) => Some(s),
        _ => None,
    }
}

pub fn as_record(v: &Value) -> Option<&State> {
    match v {
        Value::Record(r) => Some(r),
        _ => None,
    }
}

pub fn get_param<'a>(params: &'a State, var_name: &str) -> Option<&'a State> {
    match params.get(var_name) {
        Some(Value::Record(r)) => Some(r),
        _ => None,
    }
}

pub fn get_param_int(params: &State, var_name: &str, field: &str) -> i64 {
    match get_param(params, var_name).and_then(|r| r.get(field)) {
        Some(Value::Int(n)) => n.to_i64().unwrap_or(0),
        _ => 0,
    }
}

/// Human-readable rendering for error messages (ints become JSON numbers).
pub(crate) fn prettify_value(v: &Value) -> Json {
    match v {
        Value::Int(n) => json!(n.to_i64().unwrap_or(0)),
        Value::Bool(b) => json!(b),
        Value::Str(s) => json!(s),
        Value::Set(items) => Json::Array(items.iter().map(prettify_value).collect()),
        Value::Tuple(items) => Json::Array(items.iter().map(prettify_value).collect()),
        Value::Record(rec) => {
            let mut m = serde_json::Map::new();
            for (k, iv) in rec {
                m.insert(k.clone(), prettify_value(iv));
            }
            Json::Object(m)
        }
        Value::Null => Json::Null,
    }
}

pub(crate) fn prettify_state(state: &State) -> Json {
    let mut m = serde_json::Map::new();
    for (k, v) in state {
        m.insert(k.clone(), prettify_value(v));
    }
    Json::Object(m)
}

pub(crate) fn prettify_json(state: &State) -> String {
    serde_json::to_string(&prettify_state(state)).unwrap_or_default()
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApalacheConfig {
    pub spec_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub init_predicate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_predicate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub const_init: Option<String>,
    pub invariant: String,
    pub length_bound: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param_vars: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceGenerationConfig {
    pub num_traces: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "proto_step", rename_all = "snake_case")]
pub enum ClientMessage {
    Register {
        #[serde(rename = "apalacheConfig")]
        apalache_config: ApalacheConfig,
        #[serde(rename = "traceConfig")]
        trace_config: TraceGenerationConfig,
    },
    RegisterTraces {
        #[serde(rename = "apalacheConfig")]
        apalache_config: ApalacheConfig,
        #[serde(rename = "itfTracePaths")]
        itf_trace_paths: Vec<String>,
    },
    RegisterTraceGen {
        #[serde(rename = "apalacheConfig")]
        apalache_config: ApalacheConfig,
        #[serde(rename = "traceConfig")]
        trace_config: TraceGenerationConfig,
        #[serde(rename = "destPath")]
        dest_path: String,
    },
    ReportState {
        state: State,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecResult {
    Valid,
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirrorMessage {
    SpecValidated { result: SpecResult },
    InitialState { action: String, state: State },
    NextStep { action: String, parameters: State },
    StepOk,
    StepMismatch { action: Option<String>, expected: State, actual: State },
    AllStepsDone,
    GenTracesDone { itf_trace_paths: Vec<String> },
    ProtocolError { error: String },
    RegisterError { error: String },
}

/// Serialize a Value in the *tagged* form. Used only by encode_client_message
/// for the ReportState variant; matches the TS JSON.stringify of the tagged
/// Value with the bigint replacer.
impl Serialize for Value {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Value::Int(n) => {
                let mut m = s.serialize_map(Some(2))?;
                m.serialize_entry("tag", "int")?;
                m.serialize_entry("val", &json!({ "#bigint": n.to_string() }))?;
                m.end()
            }
            Value::Bool(b) => {
                let mut m = s.serialize_map(Some(2))?;
                m.serialize_entry("tag", "bool")?;
                m.serialize_entry("val", b)?;
                m.end()
            }
            Value::Str(v) => {
                let mut m = s.serialize_map(Some(2))?;
                m.serialize_entry("tag", "str")?;
                m.serialize_entry("val", v)?;
                m.end()
            }
            Value::Set(items) => {
                let mut m = s.serialize_map(Some(2))?;
                m.serialize_entry("tag", "set")?;
                m.serialize_entry("val", items)?;
                m.end()
            }
            Value::Tuple(items) => {
                let mut m = s.serialize_map(Some(2))?;
                m.serialize_entry("tag", "tuple")?;
                m.serialize_entry("val", items)?;
                m.end()
            }
            Value::Record(rec) => {
                let mut m = s.serialize_map(Some(2))?;
                m.serialize_entry("tag", "record")?;
                m.serialize_entry("val", rec)?;
                m.end()
            }
            Value::Null => {
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry("tag", "null")?;
                m.end()
            }
        }
    }
}

pub fn encode_client_message(msg: &ClientMessage) -> String {
    serde_json::to_string(msg).expect("ClientMessage serialization cannot fail")
}

/// Decode arbitrary ITF JSON into a Value (faithful to the TS `walk`).
fn walk(v: &Json) -> Value {
    match v {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::String(s) => Value::Str(s.clone()),
        Json::Number(n) => Value::Int(number_to_bigint(n)),
        Json::Array(items) => Value::Set(items.iter().map(walk).collect()),
        Json::Object(obj) => {
            if let Some(Json::String(s)) = obj.get("#bigint") {
                return Value::Int(s.parse::<BigInt>().unwrap_or_else(|_| BigInt::from(0)));
            }
            if let Some(Json::Array(items)) = obj.get("#tup") {
                return Value::Tuple(items.iter().map(walk).collect());
            }
            if let Some(Json::Array(items)) = obj.get("#set") {
                return Value::Set(items.iter().map(walk).collect());
            }
            let mut rec = State::new();
            for (k, iv) in obj {
                rec.insert(k.clone(), walk(iv));
            }
            Value::Record(rec)
        }
    }
}

fn number_to_bigint(n: &serde_json::Number) -> BigInt {
    if let Some(i) = n.as_i64() {
        BigInt::from(i)
    } else if let Some(u) = n.as_u64() {
        BigInt::from(u)
    } else {
        n.to_string().parse::<BigInt>().unwrap_or_else(|_| BigInt::from(0))
    }
}

/// Extract a State from a JSON object field via `walk`.
fn walk_record(v: Option<&Json>) -> State {
    match v.map(walk) {
        Some(Value::Record(r)) => r,
        _ => State::new(),
    }
}

fn str_field(obj: &serde_json::Map<String, Json>, key: &str) -> String {
    obj.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn walk_message(obj: &serde_json::Map<String, Json>) -> MirrorMessage {
    let step = obj.get("proto_step").and_then(|v| v.as_str()).unwrap_or("");
    match step {
        "spec_validated" => {
            let result = match obj.get("result") {
                Some(Json::String(s)) if s == "valid" => SpecResult::Valid,
                Some(Json::String(s)) => SpecResult::Invalid(s.clone()),
                Some(other) => SpecResult::Invalid(serde_json::to_string(other).unwrap_or_default()),
                None => SpecResult::Invalid("null".to_string()),
            };
            MirrorMessage::SpecValidated { result }
        }
        "initial_state" => MirrorMessage::InitialState {
            action: str_field(obj, "action"),
            state: walk_record(obj.get("state")),
        },
        "next_step" => MirrorMessage::NextStep {
            action: str_field(obj, "action"),
            parameters: walk_record(obj.get("parameters")),
        },
        "step_ok" => MirrorMessage::StepOk,
        "step_mismatch" => MirrorMessage::StepMismatch {
            action: obj.get("action").and_then(|v| v.as_str()).map(String::from),
            expected: walk_record(obj.get("expected")),
            actual: walk_record(obj.get("actual")),
        },
        "all_steps_done" => MirrorMessage::AllStepsDone,
        "gen_traces_done" => MirrorMessage::GenTracesDone {
            itf_trace_paths: obj
                .get("itfTracePaths")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default(),
        },
        "protocol_error" => MirrorMessage::ProtocolError { error: str_field(obj, "error") },
        "register_error" => MirrorMessage::RegisterError { error: str_field(obj, "error") },
        other => MirrorMessage::ProtocolError { error: format!("unknown proto_step: {other}") },
    }
}

pub fn decode_mirror_message(line: &str) -> Result<MirrorMessage, crate::Error> {
    let raw: Json = serde_json::from_str(line)?;
    match raw {
        Json::Object(obj) => Ok(walk_message(&obj)),
        _ => Ok(MirrorMessage::ProtocolError { error: "expected a JSON object".to_string() }),
    }
}
