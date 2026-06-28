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
