# MirrorRust

Rust client for the [Mirrors](https://github.com/NzSN/ModelMirrors) protocol —
replay TLA+ traces against your state-machine implementation over stdio,
plain TCP, or TLS 1.3 mutual TLS.

## Build & Test

```bash
cargo build
MIRRORS_FIXTURES=/path/to/Mirrors/test/fixtures cargo test
MIRROR_BIN=/path/to/ModelMirrors \
  SPEC=/path/to/authoritative/Counter.tla cargo test --test smoke
MIRROR_BIN=/path/to/ModelMirrors \
  SPEC=/path/to/authoritative/Counter.tla cargo test --test server_mode_smoke
```

`SPEC` is optional and defaults to `specs/Counter.tla`. The smoke test sends
that exact model inline, generates traces, and replays them through the Rust
state computer.

The server-mode smoke replays the same inline Counter model through real
`mirror --serve` and `mirror --server --tls` processes, then deliberately adds
an observable state key and requires `step_mismatch`. It also covers async job
ordering/cancellation/eviction/queue pressure and uses an ephemeral PKI to test
TLS 1.3-only negotiation, SAN-only identity, client authentication,
certificate pinning, registry failover, and POSIX client-key permissions.

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
- `submit_validate_async`, `submit_trace_gen_async`, `query_job`, `await_job`,
  and `cancel_job` — server-mode async jobs with cross-connection job IDs.
- `run_client_with_transport` and the other `*_transport` variants — consume
  an already connected TCP/mTLS transport for one session.
- `spec_from_file` / `spec_from_files` — build a root-first, canonical-path-deduplicated `EXTENDS`/`INSTANCE` closure.
- `preset_client(states)` — a `StateComputer` serving a fixed state sequence.
- Helpers: `as_int`, `as_str`, `as_record`, `get_param`, `get_param_int`.
- Encoding: `encode_state`, `encode_client_message`, `decode_mirror_message`.
- Compiled model admission: `make_verify_request`,
  `run_client_negotiated`, `run_client_with_traces_negotiated`, and their
  transport variants, with an exact `CompiledAdapterRegistry` and deferred
  `LocalBinding` factory.
- Transport: `spawn_mirror`, `connect_mirror`, `connect_tls_mirror`,
  `connect_mirror_from_registry`, `discover_mirrors`, `TlsOptions`, `Transport`.

## Protocol conformance

MirrorRust exposes the synchronous `register`, `register_traces`,
`register_trace_gen`, and `register_validate` flows over stdio, TCP, and mTLS,
plus the server-mode async job protocol and Consul-compatible mTLS registry
discovery. Explorer sessions are not yet exposed and are outside the supported
client-message subset checked by the canonical corpus test.

All outbound messages are one non-empty UTF-8 JSON object per line. Payloads
larger than 65,535 bytes or containing an embedded LF are rejected before any
write. `report_state` uses clean ITF values and arbitrary-precision `#bigint`
encoding. Incoming malformed ITF values, fractional JSON numbers, malformed
messages, and malformed diff hints are errors rather than silent defaults.
`protocol_error`, impossible replies, mismatches, and other terminal replies
end the connection.

Incoming JSON is decoded lexically so arbitrary-precision numbers remain exact
and an ordinary record key named `$serde_json::private::Number` remains an
ordinary key. Model-interface envelopes additionally reject duplicate keys,
unknown versioned fields, invalid status-specific combinations, excessive
depth/nodes, and noncanonical digests before any adapter callback runs.

State values use the typed `Value` enum (`Int(BigInt)`, `Bool`, `Str`, `Set`, `Seq`, `Tuple`, `Map`, `Record`, `Variant`, `Unserializable`, `Null`), serialized to the Apalache ITF format (`{"#bigint":"42"}`, `{"#tup":[...]}`, `{"#set":[...]}`, `{"#map":[[k,v],...]}`, `{"tag":t,"value":v}`, `{"#unserializable":s}`; bare JSON arrays decode as `Seq`).

## Current conformance and remote operation

The normative contract is the [Mirrors client guide](../Mirrors/Docs/client-implementation-guide.md).
For server setup and certificate/remote-file rules, see the
[remote server runbook](../Mirrors/Docs/remote-server-guide.md). Inline model
sources and their dependency closure are separate from server-visible trace
paths. A remote `destPath` never names a local client directory. Consume inline
trace results on network connections; `TRACE_RESULT_TOO_LARGE` is an explicit
backend failure, not permission to launch Apalache directly as a fallback.

Server async jobs and asynchronous SUT operations are different features. The
submitting connection owns its jobs: other connections can query or await them,
but disconnecting the owner cancels and evicts them. Job IDs are not durable
handles. Await timeouts return current status; stop polling cancelled or unknown
jobs. Logical cancellation can precede physical backend cleanup.

Shared async reply fixtures live under the client's test fixtures and originate
in `Mirrors/test/client-conformance/async-replies.json`. The focused acceptance
runner is `APALACHE_MC=/path/to/apalache-mc bash ../Mirrors/tools/interop/clients.sh`.
It requires live dependencies and checks fixture copies before running all three
clients. Unit tests do not prove runtime heap leak-freedom.

Async submission verifies the accepted job kind; query/await/cancel verify the
reply job ID. Send, read, decode, correlation and protocol failures close the
connection while retaining the primary error. A registration rejection such as
queue pressure leaves a valid connection usable. Mutable transport borrowing
serializes exchanges; backend jobs submitted before awaiting can run concurrently.
Compiled model-interface negotiation is available for reviewed metadata and
exact local adapter registrations. MirrorRust does not claim a compiler-emitted
`mirrorrust-v1` target; the Counter target in MirrorGate is a fixture-only
`mirrorrust-counter-fixture-v1` acceptance binding. The optional Gate facade
lives in `../MirrorGate/integrations/mirrorrust` and is not a MirrorRust runtime
dependency.

This source update adds the public `Error::ModelInterface` and
`Error::Registration` variants. Callers that exhaustively match `Error` must add
arms for local admission failures and structured server registration failures,
respectively. Existing runner signatures and wire encodings are retained, but
the enum addition is a Rust source-compatibility migration for exhaustive
matches. Package publication and versioning are handled separately.
