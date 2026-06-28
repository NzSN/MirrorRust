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
            init_predicate: None,
            next_predicate: None,
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
