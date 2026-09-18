use num_bigint::BigInt;
use num_traits::ToPrimitive;
use serde::de::Error as _;
use serde::ser::{Serialize, Serializer};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value as Json};
use std::collections::BTreeMap;

/// Mirrors the Haskell Apalache.Types.Value (tagged representation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Int(BigInt),
    Bool(bool),
    Str(String),
    Set(Vec<Value>),
    Seq(Vec<Value>),
    Tuple(Vec<Value>),
    Map(Vec<(Value, Value)>),
    Record(State),
    Variant(String, Box<Value>),
    Unserializable(String),
    Null,
}

/// A state map: field name -> tagged value.
pub type State = BTreeMap<String, Value>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSegment {
    Field(String),
    Index(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffHint {
    ValueMismatch {
        path: Vec<PathSegment>,
        expected: Value,
        actual: Value,
    },
    Missing {
        path: Vec<PathSegment>,
        expected: Value,
    },
    Extra {
        path: Vec<PathSegment>,
        actual: Value,
    },
    MissingElem {
        path: Vec<PathSegment>,
        expected: Value,
    },
    ExtraElem {
        path: Vec<PathSegment>,
        actual: Value,
    },
    TypeMismatch {
        path: Vec<PathSegment>,
        expected: Value,
        actual: Value,
    },
    Truncated {
        path: Vec<PathSegment>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ApalacheSpec {
    pub sources: Vec<String>,
}

/// Encode a Value to the clean ITF JSON form (used on the wire by report_state).
fn encode_value(v: &Value) -> Json {
    match v {
        Value::Int(n) => json!({ "#bigint": n.to_string() }),
        Value::Bool(b) => json!(b),
        Value::Str(s) => json!(s),
        Value::Set(items) => json!({ "#set": items.iter().map(encode_value).collect::<Vec<_>>() }),
        Value::Seq(items) => Json::Array(items.iter().map(encode_value).collect()),
        Value::Tuple(items) => {
            json!({ "#tup": items.iter().map(encode_value).collect::<Vec<_>>() })
        }
        Value::Map(entries) => {
            let pairs: Vec<Json> = entries
                .iter()
                .map(|(k, v)| Json::Array(vec![encode_value(k), encode_value(v)]))
                .collect();
            json!({ "#map": pairs })
        }
        Value::Record(rec) => {
            let mut m = serde_json::Map::new();
            for (k, iv) in rec {
                m.insert(k.clone(), encode_value(iv));
            }
            Json::Object(m)
        }
        Value::Variant(tag, val) => json!({ "tag": tag, "value": encode_value(val) }),
        Value::Unserializable(s) => json!({ "#unserializable": s }),
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
        Value::Int(n) => n
            .to_i64()
            .map_or_else(|| json!(n.to_string()), |i| json!(i)),
        Value::Bool(b) => json!(b),
        Value::Str(s) => json!(s),
        Value::Set(items) => Json::Array(items.iter().map(prettify_value).collect()),
        Value::Seq(items) => Json::Array(items.iter().map(prettify_value).collect()),
        Value::Tuple(items) => Json::Array(items.iter().map(prettify_value).collect()),
        Value::Map(entries) => Json::Array(
            entries
                .iter()
                .map(|(k, v)| Json::Array(vec![prettify_value(k), prettify_value(v)]))
                .collect(),
        ),
        Value::Record(rec) => {
            let mut m = serde_json::Map::new();
            for (k, iv) in rec {
                m.insert(k.clone(), prettify_value(iv));
            }
            Json::Object(m)
        }
        Value::Variant(tag, val) => json!({ "tag": tag, "value": prettify_value(val) }),
        Value::Unserializable(s) => json!(s),
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceGenerationConfig {
    pub num_traces: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "proto_step", rename_all = "snake_case")]
pub enum ClientMessage {
    Register {
        #[serde(rename = "apalacheConfig")]
        apalache_config: ApalacheConfig,
        #[serde(rename = "traceConfig")]
        trace_config: TraceGenerationConfig,
        #[serde(skip_serializing_if = "Option::is_none")]
        spec: Option<ApalacheSpec>,
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
        dest_path: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        spec: Option<ApalacheSpec>,
    },
    RegisterValidate {
        #[serde(rename = "apalacheConfig")]
        apalache_config: ApalacheConfig,
        bound: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        spec: Option<ApalacheSpec>,
    },
    RegisterValidateAsync {
        #[serde(rename = "apalacheConfig")]
        apalache_config: ApalacheConfig,
        bound: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        spec: Option<ApalacheSpec>,
    },
    RegisterTraceGenAsync {
        #[serde(rename = "apalacheConfig")]
        apalache_config: ApalacheConfig,
        #[serde(rename = "traceConfig")]
        trace_config: TraceGenerationConfig,
        #[serde(rename = "destPath", skip_serializing_if = "Option::is_none")]
        dest_path: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        spec: Option<ApalacheSpec>,
    },
    QueryJob {
        #[serde(rename = "jobId")]
        job_id: String,
    },
    AwaitJob {
        #[serde(rename = "jobId")]
        job_id: String,
        #[serde(rename = "timeoutSecs", skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
    },
    CancelJob {
        #[serde(rename = "jobId")]
        job_id: String,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Validate,
    GenTraces,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobPhase {
    Pending,
    Running,
    Done,
    Failed,
    Cancelled,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobOutcome {
    Validate(SpecResult),
    GenTraces {
        itf_trace_paths: Vec<String>,
        itf_traces: Vec<Json>,
    },
    InfraError(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirrorMessage {
    SpecValidated {
        result: SpecResult,
    },
    InitialState {
        action: String,
        state: State,
    },
    NextStep {
        action: String,
        parameters: State,
    },
    StepOk,
    StepMismatch {
        action: Option<String>,
        expected: State,
        actual: State,
        hints: Vec<DiffHint>,
    },
    AllStepsDone,
    GenTracesDone {
        itf_trace_paths: Vec<String>,
        itf_traces: Vec<Json>,
    },
    ProtocolError {
        error: String,
    },
    RegisterError {
        error: String,
    },
    JobAccepted {
        job_id: String,
        kind: JobKind,
    },
    JobStatus {
        job_id: String,
        phase: JobPhase,
    },
    JobResult {
        job_id: String,
        outcome: JobOutcome,
    },
}

/// Serialize a Value directly in the clean ITF wire representation.
impl Serialize for Value {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        encode_value(self).serialize(s)
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Json::deserialize(deserializer)?;
        walk(&value).map_err(D::Error::custom)
    }
}

pub fn encode_client_message(msg: &ClientMessage) -> String {
    serde_json::to_string(msg).expect("ClientMessage serialization cannot fail")
}

/// Decode arbitrary ITF JSON into a Value (faithful to the TS `walk`).
fn invalid_value(message: impl Into<String>) -> crate::Error {
    crate::Error::ProtocolError(format!("invalid ITF value: {}", message.into()))
}

fn walk(v: &Json) -> Result<Value, crate::Error> {
    match v {
        Json::Null => Ok(Value::Null),
        Json::Bool(b) => Ok(Value::Bool(*b)),
        Json::String(s) => Ok(Value::Str(s.clone())),
        Json::Number(n) => Ok(Value::Int(number_to_bigint(n)?)),
        Json::Array(items) => Ok(Value::Seq(
            items.iter().map(walk).collect::<Result<_, _>>()?,
        )),
        Json::Object(obj) => {
            if let Some(raw) = obj.get("#bigint") {
                let Json::String(raw) = raw else {
                    return Err(invalid_value("#bigint must contain a decimal string"));
                };
                if raw.is_empty() {
                    return Ok(Value::Null);
                }
                let parsed = raw
                    .parse::<BigInt>()
                    .map_err(|_| invalid_value(format!("malformed #bigint {raw:?}")))?;
                return Ok(Value::Int(parsed));
            }
            if let Some(raw) = obj.get("#tup") {
                let Json::Array(items) = raw else {
                    return Err(invalid_value("#tup must contain an array"));
                };
                return Ok(Value::Tuple(
                    items.iter().map(walk).collect::<Result<_, _>>()?,
                ));
            }
            if let Some(raw) = obj.get("#set") {
                let Json::Array(items) = raw else {
                    return Err(invalid_value("#set must contain an array"));
                };
                return Ok(Value::Set(
                    items.iter().map(walk).collect::<Result<_, _>>()?,
                ));
            }
            if let Some(raw) = obj.get("#map") {
                let Json::Array(entries) = raw else {
                    return Err(invalid_value("#map must contain an array"));
                };
                let mut pairs = Vec::with_capacity(entries.len());
                for entry in entries {
                    let Json::Array(pair) = entry else {
                        return Err(invalid_value("#map entry must be a two-element array"));
                    };
                    if pair.len() != 2 {
                        return Err(invalid_value("#map entry must have exactly two elements"));
                    }
                    pairs.push((walk(&pair[0])?, walk(&pair[1])?));
                }
                return Ok(Value::Map(pairs));
            }
            if let Some(raw) = obj.get("#unserializable") {
                let Json::String(raw) = raw else {
                    return Err(invalid_value("#unserializable must contain a string"));
                };
                return Ok(Value::Unserializable(raw.clone()));
            }
            if obj.len() == 2 && obj.contains_key("tag") && obj.contains_key("value") {
                let Some(Json::String(tag)) = obj.get("tag") else {
                    return Err(invalid_value("variant tag must be a string"));
                };
                return Ok(Value::Variant(tag.clone(), Box::new(walk(&obj["value"])?)));
            }
            let mut rec = State::new();
            for (k, iv) in obj {
                rec.insert(k.clone(), walk(iv)?);
            }
            Ok(Value::Record(rec))
        }
    }
}

fn number_to_bigint(n: &serde_json::Number) -> Result<BigInt, crate::Error> {
    let text = n.to_string();
    let (negative, unsigned) = text
        .strip_prefix('-')
        .map_or((false, text.as_str()), |rest| (true, rest));
    let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((m, e)) => (
            m,
            e.parse::<i64>()
                .map_err(|_| invalid_value("bad number exponent"))?,
        ),
        None => (unsigned, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut digits = format!("{whole}{fraction}");
    let scale = exponent - i64::try_from(fraction.len()).unwrap_or(i64::MAX);
    if scale >= 0 {
        digits.extend(std::iter::repeat_n('0', scale as usize));
    } else {
        let remove = (-scale) as usize;
        if remove > digits.len() || !digits[digits.len() - remove..].bytes().all(|b| b == b'0') {
            return Err(invalid_value(
                "bare JSON number not in ITF integral grammar",
            ));
        }
        digits.truncate(digits.len() - remove);
        if digits.is_empty() {
            digits.push('0');
        }
    }
    let signed = if negative {
        format!("-{digits}")
    } else {
        digits
    };
    signed
        .parse::<BigInt>()
        .map_err(|_| invalid_value("malformed bare JSON integer"))
}

/// Extract a State from a JSON object field via `walk`.
fn walk_record(v: Option<&Json>, field: &str) -> Result<State, crate::Error> {
    match v {
        Some(v) => match walk(v)? {
            Value::Record(r) => Ok(r),
            _ => Err(invalid_value(format!("{field} must be an object"))),
        },
        None => Err(invalid_value(format!("missing {field}"))),
    }
}

fn str_field(obj: &serde_json::Map<String, Json>, key: &str) -> Result<String, crate::Error> {
    obj.get(key)
        .and_then(Json::as_str)
        .map(String::from)
        .ok_or_else(|| invalid_value(format!("{key} must be a string")))
}

fn path_segments(value: Option<&Json>) -> Result<Vec<PathSegment>, crate::Error> {
    let Some(Json::Array(parts)) = value else {
        return Err(invalid_value("hint path must be an array"));
    };
    parts
        .iter()
        .map(|part| match part {
            Json::Object(obj) if obj.len() == 1 => {
                if let Some(Json::String(field)) = obj.get("field") {
                    Ok(PathSegment::Field(field.clone()))
                } else if let Some(index) = obj.get("index").and_then(Json::as_u64) {
                    Ok(PathSegment::Index(index))
                } else {
                    Err(invalid_value("invalid hint path segment"))
                }
            }
            _ => Err(invalid_value("invalid hint path segment")),
        })
        .collect()
}

fn hint_value(obj: &serde_json::Map<String, Json>, key: &str) -> Result<Value, crate::Error> {
    walk(
        obj.get(key)
            .ok_or_else(|| invalid_value(format!("hint missing {key}")))?,
    )
}

fn decode_hint(value: &Json) -> Result<DiffHint, crate::Error> {
    let Json::Object(obj) = value else {
        return Err(invalid_value("hint must be an object"));
    };
    let kind = obj
        .get("kind")
        .and_then(Json::as_str)
        .ok_or_else(|| invalid_value("hint kind must be a string"))?;
    let path = path_segments(obj.get("path"))?;
    match kind {
        "value_mismatch" => Ok(DiffHint::ValueMismatch {
            path,
            expected: hint_value(obj, "expected")?,
            actual: hint_value(obj, "actual")?,
        }),
        "missing" => Ok(DiffHint::Missing {
            path,
            expected: hint_value(obj, "expected")?,
        }),
        "extra" => Ok(DiffHint::Extra {
            path,
            actual: hint_value(obj, "actual")?,
        }),
        "missing_elem" => Ok(DiffHint::MissingElem {
            path,
            expected: hint_value(obj, "expected")?,
        }),
        "extra_elem" => Ok(DiffHint::ExtraElem {
            path,
            actual: hint_value(obj, "actual")?,
        }),
        "type_mismatch" => Ok(DiffHint::TypeMismatch {
            path,
            expected: hint_value(obj, "expected")?,
            actual: hint_value(obj, "actual")?,
        }),
        "truncated" => Ok(DiffHint::Truncated { path }),
        _ => Err(invalid_value(format!("unknown hint kind {kind:?}"))),
    }
}

fn decode_spec_result(value: Option<&Json>) -> Result<SpecResult, crate::Error> {
    match value {
        Some(Json::String(s)) if s == "valid" => Ok(SpecResult::Valid),
        Some(Json::Object(result)) => {
            let detail = result
                .get("invalid")
                .and_then(Json::as_str)
                .ok_or_else(|| invalid_value("invalid spec result must contain a string"))?;
            Ok(SpecResult::Invalid(detail.to_string()))
        }
        _ => Err(invalid_value("spec result has an invalid shape")),
    }
}

fn string_array(value: Option<&Json>, field: &str) -> Result<Vec<String>, crate::Error> {
    value
        .and_then(Json::as_array)
        .ok_or_else(|| invalid_value(format!("{field} must be an array")))?
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(String::from)
                .ok_or_else(|| invalid_value(format!("{field} entries must be strings")))
        })
        .collect()
}

fn decode_job_outcome(value: Option<&Json>) -> Result<JobOutcome, crate::Error> {
    let outcome = value
        .and_then(Json::as_object)
        .ok_or_else(|| invalid_value("job_result.outcome must be an object"))?;
    if let Some(validate) = outcome.get("validate") {
        return Ok(JobOutcome::Validate(decode_spec_result(Some(validate))?));
    }
    if let Some(gen) = outcome.get("genTraces").and_then(Json::as_object) {
        let itf_trace_paths = string_array(gen.get("itfTracePaths"), "itfTracePaths")?;
        let itf_traces = match gen.get("itfTraces") {
            None => Vec::new(),
            Some(Json::Array(traces)) => traces.clone(),
            Some(_) => return Err(invalid_value("itfTraces must be an array")),
        };
        return Ok(JobOutcome::GenTraces {
            itf_trace_paths,
            itf_traces,
        });
    }
    if let Some(error) = outcome.get("error").and_then(Json::as_str) {
        return Ok(JobOutcome::InfraError(error.to_string()));
    }
    Err(invalid_value("unknown job_result outcome"))
}

fn walk_message(obj: &serde_json::Map<String, Json>) -> Result<MirrorMessage, crate::Error> {
    let step = obj
        .get("proto_step")
        .and_then(Json::as_str)
        .ok_or_else(|| invalid_value("proto_step must be a string"))?;
    Ok(match step {
        "spec_validated" => {
            let result = decode_spec_result(obj.get("result"))?;
            MirrorMessage::SpecValidated { result }
        }
        "initial_state" => MirrorMessage::InitialState {
            action: str_field(obj, "action")?,
            state: walk_record(obj.get("state"), "state")?,
        },
        "next_step" => MirrorMessage::NextStep {
            action: str_field(obj, "action")?,
            parameters: walk_record(obj.get("parameters"), "parameters")?,
        },
        "step_ok" => MirrorMessage::StepOk,
        "step_mismatch" => MirrorMessage::StepMismatch {
            action: obj.get("action").and_then(|v| v.as_str()).map(String::from),
            expected: walk_record(obj.get("expected"), "expected")?,
            actual: walk_record(obj.get("actual"), "actual")?,
            hints: match obj.get("hints") {
                None => Vec::new(),
                Some(Json::Array(hints)) => {
                    hints.iter().map(decode_hint).collect::<Result<_, _>>()?
                }
                Some(_) => return Err(invalid_value("hints must be an array")),
            },
        },
        "all_steps_done" => MirrorMessage::AllStepsDone,
        "gen_traces_done" => MirrorMessage::GenTracesDone {
            itf_trace_paths: string_array(obj.get("itfTracePaths"), "itfTracePaths")?,
            itf_traces: match obj.get("itfTraces") {
                None => Vec::new(),
                Some(Json::Array(traces)) => traces.clone(),
                Some(_) => return Err(invalid_value("itfTraces must be an array")),
            },
        },
        "protocol_error" => MirrorMessage::ProtocolError {
            error: str_field(obj, "error")?,
        },
        "register_error" => MirrorMessage::RegisterError {
            error: str_field(obj, "error")?,
        },
        "job_accepted" => MirrorMessage::JobAccepted {
            job_id: str_field(obj, "jobId")?,
            kind: match obj.get("kind").and_then(Json::as_str) {
                Some("validate") => JobKind::Validate,
                Some("gen_traces") => JobKind::GenTraces,
                _ => return Err(invalid_value("job_accepted.kind has an invalid value")),
            },
        },
        "job_status" => MirrorMessage::JobStatus {
            job_id: str_field(obj, "jobId")?,
            phase: match obj.get("phase").and_then(Json::as_str) {
                Some("pending") => JobPhase::Pending,
                Some("running") => JobPhase::Running,
                Some("done") => JobPhase::Done,
                Some("failed") => JobPhase::Failed,
                Some("cancelled") => JobPhase::Cancelled,
                Some("unknown") => JobPhase::Unknown,
                _ => return Err(invalid_value("job_status.phase has an invalid value")),
            },
        },
        "job_result" => MirrorMessage::JobResult {
            job_id: str_field(obj, "jobId")?,
            outcome: decode_job_outcome(obj.get("outcome"))?,
        },
        other => MirrorMessage::ProtocolError {
            error: format!("unknown proto_step: {other}"),
        },
    })
}

pub fn decode_mirror_message(line: &str) -> Result<MirrorMessage, crate::Error> {
    let raw = crate::json::parse(
        line,
        crate::json::Limits {
            max_bytes: usize::MAX,
            max_depth: 128,
            max_nodes: 16_384,
            reject_duplicate_keys: false,
        },
    )
    .map_err(|error| crate::Error::ProtocolError(format!("invalid JSON: {error}")))?;
    match raw {
        Json::Object(obj) => walk_message(&obj),
        _ => Ok(MirrorMessage::ProtocolError {
            error: "expected a JSON object".to_string(),
        }),
    }
}
