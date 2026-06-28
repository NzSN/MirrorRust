# MirrorRust Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port the TypeScript `MirrorECMA` client (ModelMirros protocol) to a faithful, idiomatic, synchronous Rust library crate `mirrorrust`.

**Architecture:** A single library crate with three modules mirroring the TS source: `protocol` (Value/State types, ITF JSON encode/decode, helpers), `transport` (spawns a binary over stdio, blocking line I/O), `client` (lock-step request/response loop + entry points). All I/O is synchronous. Errors are a `thiserror`-derived enum. Arbitrary-precision integers use `num-bigint`.

**Tech Stack:** Rust 2021, `serde` + `serde_json`, `num-bigint` + `num-traits`, `thiserror`; dev-dep `tempfile`.

**Reference design:** `docs/superpowers/specs/2026-06-28-mirrorrust-port-design.md`. Original source to port: `../MirrorECMA/src/{protocol,client,transport,index}.ts` and tests `../MirrorECMA/test/{protocol,smoke}.test.ts`.

---

## Task 1: Crate scaffold + core `Value` / `State` types

**Files:**
- Create: `Cargo.toml`
- Create: `src/lib.rs`
- Create: `src/protocol.rs`

- [ ] **Step 1: Create `Cargo.toml`**

```toml
[package]
name = "mirrorrust"
version = "1.0.0"
edition = "2021"
description = "Rust client for the ModelMirros protocol"
license = "ISC"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
num-bigint = "0.4"
num-traits = "0.2"
thiserror = "1"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Create `src/protocol.rs` with core types**

```rust
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
```

- [ ] **Step 3: Create `src/lib.rs`**

```rust
pub mod protocol;

pub use protocol::{State, Value};
```

- [ ] **Step 4: Build**

Run: `cargo build`
Expected: compiles with no errors (warnings about unused `Value` variants are acceptable at this stage).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/protocol.rs
git commit -m "feat: scaffold mirrorrust crate with Value/State types"
```

---

## Task 2: Clean ITF encode (`encode_value`, `encode_state`)

These produce the *clean* ITF wire form used by the real `report_state` (ints become `{"#bigint":"N"}`, sets `{"#set":[...]}`, tuples `{"#tup":[...]}`).

**Files:**
- Modify: `src/protocol.rs`
- Modify: `src/lib.rs`
- Create: `tests/protocol.rs`

- [ ] **Step 1: Write the failing test in `tests/protocol.rs`**

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test protocol encode_state`
Expected: FAIL — `encode_state` not found in `mirrorrust`.

- [ ] **Step 3: Implement `encode_value` / `encode_state` in `src/protocol.rs`**

Add `use serde_json::{json, Value as Json};` to the top, then append:

```rust
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
```

- [ ] **Step 4: Re-export from `src/lib.rs`**

```rust
pub use protocol::{encode_state, State, Value};
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --test protocol encode_state`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add src/protocol.rs src/lib.rs tests/protocol.rs
git commit -m "feat: clean ITF encoding (encode_value/encode_state)"
```

---

## Task 3: Value helpers + prettify

Ports `asInt`/`asStr`/`asRecord`/`getParam`/`getParamInt` and the internal `prettifyState`/`prettifyValue`.

**Files:**
- Modify: `src/protocol.rs`
- Modify: `src/lib.rs`
- Modify: `tests/protocol.rs`

- [ ] **Step 1: Write the failing tests (append to `tests/protocol.rs`)**

