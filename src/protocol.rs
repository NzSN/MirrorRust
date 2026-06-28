use num_bigint::BigInt;
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
