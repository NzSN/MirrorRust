use num_bigint::BigInt;
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