```rust
use mirrorrust::{as_int, as_record, as_str, get_param, get_param_int};

#[test]
fn helper_as_int() {
    assert_eq!(as_int(&Value::Int(BigInt::from(42))), Some(&BigInt::from(42)));
    assert_eq!(as_int(&Value::Bool(true)), None);
    assert_eq!(as_int(&Value::Str("hi".into())), None);
    assert_eq!(as_int(&Value::Null), None);
}

#[test]
fn helper_as_str() {
    assert_eq!(as_str(&Value::Str("hello".into())), Some("hello"));
    assert_eq!(as_str(&Value::Int(BigInt::from(1))), None);
    assert_eq!(as_str(&Value::Bool(false)), None);
}

#[test]
fn helper_as_record() {
    let rec = st(vec![("a", Value::Int(BigInt::from(1)))]);
    assert_eq!(as_record(&Value::Record(rec.clone())), Some(&rec));
    assert_eq!(as_record(&Value::Int(BigInt::from(1))), None);
    assert_eq!(as_record(&Value::Null), None);
}

#[test]
fn helper_get_param_and_int() {
    let params = st(vec![
        ("x", Value::Record(st(vec![("foo", Value::Int(BigInt::from(42)))]))),
        ("y", Value::Int(BigInt::from(7))),
    ]);
    assert_eq!(
        get_param(&params, "x"),
        Some(&st(vec![("foo", Value::Int(BigInt::from(42)))]))
    );
    assert_eq!(get_param(&params, "y"), None);
    assert_eq!(get_param(&params, "missing"), None);
    assert_eq!(get_param_int(&params, "x", "foo"), 42);
    assert_eq!(get_param_int(&params, "missing", "foo"), 0);
    assert_eq!(get_param_int(&params, "x", "bar"), 0);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test protocol helper`
Expected: FAIL — helpers not found.

- [ ] **Step 3: Implement helpers + prettify in `src/protocol.rs`**

Add `use num_traits::ToPrimitive;` to the top, then append:

```rust
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
```

- [ ] **Step 4: Re-export helpers from `src/lib.rs`**

```rust
pub use protocol::{
    as_int, as_record, as_str, encode_state, get_param, get_param_int, State, Value,
};
```

- [ ] **Step 5: Run tests**

Run: `cargo test --test protocol helper`
Expected: PASS (4 tests).

- [ ] **Step 6: Commit**

```bash
git add src/protocol.rs src/lib.rs tests/protocol.rs
git commit -m "feat: value helpers (as_int/as_str/as_record/get_param) and prettify"
```

---

## Task 4: Message types, `Value` Serialize, `encode_client_message`

Defines the message structs/enums and the client-message encoder. Note the faithful quirk: `encode_client_message` over a `report_state` message produces the **tagged** form with `#bigint` (matching the TS `JSON.stringify` + bigint replacer over the tagged structure), which differs from `encode_state`'s clean form. To reproduce this, `Value` gets a `Serialize` impl producing the tagged form.

**Files:**
- Modify: `src/protocol.rs`
- Modify: `src/lib.rs`
- Modify: `tests/protocol.rs`

- [ ] **Step 1: Write the failing tests (append to `tests/protocol.rs`)**

```rust
use mirrorrust::{
    encode_client_message, ApalacheConfig, ClientMessage, TraceGenerationConfig,
};

fn cfg() -> ApalacheConfig {
    ApalacheConfig {
        spec_path: "/foo/bar.tla".into(),
        init_predicate: None,
        next_predicate: None,
        const_init: None,
        invariant: "TraceComplete".into(),
        length_bound: 5,
        param_vars: None,
    }
}

#[test]
fn encode_register() {
    let msg = ClientMessage::Register {
        apalache_config: cfg(),
        trace_config: TraceGenerationConfig { num_traces: 10, view: None },
    };
    let v: serde_json::Value = serde_json::from_str(&encode_client_message(&msg)).unwrap();
    assert_eq!(v["proto_step"], json!("register"));
    assert_eq!(v["apalacheConfig"]["specPath"], json!("/foo/bar.tla"));
    assert_eq!(v["apalacheConfig"]["invariant"], json!("TraceComplete"));
    assert_eq!(v["apalacheConfig"]["lengthBound"], json!(5));
    assert_eq!(v["traceConfig"]["numTraces"], json!(10));
    // optional fields omitted
    assert!(v["apalacheConfig"].get("initPredicate").is_none());
    assert!(v["apalacheConfig"].get("constInit").is_none());
    assert!(v["traceConfig"].get("view").is_none());
}

#[test]
fn encode_register_traces() {
    let msg = ClientMessage::RegisterTraces {
        apalache_config: cfg(),
        itf_trace_paths: vec!["/tmp/trace1.itf.json".into(), "/tmp/trace2.itf.json".into()],
    };
    let v: serde_json::Value = serde_json::from_str(&encode_client_message(&msg)).unwrap();
    assert_eq!(v["proto_step"], json!("register_traces"));
    assert_eq!(v["itfTracePaths"], json!(["/tmp/trace1.itf.json", "/tmp/trace2.itf.json"]));
}

#[test]
fn encode_report_state_is_tagged_with_bigint() {
    let s = st(vec![
        ("count", Value::Int(BigInt::parse_bytes(b"9007199254740991", 10).unwrap())),
        ("flag", Value::Bool(true)),
    ]);
    let msg = ClientMessage::ReportState { state: s };
    let v: serde_json::Value = serde_json::from_str(&encode_client_message(&msg)).unwrap();
    assert_eq!(
        v["state"]["count"],
        json!({ "tag": "int", "val": { "#bigint": "9007199254740991" } })
    );
    assert_eq!(v["state"]["flag"], json!({ "tag": "bool", "val": true }));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test protocol encode_register`
