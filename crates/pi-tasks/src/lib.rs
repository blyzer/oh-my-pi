//! Deterministic phase engine for AI developer workflows.
//!
//! Code owns sequencing, retries and acceptance; an agent owns only the work
//! inside one bounded phase. The engine is *driven*, not autonomous: it answers
//! "what runs next" and "did that count", while the caller executes the step —
//! the model providers live in the TypeScript layer, so Rust never spawns one.
//!
//! - [`Envelope`] — the typed handoff; context crosses phases in code.
//! - [`Gate`] — verifies an envelope's claims after the fact.
//! - [`Run`] — the state machine: [`Run::next_step`] / [`Run::submit_agent_output`].
//! - [`Tracer`] — 32-byte fixed-stride binary event log with an interned string
//!   table; [`TraceReader`] tails it from another process by sequence number.
//!
//! Text appears in exactly two places, both unavoidable: the model's own turn
//! (parsed once at the edge by [`Envelope::from_agent_text`]) and the
//! correction prompt fed back to it. Everything else is numbers.

pub mod envelope;
pub mod gate;
pub mod orchestrator;
pub mod phase;
pub mod trace;

#[cfg(test)]
mod test_support;

pub use envelope::{Envelope, EnvelopeError, EnvelopeStatus};
pub use gate::{ArtifactsExist, Check, FilesNonEmpty, Gate, GateCtx, GateReport, JsonParses};
pub use orchestrator::{Outcome, Run, RunError, RunSummary, Step, Workflow};
pub use phase::{PhaseKind, PhaseParams, PhaseRecord, PhaseStatus};
pub use trace::{Event, EventKind, EventRecord, TraceReader, Tracer};
