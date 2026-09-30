# MirrorRust client-guide conformance fixes

Date: 2026-10-01

Status: implemented; local profile acceptance passed. The user approved the audit's
recommendation to fix negotiation validation and registry pin propagation first.
This plan covers those fixes and the remaining audit findings. Approval is not
test evidence or a completed conformance claim.

## Objective and baseline

Close the identified client-guide gaps without relaxing its normative rules.
Retain the existing legacy wire messages and replay behavior, strict negotiated
admission, separate failure families, and exactly-once binding disposal.

Sources:

- [Client guide](../../Mirrors/Docs/client-implementation-guide.md): C26, C27,
  MI1, MI5, MI8, MI9, MI10, MI13, MI14, and section 11.
- [Runtime negotiation specification](../../Mirrors/Docs/model-interface-runtime-distribution-design.md):
  section 9.1 status precedence and section 9.2 structured failures.
- [Rust generated target](../../Mirrors/Docs/model-interface-compiler/rust-target.md).
- [Gate Rust status](../../MirrorGate/docs/rust-evaluator-sdk-status.md).

Planning baselines:

| Repository | HEAD |
| --- | --- |
| MirrorRust | `cf0e6297f0973696d0900f5ffb598c9d4dd10ff7` |
| Mirrors | `da0ae6d4f50176cdc4d7795006fc9e71471279a8` |
| MirrorGate | `0fa8a311018db9cbfdf2bb516235c1ec09e1ddbd` |

Mirrors and MirrorGate have existing worktree changes. Preserve them and inspect
the affected files before editing; HEAD alone does not identify their current
contents. The guide's planning-time SHA-256 is
`60736669e3c18207327b4de6cdbda97bc69ba8e6623a1176a9f1f5ff66cc607b`;
the runtime specification's is
`04ea8dbf6835e61bc9e53f0f3d931715842e38b163635fc95baf2fabe5c56e2a`.
Recheck these sources before implementation if they change.

The preceding audit exercised all 66 MirrorRust tests across separate runs:
65 passed and the registry discovery fixture failed. All three live smoke tests
ran with real Mirrors and Apalache. The separate generated-binding harness
passed 7/7 after its required manifest layout was supplied in `/tmp`. Those
results describe the audited sources, not the future fixes.

## Execution order

1. Fix compiled negotiation admission.
2. Preserve operator-configured registry pins.
3. Enforce literal POSIX key permissions.
4. Repair the registry HTTP fixture.
5. Restore the pre-negotiation legacy error API and migrate negotiated consumers.
6. Reconcile documentation and run final acceptance against the changed sources.

Steps 1-4 are small implementation changes. Step 5 is a coordinated API change
across client and consumer repositories and must include their compile checks.
No commit, push, package publication, or deployment is part of this plan.

## 1. Compiled negotiation admission

Owner/files: MirrorRust `src/model_interface.rs`, `tests/model_interface.rs`.

- [x] Define the accepted replies for the actual `verify` request explicitly.
  A successful admission requires a valid `matched` reply, accepted descriptor
  schema, and exact semantic digest. Under explicit `prefer`, an old-server
  omission or a valid `unsupported`/`unavailable` reply may use the separately
  supplied fallback factory.
- [x] Reject `too_large`, `resolved`, and `not_modified` as mode-inappropriate
  for compiled verification. The normative precedence selects `too_large` only
  for a descriptor request; adding a digest comparison while retaining that
  fallback would leave the mode-validation gap open.
- [x] Check failure envelopes against the same request mode and status-specific
  field rules. Preserve stable structured codes on valid server failures and
  classify malformed or contradictory envelopes as local negotiation failures.
- [x] Retain canonical digest parsing, strict duplicate/unknown-field decoding,
  no implicit fallback, zero callbacks before admission, and cleanup precedence.
- [x] Add focused defensive regression coverage for request-mode validation,
  both policies, and zero negotiated/fallback/config/computer/disposal callbacks
  when admission is rejected. Retain the positive explicit fallback cases.