Expected: FAIL — message types / `encode_client_message` not found.

- [ ] **Step 3: Add `serde` derive + message types in `src/protocol.rs`**

Add to the imports at the top (these provide the manual `Serialize` impl for `Value`; the config/message types use the full-path `#[derive(serde::Serialize)]` so there is no name clash):

```rust
use serde::ser::{Serialize, SerializeMap, Serializer};
```

Append:

```rust
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

/// Serialize a Value in the *tagged* form (matches TS JSON.stringify of the
/// tagged Value with the bigint replacer). Used only by encode_client_message
/// for the ReportState variant.
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
```

- [ ] **Step 4: Re-export from `src/lib.rs`**

```rust
pub use protocol::{
    as_int, as_record, as_str, encode_client_message, encode_state, get_param, get_param_int,
    ApalacheConfig, ClientMessage, MirrorMessage, SpecResult, State, TraceGenerationConfig, Value,
};
```

- [ ] **Step 5: Run tests**

Run: `cargo test --test protocol encode_register encode_report_state`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/protocol.rs src/lib.rs tests/protocol.rs
git commit -m "feat: message types, tagged Value Serialize, encode_client_message"
```

---

## Task 5: Error enum + decode (`walk`, `walk_message`, `decode_mirror_message`)

**Files:**
- Modify: `src/lib.rs` (add `Error`)
- Modify: `src/protocol.rs` (add decode)
- Modify: `tests/protocol.rs`

- [ ] **Step 1: Add the `Error` enum to `src/lib.rs`**

Replace the contents of `src/lib.rs` with:

```rust
pub mod client;
pub mod protocol;
pub mod transport;

use protocol::prettify_json;

