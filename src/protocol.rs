use num_bigint::BigInt;
use num_traits::ToPrimitive;
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