Acceptance: no descriptor-only status can authorize a verify binding or fallback;
digest mismatch remains terminal; existing successful replay and disposal checks
pass. This change must not introduce runtime descriptor interpretation.

## 2. Registry certificate pin propagation

Owner/files: MirrorRust `src/registry.rs`, `tests/registry.rs`,
`tests/server_mode_smoke.rs`, `README.md`.

- [x] Establish and document precedence:
  `pin_override` > `TlsOptions.pin` > candidate `cert-sha256`.
- [x] Preserve the existing configured TLS pin when neither explicit override
  nor a stronger caller choice replaces it. Missing candidate metadata must not
  clear an operator pin.
- [x] Keep pin validation and comparison in the existing TLS connection path,
  before protocol writes. Invalid configured pins must remain errors.
- [x] Verify configured-pin propagation with and without candidate metadata,
  explicit override precedence, case-insensitive comparison, and existing
  correctly pinned candidate failover using the local test PKI.

Acceptance: changing discovery metadata cannot silently replace or remove the
selected operator pin. Existing explicit override behavior remains documented.

## 3. POSIX client-key permissions

Owner/files: MirrorRust `src/transport.rs`, `tests/transport.rs` or the existing
local-PKI cases in `tests/server_mode_smoke.rs`, `README.md`.

- [x] Require `(mode & 0o7777) == 0o600` on POSIX, excluding file-type bits and
  rejecting additional permission or special-mode bits.
- [x] Keep the check before key parsing/connection establishment and update the
  error text and README to describe the exact rule.
- [x] Verify acceptance of `0600` and rejection of representative owner-only,
  executable, group-readable, and special-bit modes. Gate POSIX assertions by OS.

Acceptance: only literal `0600` passes on POSIX. No new Windows ACL guarantee is
claimed by the existing non-Unix implementation.

## 4. Registry fixture completion

Owner/files: MirrorRust `tests/registry.rs`; inspect the similar
`registry_once` fixture in `tests/server_mode_smoke.rs` for the same assumption.

- [x] Replace the single request `read` with bounded reading through the HTTP
  header terminator (`\r\n\r\n`). Handle TCP fragmentation and reject EOF or
  excessive header size explicitly in the fixture.
- [x] Send and flush the response only after the complete request headers have
  been consumed. Preserve assertions on the request path and Accept header.
- [x] Run the existing discovery tests serially and in the normal suite.
  Retain failed-closed status/JSON cases.

Acceptance: the valid discovery test returns its two expected entries without
the unread-request reset observed in the audit. Do not hide the failure with
retries, sleeps, relaxed assertions, or blanket acceptance of partial responses.
Production HTTP-client redesign is outside this fixture repair.

## 5. Legacy error compatibility

Owner/files: MirrorRust `src/lib.rs`, `src/model_interface.rs`,
`tests/model_interface.rs`, a legacy-consumer compile fixture, and `README.md`;
MirrorGate `integrations/mirrorrust/src/{lib,main}.rs` and its tests;
Mirrors `tools/model-interface-rust/tests.rs` and any compiler-emitted code whose
type assumptions are affected.

Recommended design:

- [x] Restore the legacy `Error` variant set and legacy runner signatures from
  `328e4e6^`, the parent of the model-interface introduction. Remove the extension's
  `ModelInterface` and `Registration` variants from that legacy enum.
- [x] Add a separate public negotiated error type, provisionally
  `NegotiatedError`, with variants for a wrapped legacy `Error`, a structured
  server registration failure, and a local model-interface failure. Mark this
  new type non-exhaustive from introduction; do not retroactively mark the legacy
  enum non-exhaustive, which would also break exhaustive callers.
- [x] Migrate negotiated runners and public admission helpers consistently.
  Preserve error sources and stable codes. A normal replay mismatch remains a
  distinct wrapped `Error::StepMismatch`, not a model-admission failure.
- [x] Update Gate evaluator result types, failure mapping, cleanup precedence,
  and generated-harness matches to use the negotiated type. Keep Gate authority
  and resource ownership unchanged.