pub use protocol::{
    as_int, as_record, as_str, decode_mirror_message, encode_client_message, encode_state,
    get_param, get_param_int, ApalacheConfig, ClientMessage, MirrorMessage, SpecResult, State,
    TraceGenerationConfig, Value,
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
```

Note: `client` and `transport` modules are referenced here but created in Tasks 6–7. To keep this task building on its own, temporarily create empty stub files now:

Run:
```bash
printf '' > src/client.rs
printf '' > src/transport.rs
```

- [ ] **Step 2: Write the failing decode tests (append to `tests/protocol.rs`)**

```rust
use mirrorrust::{decode_mirror_message, MirrorMessage, SpecResult};

#[test]
fn decode_spec_validated_valid() {
    let m = decode_mirror_message(r#"{"proto_step":"spec_validated","result":"valid"}"#).unwrap();
    assert_eq!(m, MirrorMessage::SpecValidated { result: SpecResult::Valid });
}

#[test]
fn decode_spec_validated_invalid() {
    let m = decode_mirror_message(r#"{"proto_step":"spec_validated","result":{"invalid":"bad spec"}}"#).unwrap();
    assert_eq!(
        m,
        MirrorMessage::SpecValidated {
            result: SpecResult::Invalid(r#"{"invalid":"bad spec"}"#.to_string())
        }
    );
}

#[test]
fn decode_initial_state() {
    let m = decode_mirror_message(r#"{"proto_step":"initial_state","action":"Init","state":{"count":0}}"#).unwrap();
    assert_eq!(
        m,
        MirrorMessage::InitialState {
            action: "Init".into(),
            state: st(vec![("count", Value::Int(BigInt::from(0)))]),
        }
    );
}

#[test]
fn decode_next_step() {
    let m = decode_mirror_message(r#"{"proto_step":"next_step","action":"Incr","parameters":{"by":1}}"#).unwrap();
    assert_eq!(
        m,
        MirrorMessage::NextStep {
            action: "Incr".into(),
            parameters: st(vec![("by", Value::Int(BigInt::from(1)))]),
        }
    );
}

#[test]
fn decode_step_ok_and_all_done() {
    assert_eq!(decode_mirror_message(r#"{"proto_step":"step_ok"}"#).unwrap(), MirrorMessage::StepOk);
    assert_eq!(decode_mirror_message(r#"{"proto_step":"all_steps_done"}"#).unwrap(), MirrorMessage::AllStepsDone);
}

#[test]
fn decode_step_mismatch() {
    let m = decode_mirror_message(r#"{"proto_step":"step_mismatch","action":"Inc","expected":{"count":1},"actual":{"count":2}}"#).unwrap();
    assert_eq!(
        m,
        MirrorMessage::StepMismatch {
            action: Some("Inc".into()),
            expected: st(vec![("count", Value::Int(BigInt::from(1)))]),
            actual: st(vec![("count", Value::Int(BigInt::from(2)))]),
        }
    );
}

#[test]
fn decode_gen_traces_done_and_errors() {
    assert_eq!(
        decode_mirror_message(r#"{"proto_step":"gen_traces_done","itfTracePaths":["/tmp/t1.itf.json"]}"#).unwrap(),
        MirrorMessage::GenTracesDone { itf_trace_paths: vec!["/tmp/t1.itf.json".into()] }
    );
    assert_eq!(
        decode_mirror_message(r#"{"proto_step":"protocol_error","error":"bad!"}"#).unwrap(),
        MirrorMessage::ProtocolError { error: "bad!".into() }
    );
    assert_eq!(
        decode_mirror_message(r#"{"proto_step":"register_error","error":"spec not found"}"#).unwrap(),
        MirrorMessage::RegisterError { error: "spec not found".into() }
    );
}

#[test]
fn decode_unknown_proto_step() {
    assert_eq!(
        decode_mirror_message(r#"{"proto_step":"unknown_thing","x":1}"#).unwrap(),
        MirrorMessage::ProtocolError { error: "unknown proto_step: unknown_thing".into() }
    );
}

#[test]
fn decode_value_kinds_via_initial_state() {
    let m = decode_mirror_message(
        r#"{"proto_step":"initial_state","action":"Init","state":{
            "ready":true,"done":false,"name":"alice","nothing":null,
            "person":{"name":"bob","age":30},
            "pair":{"#tup":["foo",7]},
            "items":[1,2,3]
        }}"#,
    )
    .unwrap();
    let state = match m {
        MirrorMessage::InitialState { state, .. } => state,
        _ => panic!("expected initial_state"),
    };
    assert_eq!(state["ready"], Value::Bool(true));
    assert_eq!(state["done"], Value::Bool(false));
    assert_eq!(state["name"], Value::Str("alice".into()));
    assert_eq!(state["nothing"], Value::Null);
    assert_eq!(
        state["person"],
        Value::Record(st(vec![("name", Value::Str("bob".into())), ("age", Value::Int(BigInt::from(30)))]))
    );
    assert_eq!(
        state["pair"],
        Value::Tuple(vec![Value::Str("foo".into()), Value::Int(BigInt::from(7))])
    );
    assert_eq!(
        state["items"],
        Value::Set(vec![Value::Int(BigInt::from(1)), Value::Int(BigInt::from(2)), Value::Int(BigInt::from(3))])
    );
}

#[test]
fn encode_state_decode_round_trip_bigint() {
    let s = st(vec![("count", Value::Int(BigInt::parse_bytes(b"12345678901234567890", 10).unwrap()))]);
    let line = serde_json::to_string(&json!({
        "proto_step": "initial_state",
        "action": "Init",
        "state": encode_state(&s),
    }))
    .unwrap();
    let m = decode_mirror_message(&line).unwrap();
    let state = match m {
        MirrorMessage::InitialState { state, .. } => state,
        _ => panic!(),
    };
    assert_eq!(state["count"], Value::Int(BigInt::parse_bytes(b"12345678901234567890", 10).unwrap()));
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --test protocol decode_`
Expected: FAIL — `decode_mirror_message` not found.

- [ ] **Step 4: Implement decode in `src/protocol.rs`**

Append:

```rust
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
```

- [ ] **Step 5: Run tests**

Run: `cargo test --test protocol decode_ encode_state_decode_round_trip`
Expected: PASS.

- [ ] **Step 6: Full protocol test run + build**

Run: `cargo test --test protocol`
Expected: all protocol tests PASS.

- [ ] **Step 7: Commit**

```bash
git add src/lib.rs src/protocol.rs src/client.rs src/transport.rs tests/protocol.rs
git commit -m "feat: Error enum and mirror-message decoding"
```

---

## Task 6: `transport.rs` — spawn a binary over stdio

**Files:**
- Modify: `src/transport.rs`
- Modify: `src/lib.rs` (re-export)

- [ ] **Step 1: Implement `src/transport.rs`**

```rust
use crate::Error;
use std::io::{BufRead, BufReader, Lines, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub struct Transport {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Lines<BufReader<ChildStdout>>,
}

pub fn spawn_mirror(bin_path: &str) -> Result<Transport, Error> {
    let mut child = Command::new(bin_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let lines = BufReader::new(stdout).lines();
    Ok(Transport { child, stdin: Some(stdin), lines })
}

impl Transport {
    /// Write a single newline-terminated line and flush.
    pub fn send(&mut self, line: &str) -> Result<(), Error> {
        let stdin = self.stdin.as_mut().ok_or(Error::TransportClosed)?;
        stdin.write_all(line.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(())
    }

    /// Read one line from the child's stdout. `Ok(None)` on EOF.
    pub fn recv(&mut self) -> Result<Option<String>, Error> {
        match self.lines.next() {
            Some(Ok(line)) => Ok(Some(line)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Close stdin (signal EOF) and wait for the child. Idempotent.
    pub fn close(&mut self) -> Result<i32, Error> {
        self.stdin.take(); // dropping ChildStdin closes the pipe
        let status = self.child.wait()?;
        Ok(status.code().unwrap_or(0))
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.wait();
    }
}
```

- [ ] **Step 2: Re-export from `src/lib.rs`**

Add `transport::{spawn_mirror, Transport}` to the `pub use` list:

```rust
pub use transport::{spawn_mirror, Transport};
```

- [ ] **Step 3: Build**

Run: `cargo build`
Expected: compiles cleanly.

- [ ] **Step 4: Commit**

```bash
git add src/transport.rs src/lib.rs
git commit -m "feat: synchronous stdio transport (spawn_mirror)"
```

---

## Task 7: `client.rs` — StateComputer, entry points, loops, preset_client

**Files:**
- Modify: `src/client.rs`
- Modify: `src/lib.rs` (re-export)
- Modify: `tests/protocol.rs` (preset_client unit test)

- [ ] **Step 1: Write the failing preset_client test (append to `tests/protocol.rs`)**

```rust
use mirrorrust::{preset_client, StateComputer};

#[test]
fn preset_client_serves_in_order() {
    let s0 = st(vec![("count", Value::Int(BigInt::from(0)))]);
    let s1 = st(vec![("count", Value::Int(BigInt::from(2)))]);
    let mut pc = preset_client(vec![s0.clone(), s1.clone()]);
    let empty = State::new();
    assert_eq!(pc.compute("init", &empty, &empty), s0);
    assert_eq!(pc.compute("tick", &empty, &s0), s1);
}

#[test]
#[should_panic(expected = "preset_client exhausted")]
fn preset_client_panics_when_exhausted() {
    let mut pc = preset_client(vec![]);
    let empty = State::new();
    let _ = pc.compute("init", &empty, &empty);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test protocol preset_client`
Expected: FAIL — `preset_client` / `StateComputer` not found.

- [ ] **Step 3: Implement `src/client.rs`**

```rust
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
pub struct PresetClient {
    states: Vec<State>,
    index: usize,
}

impl StateComputer for PresetClient {
    fn compute(&mut self, _action: &str, _params: &State, _prev: &State) -> State {
        let s = self
            .states
            .get(self.index)
            .cloned()
            .unwrap_or_else(|| panic!("preset_client exhausted"));
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
```

- [ ] **Step 4: Re-export from `src/lib.rs`**

Add to the `pub use` list:

```rust
pub use client::{
    preset_client, run_client, run_client_gen_traces, run_client_with_traces, PresetClient,
    StateComputer,
};
```

- [ ] **Step 5: Run tests + build**

Run: `cargo test --test protocol preset_client`
Expected: PASS (2 tests).
Run: `cargo build`
Expected: compiles cleanly.

- [ ] **Step 6: Commit**

```bash
git add src/client.rs src/lib.rs tests/protocol.rs
git commit -m "feat: client loops, entry points, StateComputer, preset_client"
```

---

## Task 8: `tests/smoke.rs` — gated integration test

Ports `test/smoke.test.ts`. Skips cleanly when `MIRROR_BIN` is unset.

**Files:**
- Create: `tests/smoke.rs`

- [ ] **Step 1: Create `tests/smoke.rs`**

```rust
use mirrorrust::{
    as_int, get_param, preset_client, run_client, run_client_gen_traces, run_client_with_traces,
    ApalacheConfig, State, StateComputer, TraceGenerationConfig, Value,
};
use num_bigint::BigInt;

fn st(pairs: Vec<(&str, Value)>) -> State {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn spec_path() -> String {
    std::env::var("SPEC").unwrap_or_else(|_| "./specs/Counter.tla".to_string())
}

fn apalache_config() -> ApalacheConfig {
    ApalacheConfig {
        spec_path: spec_path(),
        init_predicate: None,
        next_predicate: None,
        const_init: Some("CInit".into()),
        invariant: "TraceComplete".into(),
        length_bound: 6,
        param_vars: Some("parameters".into()),
    }
}

fn trace_config() -> TraceGenerationConfig {
    TraceGenerationConfig { num_traces: 100, view: Some("View".into()) }
}

struct CounterComputer {
    count: BigInt,
}

impl CounterComputer {
    fn new() -> Self {
        CounterComputer { count: BigInt::from(0) }
    }
    fn to_state(&self) -> State {
        st(vec![("count", Value::Int(self.count.clone()))])
    }
}

impl StateComputer for CounterComputer {
    fn compute(&mut self, action: &str, params: &State, prev: &State) -> State {
        if action == "Init" || !prev.contains_key("count") {
            self.count = BigInt::from(0);
            return self.to_state();
        }
        let stride = get_param(params, "parameters")
            .and_then(|rec| rec.get("stride"))
            .and_then(as_int)
            .cloned()
            .unwrap_or_else(|| BigInt::from(0));
        self.count += stride;
        self.to_state()
    }
}

#[test]
fn smoke() {
    let bin = match std::env::var("MIRROR_BIN") {
        Ok(b) if !b.is_empty() => b,
        _ => {
            eprintln!("MIRROR_BIN not set; skipping smoke test");
            return;
        }
    };

    // register (trace generation + replay)
    run_client(&bin, apalache_config(), trace_config(), CounterComputer::new())
        .expect("register smoke test failed");

    // register_traces (replay a pre-generated trace against fixed states)
    let trace_path = std::fs::canonicalize("specs/traces/violation.itf.json")
        .expect("trace file")
        .to_string_lossy()
        .into_owned();
    let states = vec![
        st(vec![("count", Value::Int(BigInt::from(0)))]),
        st(vec![("count", Value::Int(BigInt::from(2)))]),
        st(vec![("count", Value::Int(BigInt::from(4)))]),
        st(vec![("count", Value::Int(BigInt::from(6)))]),
        st(vec![("count", Value::Int(BigInt::from(8)))]),
        st(vec![("count", Value::Int(BigInt::from(10)))]),
        st(vec![("count", Value::Int(BigInt::from(13)))]),
    ];
    run_client_with_traces(&bin, apalache_config(), vec![trace_path], preset_client(states))
        .expect("register_traces smoke test failed");

    // register_trace_gen (write traces to a temp dir)
    let dir = tempfile::tempdir().expect("tempdir");
    run_client_gen_traces(
        &bin,
        apalache_config(),
        dir.path().to_str().unwrap(),
        trace_config(),
    )
    .expect("register_trace_gen smoke test failed");
    let count = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".itf.json"))
        .count();
    assert!(count > 0, "no trace files generated");
}
```

- [ ] **Step 2: Run the smoke test (skips without a binary)**

Run: `cargo test --test smoke`
Expected: PASS with the line `MIRROR_BIN not set; skipping smoke test` (test returns early).

- [ ] **Step 3: (Optional) run against a real binary**

Run: `MIRROR_BIN=/path/to/ModelMirros cargo test --test smoke -- --nocapture`
Expected: PASS, prints generated-trace count.

- [ ] **Step 4: Commit**

```bash
git add tests/smoke.rs
git commit -m "test: gated smoke integration test (MIRROR_BIN)"
```

---

## Task 9: README + final verification

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Replace `README.md`**

```markdown
# MirrorRust

Rust client for the [ModelMirros](https://github.com/NzSN/ModelMirrors) protocol —
replay TLA+ traces against your state-machine implementation over stdio. A port of
[MirrorECMA](../MirrorECMA).

## Build & Test

```bash
cargo build
cargo test                       # unit/protocol tests (no binary needed)
MIRROR_BIN=/path/to/ModelMirros cargo test --test smoke   # end-to-end
```

## Quick Start

```rust
use mirrorrust::{run_client, get_param, as_int, ApalacheConfig, State, StateComputer, TraceGenerationConfig, Value};
use num_bigint::BigInt;

fn v(n: i64) -> Value { Value::Int(BigInt::from(n)) }

struct Counter { count: BigInt }
impl StateComputer for Counter {
    fn compute(&mut self, action: &str, params: &State, prev: &State) -> State {
        if action == "Init" || !prev.contains_key("count") {
            self.count = BigInt::from(0);
        } else {
            let stride = get_param(params, "parameters")
                .and_then(|r| r.get("stride")).and_then(as_int).cloned()
                .unwrap_or_else(|| BigInt::from(0));
            self.count += stride;
        }
        [("count".to_string(), Value::Int(self.count.clone()))].into_iter().collect()
    }
}

fn main() -> Result<(), mirrorrust::Error> {
    run_client(
        "/path/to/ModelMirros",
        ApalacheConfig {
            spec_path: "specs/Counter.tla".into(),
            invariant: "TraceComplete".into(),
            length_bound: 6,
            const_init: Some("CInit".into()),
            param_vars: Some("parameters".into()),
            init_predicate: None, next_predicate: None,
        },
        TraceGenerationConfig { num_traces: 100, view: Some("View".into()) },
        Counter { count: BigInt::from(0) },
    )
}
```

## API

- `run_client(bin, apalache_config, trace_config, compute)` — generate traces and replay.
- `run_client_with_traces(bin, apalache_config, trace_paths, compute)` — replay given ITF traces.
- `run_client_gen_traces(bin, apalache_config, dest_path, trace_config)` — generate traces to disk.
- `preset_client(states)` — a `StateComputer` serving a fixed state sequence.
- Helpers: `as_int`, `as_str`, `as_record`, `get_param`, `get_param_int`.
- Encoding: `encode_state`, `encode_client_message`, `decode_mirror_message`.
- Transport: `spawn_mirror`, `Transport`.

State values use the tagged `Value` enum (`Int(BigInt)`, `Bool`, `Str`, `Set`, `Tuple`, `Record`, `Null`), serialized to the Apalache ITF format (`{"#bigint":"42"}`, `{"#tup":[...]}`, `{"#set":[...]}`).
```

- [ ] **Step 2: Final verification**

Run: `cargo build`
Expected: clean.
Run: `cargo test`
Expected: all protocol tests PASS; smoke test PASS (skipped).
Run: `cargo clippy --all-targets -- -D warnings`
Expected: no warnings. (Fix any clippy findings, e.g. needless clones, before committing.)

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: Rust README for mirrorrust"
```

---

## Behavioral parity checklist (verify against `../MirrorECMA`)

- [ ] `encode_state` produces clean ITF (`{"#bigint"}`, `{"#set"}`, `{"#tup"}`); `encode_client_message` on `report_state` produces the tagged form — both reproduced.
- [ ] Decode reads bare JSON arrays as `Set`, numbers as `Int`, `#bigint`/`#tup`/`#set` markers handled; unknown `proto_step` → `ProtocolError`.
- [ ] Main loop: `spec_validated` gate, `initial_state`/`next_step` → `report_state`, `step_ok` no-op, `all_steps_done` → success, `step_mismatch`/errors → `Err`.
- [ ] `gen_traces_loop` accepts only `gen_traces_done`.
- [ ] `preset_client` panics on exhaustion.
- [ ] Optional `ApalacheConfig`/`TraceGenerationConfig` fields omitted from JSON when `None`.
