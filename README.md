# MirrorRust

Rust client for the [Mirrors](https://github.com/NzSN/ModelMirrors) protocol —
replay TLA+ traces against your state-machine implementation over stdio,
plain TCP, or TLS 1.3 mutual TLS.

## Build & Test

```bash
cargo build
cargo test                       # unit/protocol tests (no binary needed)
MIRROR_BIN=/path/to/ModelMirrors \
  SPEC=/path/to/authoritative/Counter.tla cargo test --test smoke
MIRROR_BIN=/path/to/ModelMirrors \
  SPEC=/path/to/authoritative/Counter.tla cargo test --test server_mode_smoke
```

`SPEC` is optional and defaults to `specs/Counter.tla`. The smoke test sends
that exact model inline, generates traces, and replays them through the Rust
state computer.

The server-mode smoke replays the same inline Counter model through real
`mirror --serve` and `mirror --server --tls` processes. Its mTLS leg generates
an ephemeral PKI and also checks SAN-only identity, certificate pinning, and
POSIX client-key permissions.

## Quick Start

```rust
use mirrorrust::{
    as_int, get_param, run_client_with_inline_spec, spec_from_file,
    ApalacheConfig, State, StateComputer, TraceGenerationConfig, Value,
};
use num_bigint::BigInt;

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
    let spec = spec_from_file("specs/Counter.tla")?;
    run_client_with_inline_spec(
        "/path/to/ModelMirros",
        ApalacheConfig {
            // Inline sources are materialized server-side; use the root basename.
            spec_path: "Counter.tla".into(),
            invariant: "TraceComplete".into(),
            length_bound: 6,
            const_init: Some("CInit".into()),
            param_vars: Some("parameters".into()),
            init_predicate: None,
            next_predicate: None,
        },
        TraceGenerationConfig { num_traces: 100, view: Some("View".into()) },
        Counter { count: BigInt::from(0) },
        Some(spec),
    )
}
```

### Server mode

One transport carries one session. For a remote server, ship the spec inline
because `spec_path` otherwise refers to the server's filesystem:

```rust
use mirrorrust::{
    connect_tls_mirror, run_client_with_transport, spec_from_file, TlsOptions,
};

let tls = connect_tls_mirror(
    "127.0.0.1",
    8823,
    &TlsOptions::new("ca.crt", "client.crt", "client.key"),
)?;
let spec = spec_from_file("specs/Counter.tla")?;
run_client_with_transport(tls, config, traces, counter, Some(spec))?;
```

`connect_mirror(host, port)` connects to plain `mirror --serve`. For mTLS,
`TlsOptions` supports an optional SAN verification name, handshake timeout,
and case-insensitive SHA-256 leaf-certificate pin. IP literals are checked
against IP SAN and are not sent as SNI. Subject-CN fallback is disabled by
rustls/webpki, TLS is restricted to 1.3, and client keys must not be readable
by group or other users on POSIX.

## API

- `run_client(bin, apalache_config, trace_config, compute)` — generate traces and replay.
- `run_client_with_inline_spec(..., spec)` — generate/replay with a server-independent inline spec.
- `run_client_with_traces(bin, apalache_config, trace_paths, compute)` — replay given ITF traces.
- `run_client_gen_traces(...)` / `run_client_gen_traces_with_inline_spec(...)` — generate traces and return paths plus inline ITF data.
- `run_client_validate(bin, apalache_config, bound, spec)` — validate-only flow; bounds are checked in `[1, 100]`.
- `run_client_with_transport` and the other `*_transport` variants — consume
  an already connected TCP/mTLS transport for one session.
- `spec_from_file` / `spec_from_files` — build a root-first, canonical-path-deduplicated `EXTENDS`/`INSTANCE` closure.
- `preset_client(states)` — a `StateComputer` serving a fixed state sequence.
- Helpers: `as_int`, `as_str`, `as_record`, `get_param`, `get_param_int`.
- Encoding: `encode_state`, `encode_client_message`, `decode_mirror_message`.
- Transport: `spawn_mirror`, `connect_mirror`, `connect_tls_mirror`,
  `TlsOptions`, `Transport`.

## Protocol conformance

MirrorRust exposes the synchronous `register`, `register_traces`,
`register_trace_gen`, and `register_validate` flows over stdio, TCP, and mTLS.
Registry discovery, explorer sessions, and async job messages are not yet
exposed.

All outbound messages are one non-empty UTF-8 JSON object per line. Payloads
larger than 65,535 bytes or containing an embedded LF are rejected before any
write. `report_state` uses clean ITF values and arbitrary-precision `#bigint`
encoding. Incoming malformed ITF values, fractional JSON numbers, malformed
messages, and malformed diff hints are errors rather than silent defaults.
`protocol_error`, impossible replies, mismatches, and other terminal replies
end the connection.

State values use the typed `Value` enum (`Int(BigInt)`, `Bool`, `Str`, `Set`, `Seq`, `Tuple`, `Map`, `Record`, `Variant`, `Unserializable`, `Null`), serialized to the Apalache ITF format (`{"#bigint":"42"}`, `{"#tup":[...]}`, `{"#set":[...]}`, `{"#map":[[k,v],...]}`, `{"tag":t,"value":v}`, `{"#unserializable":s}`; bare JSON arrays decode as `Seq`).
