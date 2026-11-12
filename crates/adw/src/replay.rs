//! Rebuild a [`Run`] from recorded facts (ADR 0051 R4).
//!
//! The engine stays pure: replay does not read a journal, it takes the dispatch
//! and settle facts a journal recorded and re-drives the same transitions. Each
//! recorded dispatch must be what [`Run::next_step_with_posture`] would produce
//! at that point, so a journal that disagrees with the workflow stops at the
//! first entry that diverges instead of rebuilding a different run.

use omp_core::Str;
use thiserror::Error;

use crate::{Phase, PhaseName, Posture, Run, RunError, Step};

/// One recorded fact about a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplayEvent {
	/// An attempt was committed to run.
	Dispatch {
		/// The phase.
		phase:   PhaseName,
		/// The 1-based attempt ordinal.
		attempt: u32,
		/// The posture the host resolved for the phase, when it reported one.
		posture: Option<Posture>,
	},
	/// An attempt finished and its result was decided.
	Settle {
		/// The phase.
		phase:    PhaseName,
		/// Whether the phase met its acceptance condition.
		accepted: bool,
		/// The rejection feedback; empty when accepted.
		feedback: Str,
	},
}

/// What replay left in flight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replayed {
	/// A dispatch with no settle: its outcome is unknown and recovery must
	/// decide it.
	pub in_flight: Option<(PhaseName, u32)>,
}

/// Why replay stopped.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ReplayError {
	/// The engine would not dispatch what the journal recorded.
	#[error(
		"event {index}: the journal recorded {recorded} attempt {recorded_attempt}, the engine \
		 produced {produced}"
	)]
	Diverged {
		/// Position of the event.
		index:            usize,
		/// The phase the journal recorded.
		recorded:         PhaseName,
		/// The attempt the journal recorded.
		recorded_attempt: u32,
		/// What the engine produced instead.
		produced:         String,
	},
	/// A dispatch was recorded while another attempt was still in flight.
	#[error("event {index}: dispatch while {phase} was still in flight")]
	OverlappingDispatch {
		/// Position of the event.
		index: usize,
		/// The phase still in flight.
		phase: PhaseName,
	},
	/// A settle was recorded for a phase that was not in flight.
	#[error("event {index}: settle for {phase}, which was not in flight")]
	SettleWithoutDispatch {
		/// Position of the event.
		index: usize,
		/// The phase the settle named.
		phase: PhaseName,
	},
	/// The engine refused the recorded result.
	#[error("event {index}: the engine refused the recorded result")]
	Engine {
		/// Position of the event.
		index:  usize,
		/// The engine's reason.
		source: RunError,
	},
}

fn describe(step: &Step) -> String {
	match step {
		Step::Run { phase, attempt, .. } => format!("{} attempt {attempt}", phase.name),
		Step::Wait => "wait".to_owned(),
		Step::Blocked { .. } => "blocked".to_owned(),
		Step::Done { accepted } => format!("done (accepted: {accepted})"),
	}
}