- [x] Compile an unchanged legacy consumer that exhaustively matches the
  pre-negotiation enum and uses legacy entry points. Verify its messages contain
  no `modelInterface` field.
- [x] Build the Gate integration and generated binding consumers against the
  same changed MirrorRust source and rerun their relevant error/lifecycle tests.
- [x] Record the migration for consumers of the already-added negotiated API.
  Restoring older legacy compatibility does not also preserve the current
  negotiated API's `Result<_, Error>` signature. Record this deliberately and
  decide package versioning before a separately authorized publication.

Acceptance: the pre-negotiation legacy consumer compiles unchanged; negotiated
consumers compile after their explicit migration; structured server failures,
local failures, ordinary mismatches, and cleanup precedence remain distinguishable.
MI1 is not closed merely by documenting a breaking change or incrementing a
version. Consumer compilation alone does not requalify Gate isolation.

## 6. Documentation and final acceptance

Owner/files: MirrorRust `README.md` and this plan; narrowly scoped edits to
Mirrors `Docs/client-implementation-guide.md` and MirrorGate
`docs/rust-evaluator-sdk-status.md` where their claims become stale.

- [x] Reconcile the guide's section 9 and reference-client table with the actual
  implemented `mirrorrust-v1` emitter. Keep the Gate-owned handwritten Counter
  fixture distinct from the generated Rust target and from package availability.
- [x] Document pin precedence, exact key mode, and the negotiated error migration.
  Preserve unrelated worktree edits in the guide.
- [x] Run formatting and Clippy, the complete locked MirrorRust test suite with
  canonical fixtures, and real smoke tests with explicit binary/spec/backend paths.
- [x] Run the generated Rust harness against the final changed client, including
  negotiated correct/faulty replay and its admission barrier checks. Use its
  prescribed manifest layout; a working-directory change alone does not change
  its compile-time `CARGO_MANIFEST_DIR` root.
- [x] Run generation freshness/check and relevant Gate consumer acceptance where
  their source or API changed. Report unavailable platform/backend gates explicitly.
- [x] Record final source identities, commands, test counts, remaining gaps, and
  source/local-acceptance status. Do not count optional skips as passing evidence.

Core commands, with required dependencies prepared:

```sh
cargo fmt --check
cargo clippy --offline --locked --all-targets -- -D warnings
MIRRORS_FIXTURES=../Mirrors/test/fixtures \
MIRROR_BIN=/home/nzsn/Repos/Mirrors/.lake/build/bin/mirror \
APALACHE_MC=/home/nzsn/.local/bin/apalache-mc \
SPEC=/home/nzsn/Repos/Mirrors/specs/Counter.tla \
cargo test --offline --locked -- --nocapture
```

Socket/TLS tests require host loopback permissions; the audit's command sandbox
denied listeners, and host execution resolved that environment failure. Companion
harnesses may write ignored build directories outside MirrorRust; use authorized
workspace access or a faithful temporary layout rather than altering assertions.

Completion requires all four normative gaps resolved, the registry fixture green,
the coordinated compatibility consumers green, and accurate documentation. Dynamic
descriptor support, explorer APIs, remote Gate control, new sandbox backends,
whole-program privacy proofs, and release qualification are outside this plan.


## Execution record — 2026-10-01

All six steps are implemented. The four normative gaps and registry fixture
failure are closed for the tested Linux profile. The legacy Error enum exactly
matches `328e4e6^`; negotiated helpers/runners and Gate consumers use the separate
non-exhaustive `NegotiatedError`. Existing generated Rust code needs no emitter
change. Publication/versioning remain deferred to a separate release decision.

