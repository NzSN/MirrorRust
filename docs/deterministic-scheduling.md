# Deterministic scheduling (experimental)

DPM-0–DPM-5 are accepted for this declared profile; source-hidden crate consumers,
generated replay, finite exploration and the frozen four-case native pilot pass.
[Exact language acceptance](../../Mirrors/Plans/dpm-languages-evidence-20261005/README.md)
is separate from [framework qualification](../../Mirrors/Docs/current-status.md),
full WriteSentry and new OS/Gate backend claims. The ordinary crate declaration
remains experimental and does not itself establish runtime acceptance.

The `schedule` module implements `mirrorrust.cooperative-checkpoints/v1` using
owned Rust OS threads. It grants one participating actor an interval ending at a
declared checkpoint. It does not provide an async executor, arbitrary instruction
preemption, weak-memory exploration or control of undeclared external effects.
The coordinated plan is Mirrors `Plans/dpm-mirrorecma-mirrorrust.md`.

## Generated integration kit and receipt timeline

[Mirrors kit/timeline tooling](../../Mirrors/Docs/dpm-usability-design.md)
now supplies `model_interface_gen generate-dpm/check-dpm` helpers, canonical
mapping metadata, application seeds and checklists for this language’s v1/v2
DPM targets. Applications connect `stepFor`/`step_for` to the existing binding
session and supply real worker factories, checkpoint calls and observations.
Copy application seeds before editing; regenerate owned helpers with the
compiler. Synchronous ECMA and Lean have no accepted DPM execution profile.

The read-only `timeline.py --receipt FILE --format text|json` renders actual
SDK comparison/binding/checkpoint receipts with model outcome and cleanup
separate. Its [source acceptance](../../Mirrors/Plans/dpm-usability-evidence-20261007/README.md)
does not renew the frozen SDK/native/package qualification above. Schedule
reduction, an installed cross-language CLI and synchronization wrappers
remain approved follow-ons.

## Coordinator and application seam

An `Adapter` declares actor/operation IDs, checkpoints and the expected model,
mapping and implementation digests. Its deferred `ProgramFactory` creates a
`Program` with owned worker closures, an actual-state observer and teardown.
Workers start behind an admission barrier. They keep their stacks/local variables
across `Checkpoint::arrive`, which parks until another permit or cancellation.
Returning from the worker supplies `$done`.

`start_schedule` admits the complete plan before invoking the factory.
`Execution::advance` must name exactly the next actor and destination checkpoint.
It releases that actor, checks the actual arrival and observes only at quiescence.
The terminal interval joins the thread before observation, including thread-local
destructors. `run_schedule` drives the entire fixed plan. A callback cannot
reenter the scheduler or use another actor's checkpoint successfully.

The observer must not wait for workers or acquire a resource held by a parked
worker. Factory, observer, teardown, destructors and custom runners must return
promptly. Coordinator deadlines cannot preempt arbitrary callback code. Use safe
application synchronization for shared state; scheduling does not make otherwise
undefined Rust memory access valid.

## Cancellation and retained ownership

`Cancellation` is a shared cancellation request. Coordinator waits observe it
and release owned parked threads. Cleanup joins completed workers and invokes
teardown once. A cleanup timeout reports `incomplete` with the still-owned actors;
retain the execution, release application-owned waits and retry `cleanup`.
Dropping an execution cancels and joins its threads and can block on uncooperative
application code. It never silently detaches. Hard termination needs an external
process owner.

Receipts distinguish primary outcome, plan completion and cleanup. They retain
actual observations, accepted permit/arrival order, Rust thread IDs and fresh
execution identity. Correlation IDs are not authentication credentials. Direct
bounds are 64 actors, 65,536 intervals, 128-character IDs, 65,535-byte inputs and
individual observations, 8 MiB execution evidence, and positive wait budgets up
to 24 hours. Strict artifact decoding rejects duplicate/unknown fields and wrong
profiles before acquisition.

## Generated bindings and native applications

`schedule_binding::BindingSession` is connection-local. Initialization creates
fresh executions; generated port actions call `advance`, observers read actual
session observations, and `LocalBinding.dispose` calls session disposal. Prior
incomplete executions cannot create another SUT. Disposal is idempotent, keeps
primary failures distinct from teardown failure and retains incomplete handles.
Cleanup retry updates retained ownership without changing the original replay
failure into success. Session evidence is bounded to 64 executions/16 MiB.

`replay_with_traces` uses the existing negotiated client and a private receive
observer to retain the actual peer verdict. It records real mismatch states and
ordered hints separately from binding cleanup. Existing public replay functions
and wire messages are unchanged. Expected model state must not initialize the
implementation or replace its observer.

The counter uses unchanged `mirrorrust-v1` generated bindings. The native pilot
uses explicit `mirrorrust-v2`: `MirrorIntMap<T>` provides integer-keyed maps, while
`MirrorMap<T>` retains string keys. Typed string/integer literal map projections
reject missing, duplicate and wrong-domain keys. The StateComputer contract and
semantic descriptor remain version 1; v1 generated bytes stay stable.

The WriteSentry integration is a separate application bridge over its existing
Begin/Advance/Quit protocol and atomic native phase gates. Proxy actors preserve
logical operations, while the native process reports actual phases/state and
Windows process/thread/image identities. No portable scheduler mutex enters the
trap handler. Qualification covers the four named two-operation schedules and
their declared controls, not every WriteSentry path or a complete Windows SDK.

## Finite exploration and package boundary

`schedule_exploration::explore_finite` exhaustively enumerates actor-order-
preserving merges over declared finite inputs, without POR. An unfinished-actor
switch counts as one preemption; a switch after `$done` does not. Initial bounds
are 8 actors, 64 total intervals, 64 inputs and 4,096 enumerated shapes. Run, time,
evidence and coverage limits remain explicit. Unknown cleanup stops acquisition;
unknown totals stay unknown and non-pass/truncated campaigns cannot be complete.
The local runner retains its latest execution, so retain it until any
application-owned wait has been released.

Canonical coverage preserves protocol Value types and ordinary record keys,
normalizes sets and map order, rejects duplicate/mixed map keys and retains
sequence/tuple order. Base variables and excluded instrumentation are explicit.
A model counterexample remains separate from cleanup failure. Local execution
coverage alone does not prove model conformance for all schedules.

`SCHEDULING_CAPABILITIES` embeds the normal crate's experimental declaration.
Exact source/package/native acceptance is recorded separately. Run
`cargo test --offline --test schedule --test schedule_lifecycle` for focused gates.
Mirrors' installed gate packages this crate, admits dependency sources, hides
checkouts and builds/runs fresh consumers without network access.
