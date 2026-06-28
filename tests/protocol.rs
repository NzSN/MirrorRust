use mirrorrust::{encode_state, State, Value};
use num_bigint::BigInt;
use serde_json::json;

fn st(pairs: Vec<(&str, Value)>) -> State {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

#[test]
fn encode_state_clean_itf() {
    let s = st(vec![
        ("count", Value::Int(BigInt::from(9007199254740991i64))),
        ("flag", Value::Bool(true)),
    ]);
    let encoded = encode_state(&s);
    assert_eq!(encoded["count"], json!({ "#bigint": "9007199254740991" }));
    assert_eq!(encoded["flag"], json!(true));
}

#[test]
fn encode_state_set_tuple_record_null() {
    let s = st(vec![
        ("items", Value::Set(vec![Value::Int(BigInt::from(1)), Value::Int(BigInt::from(2))])),
        ("pair", Value::Tuple(vec![Value::Str("foo".into()), Value::Int(BigInt::from(7))])),
        ("nothing", Value::Null),
        ("person", Value::Record(st(vec![("name", Value::Str("bob".into()))]))),
    ]);
    let e = encode_state(&s);
    assert_eq!(e["items"], json!({ "#set": [{ "#bigint": "1" }, { "#bigint": "2" }] }));
    assert_eq!(e["pair"], json!({ "#tup": ["foo", { "#bigint": "7" }] }));
    assert_eq!(e["nothing"], serde_json::Value::Null);
    assert_eq!(e["person"], json!({ "name": "bob" }));
}