| Check | Final result |
| --- | --- |
| Full locked MirrorRust suite | 70/70; all three real smoke tests exercised; zero ignored |
| Generated Rust harness | 7/7, including real negotiated stdio and faulty-observer mismatch |
| Compiler-generated Rust freshness | `model-interface check clean` |
| Mirrors model-interface specification executable | `MODEL INTERFACE SPEC GREEN` |
| Legacy consumer | Exhaustive pre-negotiation Error match compiled; registration has no extension |
| Gate Rust integration | 2/2; consumer driver built; Clippy passed |
| Required Gate Rust evaluator | 20/20 owned/attached × Node/Rust × five scenarios; cleanup confirmed |
| Wrong-digest evaluator rows | Four rows; zero factories/acquisitions/start events/dispatches |
| Independence and disclosure probes | Node sentinel untouched; private-canary probe passed |
| Gate broad regression | Exit 0; Python 293 pass / 3 skip; Node 227 + 4 pass; C++ 3/3 plus real control cases; Rust SDK 32/32 per feature mode and Rust worker 15/15; both runtimes' six lifecycle cases pass |
| Formatting, Clippy, diff whitespace | Passed for changed client/consumer sources |

The three Python skips are the existing aggregate-cgroup acceptance cases with
no operator-delegated parent configured. They are not passing aggregate-quota
or platform qualification evidence. The required Bubblewrap source-isolation,
worker, cleanup, and Rust evaluator cases ran successfully. At validation time,
no package was published and these changes were uncommitted.

Source hashes, binary hashes, counts and retained-log hashes are in
[evidence summary](evidence/client-guide-2026-10-01/summary.json). The same
directory retains gzip-compressed, byte-preserving final full-suite/Gate logs,
the 20 evaluator rows and the
runner-produced reproducibility manifest. That original manifest describes the
workspaces at execution time; completion-document/evidence additions happened
afterward. Mirrors advanced from the planning HEAD to
`772a82c3796f241439f610b9e65ab88049f57853` during this work; the executed binaries
and changed consumer files are identified separately in the evidence.

The final full-suite command is the core command above. Companion commands were:

```sh
# MirrorGate root: full required regression, with the pinned runtime prepared.
MIRRORGATE_NODE_RUNTIME_ROOT=/home/nzsn/Repos/MirrorGate/.work/toolchains/node-v24.15.0-linux-x64 \
bash scripts/test.sh

# MirrorGate root: required actual Rust evaluator matrix.
MIRRORRUST_ROOT=/home/nzsn/Repos/MirrorRust \
MIRRORS_ROOT=/home/nzsn/Repos/Mirrors \
APALACHE_MC=/home/nzsn/.local/bin/apalache-mc \
MIRRORGATE_NODE_RUNTIME_ROOT=/home/nzsn/Repos/MirrorGate/.work/toolchains/node-v24.15.0-linux-x64 \
MIRRORGATE_RUST_CONTROL_EVIDENCE=/tmp/mirrorrust-control-v1-acceptance.jsonl \
bash conformance/control-v1/run-rust

# Mirrors root: check the unchanged compiler-owned Rust output.
.lake/build/bin/model_interface_gen check \
  --spec specs/Counter.tla \
  --contract test/fixtures/model-interface/counter/Counter.mirror-interface.json \
  --evidence test/fixtures/model-interface/counter/counter.itf.json \
  --param-var parameters \
  --lock test/fixtures/model-interface/counter/Counter.mirror-interface.lock.json \
  --target mirrorrust-v1 \
  --out test/fixtures/model-interface/counter/generated-rust
```

The generated Rust execution harness used its actual `tools/model-interface-rust/tests.rs`
source with a temporary manifest at
`/tmp/mirrorrust-generated-audit-rfjj1556/.golden-build/model-interface-rust/Cargo.toml`.
The temporary root supplied read-only references to the actual Mirrors binary,
specification and fixtures. Its sources and both negotiated correct/faulty checks
ran against the changed MirrorRust path dependency. It used a separate resolved
consumer lock; MirrorRust and the Gate integration used their committed locks.

A formatter initially followed the harness's child modules and reformatted
compiler-owned files. They were regenerated through `model_interface_gen` and
`model_interface_spec`; the tracked generated output has no final diff and the
freshness check passes. Future harness formatting uses `skip_children=true`.
