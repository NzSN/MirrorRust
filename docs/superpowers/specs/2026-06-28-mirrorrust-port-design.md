# MirrorRust — Rust port of MirrorECMA

**Date:** 2026-06-28
**Status:** Approved (design)

## Goal

Port [MirrorECMA](../../../../MirrorECMA) — the TypeScript client for the
[ModelMirros](https://github.com/NzSN/ModelMirrors) protocol — to a faithful,
idiomatic Rust library crate `mirrorrust`. The client spawns a ModelMirros
binary, talks newline-delimited JSON over stdio in a strict request/response
lock-step, and replays TLA+ traces against a user-supplied state computer.

## Decisions (from brainstorming)

1. **Concurrency model: synchronous / blocking.** No async runtime. The protocol
   is strictly lock-step (send → wait → recv → send …), so blocking I/O via
   `std::process` + `BufRead::lines()` maps cleanly and keeps dependencies minimal.
2. **Integer precision: `num_bigint::BigInt`.** The ITF `#bigint` encoding exists
   precisely to carry arbitrary-precision integers as JSON strings; matches the TS
   `bigint` semantics exactly.
3. **Scope: full port + all tests.** Every entry point, helper, and the unit +
   integration (smoke) test suites.
4. **State logic: `StateComputer` trait + blanket impl for closures.** Supports
   stateful computers idiomatically while keeping closures ergonomic.
5. **Errors: structured `thiserror`-derived enum.** Matchable variants; carries
   `Io`/`Json` source errors.

## Architecture

Single library crate, all I/O synchronous. Module layout mirrors the TS source 1:1.

```
mirrorrust/
├── Cargo.toml
├── src/
│   ├── lib.rs          # mod decls, Error enum, public re-exports
│   ├── protocol.rs     # Value, State, messages, ITF encode/decode, helpers
│   ├── client.rs       # run_client variants, main_loop, preset_client
│   └── transport.rs    # spawn_mirror, Transport
├── tests/
│   ├── protocol.rs     # port of protocol.test.ts (always runs)
│   └── smoke.rs        # port of smoke.test.ts (gated on MIRROR_BIN)
└── specs/              # already present: Counter.tla + traces/*.itf.json
```

**Dependencies:** `serde` (derive), `serde_json`, `num-bigint = "0.4"`,
`thiserror`. **Dev-dependency:** `tempfile` (gen-traces smoke test).
`num-bigint`'s serde feature is **not** needed — `BigInt` is never serialized
directly; it goes through the manual ITF `#bigint` encoding.

## Module: `protocol.rs`

### Core types

```rust
pub enum Value {
    Int(BigInt),
    Bool(bool),
    Str(String),
    Set(Vec<Value>),
    Tuple(Vec<Value>),
    Record(State),
    Null,
}
pub type State = BTreeMap<String, Value>;
```

`BTreeMap` (not `HashMap`): JSON objects are unordered on the wire (the mirror
does not depend on key order), and sorted keys give deterministic `report_state`
output and reproducible error messages. Value-equality is order-independent either
way.

### ITF wire conversion (manual — preserves the TS encode/decode asymmetry)

- `encode_value(&Value) -> serde_json::Value`:
  `Int → {"#bigint":"N"}`, `Set → {"#set":[...]}`, `Tuple → {"#tup":[...]}`,
  `Record → object`, `Bool/Str/Null` natural.
- `encode_state(&State) -> serde_json::Value`: object of encoded values.
- `walk(serde_json::Value) -> Value` (decode): `number → Int`, `string → Str`,
  `bool → Bool`, `null → Null`, `array → Set`; object with `#bigint → Int`,
  `#tup → Tuple`, `#set → Set`, otherwise `Record`.

Note the intentional asymmetry, faithful to the original: encode emits
`{"#set":[...]}` for sets, but decode reads bare JSON arrays as sets.

### Messages

- `ClientMessage` — serde-tagged enum `#[serde(tag = "proto_step")]` with variants
  `Register`, `RegisterTraces`, `RegisterTraceGen`, `ReportState`. Inner
  `ApalacheConfig` / `TraceGenerationConfig` use `rename_all = "camelCase"` and
  `skip_serializing_if = "Option::is_none"` (so absent optionals are omitted,
  matching `JSON.stringify` dropping `undefined`).
  `ApalacheConfig`: `spec_path` (req), `init_predicate?`, `next_predicate?`,
  `const_init?`, `invariant` (req), `length_bound` (req), `param_vars?`.
  `TraceGenerationConfig`: `num_traces` (req), `view?`.
  `encode_client_message(&ClientMessage) -> String` = `serde_json::to_string`.
- `MirrorMessage` — plain Rust enum, decoded manually via `walk_message`:
  `SpecValidated { result: SpecResult }`, `InitialState { action, state }`,
  `NextStep { action, parameters }`, `StepOk`,
  `StepMismatch { action: Option<String>, expected, actual }`, `AllStepsDone`,
  `GenTracesDone { itf_trace_paths }`, `ProtocolError { error }`,
  `RegisterError { error }`.
  `SpecResult` = `enum { Valid, Invalid(String) }`.
- `decode_mirror_message(&str) -> Result<MirrorMessage, Error>`: malformed JSON →
  `Err(Json)`; unknown `proto_step` → `Ok(ProtocolError { error: "unknown proto_step: …" })`
  (faithful to the TS returning a `protocol_error` object).

### Helpers (idiomatic borrowing)

- `as_int(&Value) -> Option<&BigInt>`
- `as_str(&Value) -> Option<&str>`
- `as_record(&Value) -> Option<&State>`
- `get_param(&State, name) -> Option<&State>`
- `get_param_int(&State, name, field) -> i64` (default `0`)
- `prettify_state(&State) -> serde_json::Value` (for error messages)

## Module: `transport.rs`

```rust
pub struct Transport {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
}
pub fn spawn_mirror(bin_path: &str) -> Result<Transport, Error>;
```

- `spawn_mirror`: `Command::new(bin)` with stdin/stdout piped, stderr inherited;
  `take()` stdin/stdout so the line reader and the `wait()` handle don't alias.
- `send(&mut self, line: &str) -> Result<(), Error>`: write `line + "\n"` and
  **flush** (Rust pipes are buffered; Node streams auto-flush — explicit flush
  avoids deadlock before `recv`).
- `recv(&mut self) -> Result<Option<String>, Error>`: `lines.next()` →
  `Some(Ok)` line, EOF → `Ok(None)`.
- `close(self) -> Result<i32, Error>`: drop `stdin` (EOF), `child.wait()`, return
  exit code.

## Module: `client.rs`

```rust
pub trait StateComputer {
    fn compute(&mut self, action: &str, params: &State, prev: &State) -> State;
}
impl<F: FnMut(&str, &State, &State) -> State> StateComputer for F { /* blanket */ }
```

### Entry points

- `run_client(bin, apalache_config, trace_config, compute) -> Result<(), Error>`
- `run_client_with_traces(bin, apalache_config, trace_paths, compute) -> Result<(), Error>`
- `run_client_gen_traces(bin, apalache_config, dest_path, trace_config) -> Result<(), Error>`

Each spawns a `Transport`, sends its register message via
`encode_client_message`, then runs `main_loop` (first three carry a computer) or
`gen_traces_loop`.

### `main_loop` (port of `mainLoop`)

1. `recv` first message; require `spec_validated`. Map otherwise:
   `ProtocolError → Error::ProtocolError`, `RegisterError → Error::RegisterFailed`,
   other → `Error::UnexpectedMessage`. `spec_validated` with `Invalid(s)` →
   `Error::SpecInvalid(s)`. Close transport before returning `Err`.
2. Loop over `recv`:
   - `InitialState { action, state }` → `state = compute(action, &state, &empty)`,
     send `report_state`, remember `last_action`.
   - `NextStep { action, parameters }` → `state = compute(action, &parameters, &state)`,
     remember `last_param`, send `report_state`.
   - `StepOk` → continue.
   - `AllStepsDone` → close, return `Ok(())`.
   - `StepMismatch { action, expected, actual }` → close, return
     `Err(StepMismatch { action: action.unwrap_or(last_action), params: last_param, expected, actual })`.
   - `ProtocolError`/`RegisterError`/other → close, corresponding `Err`.

`report_state` is sent via `encode_report_state(&State) -> String`
(`{"proto_step":"report_state","state": encode_state(state)}`), using
`encode_state` — **not** `encode_client_message` — exactly as the TS main loop.

### `gen_traces_loop` (port of `genTracesLoop`)

`recv` first message: `GenTracesDone` → close, `Ok(())`; `ProtocolError` → `Err`;
`RegisterError` → `Err(RegisterFailed)`; other → `Err(UnexpectedMessage)`.

### `preset_client`

```rust
pub struct PresetClient { states: Vec<State>, index: usize }
impl StateComputer for PresetClient { /* serves states in order */ }
pub fn preset_client(states: Vec<State>) -> PresetClient;
```

On exhaustion, `compute` **panics** with `"preset_client exhausted"` — faithful to
the TS `throw`, since the trait's `compute` returns `State` to mirror the original
signature.

## `lib.rs` — Error type + public surface

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("spec invalid: {0}")]                                   SpecInvalid(String),
    #[error("{0}")]                                                 ProtocolError(String),
    #[error("register failed: {0}")]                               RegisterFailed(String),
    #[error("step mismatch on action \"{action}\": expected {}, got {}",
            prettify_json(.expected), prettify_json(.actual))]
    StepMismatch { action: String, params: State, expected: State, actual: State },
    #[error("unexpected message: {0}")]                            UnexpectedMessage(String),
    #[error("transport closed unexpectedly")]                      TransportClosed,
    #[error(transparent)] Io(#[from] std::io::Error),
    #[error(transparent)] Json(#[from] serde_json::Error),
}
```

`prettify_json(&State) -> String` = `serde_json::to_string(&prettify_state(s))`,
so the mismatch message reproduces the TS prettified output. The TS
`"${lastParam}"` → `[object Object]` quirk is **not** replicated; `params` is kept
as a structured, matchable field.

`lib.rs` declares `mod protocol/client/transport`, defines `Error`, and re-exports
the full public surface (mirroring `index.ts`): `run_client`,
`run_client_with_traces`, `run_client_gen_traces`, `preset_client`,
`PresetClient`, `StateComputer`, `Value`, `State`, `MirrorMessage`,
`ClientMessage` + variants, `ApalacheConfig`, `TraceGenerationConfig`,
`SpecResult`, `encode_client_message`, `encode_state`, `decode_mirror_message`,
`as_int`, `as_str`, `as_record`, `get_param`, `get_param_int`, `spawn_mirror`,
`Transport`, `Error`.

## Tests

- `tests/protocol.rs` — full port of `protocol.test.ts`: register round-trip,
  `register_traces` encoding, `report_state` with bigints, decode of every message
  variant, value helpers, large-bigint handling, and value-encoding round-trips
  (bool/str/null/record/tuple/set).
- `tests/smoke.rs` — port of `smoke.test.ts`, gated on the `MIRROR_BIN` env var:
  **skips cleanly when unset** (returns instead of `process::exit(1)`). A
  `CounterComputer` struct implements `StateComputer`; covers `run_client`,
  `run_client_with_traces` (via `preset_client`), and `run_client_gen_traces`
  (writes to a `tempfile::tempdir`, asserts `.itf.json` files appear). `SPEC` env
  defaults to `./specs/Counter.tla`.

## Behavioral parity notes / intentional deviations

- `StepMismatch` error message uses `prettify_state` JSON (faithful), but the
  `params` portion is a structured field rather than the TS `[object Object]`.
- Sets encode as `{"#set":[...]}` and decode from bare arrays (faithful asymmetry).
- `preset_client` exhaustion panics (faithful to the TS throw).
- `State` ordering is deterministic (BTreeMap) rather than insertion-order; does
  not affect protocol correctness.

## Verification

- `cargo build`, `cargo test` (unit/protocol tests run without a binary).
- `cargo clippy` clean.
- Smoke test runs when `MIRROR_BIN` points at a ModelMirros binary.