/// Applies `events` to a fresh `run`, in order.
///
/// # Errors
///
/// [`ReplayError`] at the first event that disagrees with what the engine would
/// have done.
pub fn replay(run: &mut Run, events: &[ReplayEvent]) -> Result<Replayed, ReplayError> {
	let mut in_flight: Option<(PhaseName, u32)> = None;
	for (index, event) in events.iter().enumerate() {
		match event {
			ReplayEvent::Dispatch { phase, attempt, posture } => {
				if let Some((open, _)) = &in_flight {
					return Err(ReplayError::OverlappingDispatch { index, phase: open.clone() });
				}
				let recorded = *posture;
				let step = run.next_step_with_posture(|_: &Phase| recorded);
				match &step {
					Step::Run { phase: produced, attempt: produced_attempt, .. }
						if produced.name == *phase && produced_attempt == attempt => {},
					_ => {
						return Err(ReplayError::Diverged {
							index,
							recorded: phase.clone(),
							recorded_attempt: *attempt,
							produced: describe(&step),
						});
					},
				}
				in_flight = Some((phase.clone(), *attempt));
			},
			ReplayEvent::Settle { phase, accepted, feedback } => {
				if in_flight.as_ref().map(|(open, _)| open) != Some(phase) {
					return Err(ReplayError::SettleWithoutDispatch { index, phase: phase.clone() });
				}
				let outcome = if *accepted {
					run.accept(PhaseName::from_ref(phase.as_str()))
				} else {
					run.reject(PhaseName::from_ref(phase.as_str()), feedback.clone())
				};
				outcome.map_err(|source| ReplayError::Engine { index, source })?;
				in_flight = None;
			},
		}
	}
	Ok(Replayed { in_flight })
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{OnReject, PhaseKind, PhaseStatus, Workflow, WorkflowName};

	fn workflow() -> Workflow {
		Workflow::new(WorkflowName::new("ship"), vec![
			Phase::new(PhaseName::new("build"), PhaseKind::Agent),
			Phase::new(PhaseName::new("review"), PhaseKind::Review)
				.consuming([PhaseName::new("build")])
				.on_reject(OnReject::Correct),
		])
		.expect("workflow")
	}

	fn name(value: &str) -> PhaseName {
		PhaseName::new(value)
	}

	fn dispatch(phase: &str, attempt: u32) -> ReplayEvent {
		ReplayEvent::Dispatch { phase: name(phase), attempt, posture: None }
	}

	fn accept(phase: &str) -> ReplayEvent {
		ReplayEvent::Settle { phase: name(phase), accepted: true, feedback: Str::empty() }
	}

	fn reject(phase: &str, feedback: &'static str) -> ReplayEvent {
		ReplayEvent::Settle {
			phase:    name(phase),
			accepted: false,
			feedback: Str::new_static(feedback),
		}
	}

	fn original() -> (Run, Vec<ReplayEvent>) {
		let mut run = Run::new(workflow());
		let mut events = Vec::new();
		loop {
			match run.next_step() {
				Step::Run { phase, attempt, .. } => {
					events.push(dispatch(phase.name.as_str(), attempt));
					if phase.name.as_str() == "review" && attempt == 1 {
						run.reject(PhaseName::from_ref(phase.name.as_str()), "missing tests")
							.expect("reject");
						events.push(reject("review", "missing tests"));
					} else {
						run.accept(PhaseName::from_ref(phase.name.as_str()))
							.expect("accept");
						events.push(accept(phase.name.as_str()));
					}
				},
				Step::Done { .. } => break,
				other => panic!("unexpected step {other:?}"),
			}
		}
		(run, events)
	}

	#[test]
	fn a_replayed_run_has_the_same_snapshot_as_the_original() {
		let (original, events) = original();
		assert!(events.len() > 4, "the scenario must include a rejection and a retry");
		let mut replayed = Run::new(workflow());
		let outcome = replay(&mut replayed, &events).expect("replay");
		assert_eq!(outcome.in_flight, None);
		assert_eq!(replayed.snapshot(), original.snapshot());
		assert!(replayed.is_accepted());
	}

	#[test]
	fn a_dispatch_with_no_settle_is_left_in_flight() {
		let mut run = Run::new(workflow());
		let outcome = replay(&mut run, &[dispatch("build", 1)]).expect("replay");
		assert_eq!(outcome.in_flight, Some((name("build"), 1)));
		assert_eq!(run.snapshot().phases[0].status, PhaseStatus::Running);
	}

	#[test]
	fn a_journal_that_disagrees_with_the_workflow_stops_at_the_first_divergence() {
		let mut run = Run::new(workflow());
		let error = replay(&mut run, &[dispatch("review", 1)]).expect_err("diverges");
		assert!(matches!(error, ReplayError::Diverged { index: 0, .. }), "{error:?}");
		let mut run = Run::new(workflow());
		let error = replay(&mut run, &[dispatch("build", 2)]).expect_err("wrong attempt");
		assert!(matches!(error, ReplayError::Diverged { index: 0, .. }), "{error:?}");
	}

	#[test]
	fn a_settle_without_a_dispatch_is_refused() {
		let mut run = Run::new(workflow());
		let error = replay(&mut run, &[accept("build")]).expect_err("no dispatch");
		assert!(matches!(error, ReplayError::SettleWithoutDispatch { index: 0, .. }), "{error:?}");
		let mut run = Run::new(workflow());
		let error = replay(&mut run, &[dispatch("build", 1), accept("review")]).expect_err("other");
		assert!(matches!(error, ReplayError::SettleWithoutDispatch { index: 1, .. }), "{error:?}");
	}

	#[test]
	fn overlapping_dispatches_are_refused() {
		let mut run = Run::new(workflow());
		let error =
			replay(&mut run, &[dispatch("build", 1), dispatch("build", 1)]).expect_err("overlap");
		assert!(matches!(error, ReplayError::OverlappingDispatch { index: 1, .. }), "{error:?}");
	}

	#[test]
	fn an_in_flight_phase_can_be_interrupted_and_dispatched_again_with_the_same_attempt() {
		let mut run = Run::new(workflow());
		let outcome = replay(&mut run, &[dispatch("build", 1)]).expect("replay");
		let (phase, attempt) = outcome.in_flight.expect("in flight");
		run.interrupt(PhaseName::from_ref(phase.as_str()))
			.expect("interrupt");
		assert_eq!(run.snapshot().phases[0].status, PhaseStatus::Pending);
		let Step::Run { phase: again, attempt: again_attempt, .. } = run.next_step() else {
			panic!("the interrupted phase must dispatch again")
		};
		assert_eq!(again.name.as_str(), "build");
		assert_eq!(again_attempt, attempt);
		assert!(run.interrupt(PhaseName::from_ref("review")).is_err());
	}

	#[test]
	fn the_snapshot_carries_the_correction_a_rejection_left() {
		let events = [
			dispatch("build", 1),
			accept("build"),
			dispatch("review", 1),
			reject("review", "missing tests"),
		];
		let mut run = Run::new(workflow());
		replay(&mut run, &events).expect("replay");
		let mut other = Run::new(workflow());
		let mut different = events.clone();
		different[3] = reject("review", "wrong error message");
		replay(&mut other, &different).expect("replay");
		assert_ne!(run.snapshot(), other.snapshot(), "feedback must be part of the snapshot");
	}

	#[test]
	fn a_retry_route_replays_to_the_same_snapshot() {
		let retry = || {
			Workflow::new(WorkflowName::new("once"), vec![
				Phase::new(PhaseName::new("build"), PhaseKind::Agent).on_reject(OnReject::Retry),
			])
			.expect("workflow")
		};
		let mut original = Run::new(retry());
		let mut events = Vec::new();
		for accepted in [false, true] {
			let Step::Run { phase, attempt, .. } = original.next_step() else {
				panic!("run")
			};
			events.push(dispatch(phase.name.as_str(), attempt));
			if accepted {
				original
					.accept(PhaseName::from_ref("build"))
					.expect("accept");
				events.push(accept("build"));
			} else {
				original
					.reject(PhaseName::from_ref("build"), "flaky")
					.expect("reject");
				events.push(reject("build", "flaky"));
			}
		}
		let mut replayed = Run::new(retry());
		replay(&mut replayed, &events).expect("replay");
		assert_eq!(replayed.snapshot(), original.snapshot());
		assert_eq!(replayed.snapshot().phases[0].attempts, 2);
	}
}
