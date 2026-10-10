//! Explicit action boundary around admission, account, auth, and same-route
//! retry.

use std::{
	future, mem,
	task::{Context, Poll},
};

use tower::{Layer, Service};

use crate::{
	body::RetryDecision,
	error::{CredentialFailure, Error, ErrorDetail, ErrorPhase, RetryAction},
	layer::{AttemptAction, ExecutionContext, LayerCall},
};

/// Marks the complete attempt sub-stack without rebuilding it per call.
#[derive(Clone, Copy, Debug, Default)]
pub struct AttemptLayer;
/// Service routing only refresh and account-rotation actions back through the
/// inner attempt stack.
#[derive(Clone, Debug)]
pub struct AttemptService<S> {
	inner: S,
}
impl<S> Layer<S> for AttemptLayer {
	type Service = AttemptService<S>;

	fn layer(&self, inner: S) -> Self::Service {
		AttemptService { inner }
	}
}
impl<S, R> Service<LayerCall<R>> for AttemptService<S>
where
	S: Service<LayerCall<R>, Error = Error> + Clone,
	R: Clone,
{
	type Error = Error;
	type Response = S::Response;

	type Future = impl Future<Output = Result<S::Response, Error>>;

	fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
		self.inner.poll_ready(cx)
	}

	fn call(&mut self, request: LayerCall<R>) -> Self::Future {
		let replacement = self.inner.clone();
		let mut service = mem::replace(&mut self.inner, replacement);
		async move {
			let mut reentries = 0_u32;
			let mut refresh_once_replay_used = false;
			// The provider failure that caused the current re-entry.
			let mut cause: Option<Error> = None;
			request.context.set_attempt_action(AttemptAction::Initial);
			loop {
				request.context.clear_body_evidence();
				let attempted_action = request.context.attempt_action();
				let reserved = request.context.attempts();
				let result = service.call(request.clone()).await;
				let mut error = match result {
					Ok(response) => return Ok(response),
					Err(error) => error,
				};
				if let Some(cause) = cause.take()
					&& let Some(cause) = unsent_reentry(cause, &error, reserved, &request.context)
				{
					return Err(cause);
				}
				if let Some(attempt) = error.receipt().attempts.last() {
					request.context.set_body_evidence(attempt.body);
				}
				let replay_safe = request
					.context
					.body_evidence()
					.is_some_and(|evidence| evidence.retry_decision == RetryDecision::Allow);
				let prebody_route_reselection = error.action == RetryAction::ReselectRoute
					&& error.receipt().attempts.is_empty()
					&& matches!(error.phase, ErrorPhase::Admission | ErrorPhase::Authentication);
				if error.committed || (!replay_safe && !prebody_route_reselection) {
					error.action = RetryAction::Never;
					request.context.finalize_error(&mut error);
					return Err(error);
				}
				let previous_account = error
					.receipt()
					.attempts
					.last()
					.and_then(|attempt| attempt.account.clone());
				let action = match &error.action {
					RetryAction::RefreshCredentialOnce => {
						if refresh_once_replay_used {
							error.action = RetryAction::Never;
							request.context.finalize_error(&mut error);
							return Err(error);
						}
						refresh_once_replay_used = true;
						AttemptAction::RefreshCredential { previous_account }
					},
					RetryAction::RefreshCredential => match attempted_action {
						AttemptAction::Initial => AttemptAction::RefreshCredential { previous_account },
						AttemptAction::RefreshCredential { previous_account: refreshed_account } => {
							AttemptAction::RotateAccount {
								previous_account: previous_account.or(refreshed_account),
							}
						},
						AttemptAction::RotateAccount { .. } => {
							error.action = RetryAction::Never;
							request.context.finalize_error(&mut error);
							return Err(error);
						},
					},
					RetryAction::RotateAccount => AttemptAction::RotateAccount { previous_account },
					RetryAction::SameRoute { .. }
					| RetryAction::SameRouteLimited { .. }
					| RetryAction::ReselectRoute
					| RetryAction::ReseedSession
					| RetryAction::SemanticRetry
					| RetryAction::Never => {
						request.context.finalize_error(&mut error);
						return Err(error);
					},
				};
				let mut hidden_receipt = error.receipt().clone();
				for attempt in &mut hidden_receipt.attempts {
					attempt.hidden = true;
				}
				request.context.merge_receipt(&hidden_receipt);
				if reentries >= request.context.budget().max_attempts.saturating_sub(1) {
					error.action = RetryAction::Never;
					request.context.finalize_error(&mut error);
					return Err(error);
				}
				let retry_attempt = reentries.saturating_add(1);
				match &action {
					AttemptAction::RefreshCredential { .. } => tracing::warn!(
						retry_attempt,
						error_kind = ?error.kind,
						error_phase = ?error.phase,
						"provider authentication failed; refreshing credential before retry"
					),
					AttemptAction::RotateAccount { .. } => tracing::warn!(
						retry_attempt,
						error_kind = ?error.kind,
						error_phase = ?error.phase,
						"provider attempt failed; rotating account before retry"
					),
					AttemptAction::Initial => {},
				}
				reentries += 1;
				request.context.set_attempt_action(action);
				cause = Some(error);
				future::poll_fn(|cx| service.poll_ready(cx)).await?;
			}
		}
	}
}

/// The provider failure `cause` to report instead of `error` when the
/// re-entry `cause` started could not start at all: a rotation found no other
/// account ([`CredentialFailure::NoRotationCandidate`]) or a refresh found
/// the credential cannot be renewed ([`CredentialFailure::NotRenewable`]),
/// in authentication, before anything was sent: it reserved no wire attempt
/// and records none `cause` did not.
///
/// The caller then sees the provider's answer (a 402, a 401) rather than the
/// credential failure its retry ran into. Any other failure of the re-entry
/// (another account's own stored credential, a refresh that ran and failed)
/// names something the provider's answer does not, so it is reported as is.
/// `cause` is final: its action is `Never`, or `ReselectRoute` when `error`
/// asked for it, so a planned fallback to another provider still runs. Its
/// last attempt, hidden when the re-entry began, is visible again.
fn unsent_reentry(
	mut cause: Error,
	error: &Error,
	reserved: u32,
	context: &ExecutionContext,
) -> Option<Error> {
	let recorded_new_attempt = error.receipt().attempts.iter().any(|attempt| {
		!cause
			.receipt()
			.attempts
			.iter()
			.any(|known| known.index == attempt.index)
	});
	let unstartable = matches!(
		error.detail_ref(),
		Some(ErrorDetail::Credential {
			reason: CredentialFailure::NoRotationCandidate | CredentialFailure::NotRenewable,
			..
		})
	);
	if !unstartable
		|| context.attempts() != reserved
		|| recorded_new_attempt
		|| error.phase != ErrorPhase::Authentication
		|| error.committed
	{
		return None;
	}
	// Every pre-send credential failure a route reports today asks for route
	// reselection (`credential_failure` in `provider/builtin.rs`), which the
	// opaque error this replaces carried too: keeping it lets a planned
	// fallback to another provider's credentials still run. `cause`'s own
	// action (a rotation or a refresh) already ran, so any other answer ends
	// the request.
	cause.action = if error.action == RetryAction::ReselectRoute {
		RetryAction::ReselectRoute
	} else {
		RetryAction::Never
	};
	if let Some(index) = cause.receipt().attempts.last().map(|attempt| attempt.index) {
		context.with_receipt(|receipt| {
			for attempt in &mut receipt.attempts {
				if attempt.index == index {
					attempt.hidden = false;
				}
			}
		});
	}
	context.finalize_error(&mut cause);
	Some(cause)
}

#[cfg(test)]
mod tests {
	use std::{
		sync::{
			Arc,
			atomic::{AtomicUsize, Ordering},
		},
		task::{Context, Poll},
		time::Duration,
	};

	use futures::future::{Ready, ready};
	use parking_lot::Mutex;
	use tower::{Service, service_fn};

	use super::AttemptService;
	use crate::{
		body::{AttemptBodyEvidence, Replayability, RetryDecision, RetryDecisionReason},
		error::{CredentialFailure, Error, ErrorDetail, ErrorKind, ErrorPhase, RetryAction},
		id::AccountId,
		layer::{AttemptAction, ExecutionContext, LayerCall},
		receipt::{
			AttemptOutcome, AttemptReceipt, Cost, ExecutionBudget, ExecutionReceipt, ProviderEvidence,
			Usage,
		},
	};

	#[derive(Clone)]
	struct RefreshOnce {
		calls:   Arc<AtomicUsize>,
		actions: Arc<Mutex<Vec<AttemptAction>>>,
	}
	impl Service<LayerCall<()>> for RefreshOnce {
		type Error = Error;
		type Future = Ready<Result<(), Error>>;
		type Response = ();

		fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Error>> {
			Poll::Ready(Ok(()))
		}

		fn call(&mut self, request: LayerCall<()>) -> Self::Future {
			self.actions.lock().push(request.context.attempt_action());
			if self.calls.fetch_add(1, Ordering::SeqCst) > 0 {
				return ready(Ok(()));
			}
			let mut receipt = ExecutionReceipt::default();
			receipt.record_attempt(AttemptReceipt {
				index:             0,
				hidden:            false,
				provider:          None,
				route:             None,
				account:           Some(AccountId::from("account")),
				principal:         None,
				body:              AttemptBodyEvidence {
					opened:         true,
					consumed:       true,
					replayability:  Replayability::Replayable,
					retry_decision: RetryDecision::Allow,
					reason:         RetryDecisionReason::ReplayableSource,
				},
				outcome:           AttemptOutcome::FailedPreCommit,
				usage:             Usage::default(),
				cost:              Cost::default(),
				provider_evidence: ProviderEvidence::default(),
				elapsed:           Duration::ZERO,
			});
			ready(Err(Error::new(
				ErrorKind::Authentication,
				ErrorPhase::Authentication,
				RetryAction::RefreshCredential,
				receipt,
			)))
		}
	}

	#[tokio::test]
	async fn refresh_reenters_with_same_previous_account_action() {
		let calls = Arc::new(AtomicUsize::new(0));
		let actions = Arc::new(Mutex::new(Vec::new()));
		let context =
			ExecutionContext::new(ExecutionBudget { max_attempts: 2, ..ExecutionBudget::default() });
		let mut service =
			AttemptService { inner: RefreshOnce { calls: calls.clone(), actions: actions.clone() } };
		futures::future::poll_fn(|cx| service.poll_ready(cx))
			.await
			.unwrap();
		service
			.call(LayerCall { payload: (), context: context.clone() })
			.await
			.unwrap();
		assert_eq!(calls.load(Ordering::SeqCst), 2);
		assert_eq!(actions.lock()[0], AttemptAction::Initial);
		assert_eq!(actions.lock()[1], AttemptAction::RefreshCredential {
			previous_account: Some(AccountId::from("account")),
		});
		assert_eq!(context.receipt().attempts.len(), 1);
		assert!(context.receipt().attempts[0].hidden);
	}

	#[derive(Clone)]
	struct RefreshTwice {
		calls:   Arc<AtomicUsize>,
		actions: Arc<Mutex<Vec<AttemptAction>>>,
	}
	impl Service<LayerCall<()>> for RefreshTwice {
		type Error = Error;
		type Future = Ready<Result<(), Error>>;
		type Response = ();

		fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Error>> {
			Poll::Ready(Ok(()))
		}

		fn call(&mut self, request: LayerCall<()>) -> Self::Future {
			self.actions.lock().push(request.context.attempt_action());
			let index = self.calls.fetch_add(1, Ordering::SeqCst) as u32;
			ready(Err(refresh_error(index, RetryAction::RefreshCredential)))
		}
	}

	fn refresh_error(index: u32, action: RetryAction) -> Error {
		let mut receipt = ExecutionReceipt::default();
		receipt.record_attempt(AttemptReceipt {
			index,
			hidden: false,
			provider: None,
			route: None,
			account: Some(AccountId::from("account")),
			principal: None,
			body: AttemptBodyEvidence {
				opened:         true,
				consumed:       true,
				replayability:  Replayability::Replayable,
				retry_decision: RetryDecision::Allow,
				reason:         RetryDecisionReason::ReplayableSource,
			},
			outcome: AttemptOutcome::FailedPreCommit,
			usage: Usage { input_tokens: 1, ..Usage::default() },
			cost: Cost::from_micro_usd(1),
			provider_evidence: ProviderEvidence::default(),
			elapsed: Duration::ZERO,
		});
		Error::new(ErrorKind::Authentication, ErrorPhase::Authentication, action, receipt)
	}

	#[derive(Clone)]
	struct RefreshSequence {
		calls:    Arc<AtomicUsize>,
		actions:  Arc<Mutex<Vec<AttemptAction>>>,
		failures: Arc<[RetryAction]>,
	}
	impl Service<LayerCall<()>> for RefreshSequence {
		type Error = Error;
		type Future = Ready<Result<(), Error>>;
		type Response = ();

		fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Error>> {
			Poll::Ready(Ok(()))
		}

		fn call(&mut self, request: LayerCall<()>) -> Self::Future {
			self.actions.lock().push(request.context.attempt_action());
			let index = self.calls.fetch_add(1, Ordering::SeqCst);
			match self.failures.get(index) {
				Some(action) => ready(Err(refresh_error(index as u32, action.clone()))),
				None => ready(Ok(())),
			}
		}
	}

	#[tokio::test]
	async fn fail_then_fail_merges_attempts_and_charges_once() {
		let calls = Arc::new(AtomicUsize::new(0));
		let actions = Arc::new(Mutex::new(Vec::new()));
		let context =
			ExecutionContext::new(ExecutionBudget { max_attempts: 2, ..ExecutionBudget::default() });
		let mut service = AttemptService { inner: RefreshTwice { calls: calls.clone(), actions } };
		futures::future::poll_fn(|cx| service.poll_ready(cx))
			.await
			.unwrap();
		let error = service
			.call(LayerCall { payload: (), context })
			.await
			.unwrap_err();
		assert_eq!(calls.load(Ordering::SeqCst), 2);
		assert_eq!(
			error
				.receipt()
				.attempts
				.iter()
				.map(|attempt| attempt.index)
				.collect::<Vec<_>>(),
			vec![0, 1]
		);
		assert_eq!(error.receipt().usage.input_tokens, 2);
		assert_eq!(error.receipt().cost.micro_usd, 2);
	}

	#[tokio::test]
	async fn explicit_refresh_replays_once_without_rotating() {
		let calls = Arc::new(AtomicUsize::new(0));
		let actions = Arc::new(Mutex::new(Vec::new()));
		let failures: Arc<[RetryAction]> =
			Arc::from([RetryAction::RefreshCredentialOnce, RetryAction::RefreshCredentialOnce]);
		let context =
			ExecutionContext::new(ExecutionBudget { max_attempts: 3, ..ExecutionBudget::default() });
		let mut service = AttemptService {
			inner: RefreshSequence { calls: calls.clone(), actions: actions.clone(), failures },
		};
		futures::future::poll_fn(|cx| service.poll_ready(cx))
			.await
			.expect("ready");
		service
			.call(LayerCall { payload: (), context })
			.await
			.expect_err("second explicit refresh request must surface");
		assert_eq!(calls.load(Ordering::SeqCst), 2);
		assert_eq!(actions.lock().as_slice(), [
			AttemptAction::Initial,
			AttemptAction::RefreshCredential { previous_account: Some(AccountId::from("account")) },
		]);
	}

	#[tokio::test]
	async fn explicit_refresh_is_still_honored_after_401_refresh() {
		let calls = Arc::new(AtomicUsize::new(0));
		let actions = Arc::new(Mutex::new(Vec::new()));
		let failures: Arc<[RetryAction]> =
			Arc::from([RetryAction::RefreshCredential, RetryAction::RefreshCredentialOnce]);
		let context =
			ExecutionContext::new(ExecutionBudget { max_attempts: 3, ..ExecutionBudget::default() });
		let mut service = AttemptService {
			inner: RefreshSequence { calls: calls.clone(), actions: actions.clone(), failures },
		};
		futures::future::poll_fn(|cx| service.poll_ready(cx))
			.await
			.expect("ready");
		service
			.call(LayerCall { payload: (), context })
			.await
			.expect("forced refresh succeeds");
		assert_eq!(calls.load(Ordering::SeqCst), 3);
		assert_eq!(actions.lock().as_slice(), [
			AttemptAction::Initial,
			AttemptAction::RefreshCredential { previous_account: Some(AccountId::from("account")) },
			AttemptAction::RefreshCredential { previous_account: Some(AccountId::from("account")) },
		]);
	}

	#[tokio::test]
	async fn persistent_401_refreshes_same_account_then_rotates_a_sibling() {
		let calls = Arc::new(AtomicUsize::new(0));
		let actions = Arc::new(Mutex::new(Vec::new()));
		let context =
			ExecutionContext::new(ExecutionBudget { max_attempts: 3, ..ExecutionBudget::default() });
		let mut service =
			AttemptService { inner: RefreshTwice { calls: calls.clone(), actions: actions.clone() } };
		futures::future::poll_fn(|cx| service.poll_ready(cx))
			.await
			.expect("ready");
		service
			.call(LayerCall { payload: (), context })
			.await
			.expect_err("persistent rejection");
		assert_eq!(calls.load(Ordering::SeqCst), 3);
		assert_eq!(actions.lock().as_slice(), [
			AttemptAction::Initial,
			AttemptAction::RefreshCredential { previous_account: Some(AccountId::from("account")) },
			AttemptAction::RotateAccount { previous_account: Some(AccountId::from("account")) },
		]);
	}
	/// Fails its first call with `provider` after reserving a wire attempt,
	/// then every re-entry in authentication before sending anything, for
	/// `reason`: a rotation with no other account or a refresh of a static key
	/// cannot start, any other reason is the re-entry's own failure.
	#[derive(Clone)]
	struct ProviderThenUnsent {
		calls:    Arc<AtomicUsize>,
		provider: (ErrorKind, u16, RetryAction),
		reason:   CredentialFailure,
		unsent:   RetryAction,
	}
	impl Service<LayerCall<()>> for ProviderThenUnsent {
		type Error = Error;
		type Future = Ready<Result<(), Error>>;
		type Response = ();

		fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Error>> {
			Poll::Ready(Ok(()))
		}

		fn call(&mut self, request: LayerCall<()>) -> Self::Future {
			if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
				let index = request.context.reserve_attempt().expect("attempt");
				let (kind, status, action) = self.provider.clone();
				let mut error = refresh_error(index, action);
				error.kind = kind;
				return ready(Err(error.status(Some(status))));
			}
			ready(Err(Error::credential(
				crate::catalog::ProviderId::from("provider"),
				self.reason,
				self.unsent.clone(),
				request.context.receipt(),
			)))
		}
	}

	/// Calls `service` once and returns its failure.
	async fn failure(mut service: AttemptService<ProviderThenUnsent>) -> Error {
		let context =
			ExecutionContext::new(ExecutionBudget { max_attempts: 3, ..ExecutionBudget::default() });
		futures::future::poll_fn(|cx| service.poll_ready(cx))
			.await
			.expect("ready");
		service
			.call(LayerCall { payload: (), context })
			.await
			.expect_err("the request fails")
	}

	/// A rotation (402) or refresh (401) that cannot start reports the
	/// provider's answer, not the credential failure the retry ran into.
	#[tokio::test]
	async fn unsent_reentry_reports_the_provider_failure_that_caused_it() {
		for (kind, status, action, reason, unsent) in [
			(
				ErrorKind::PaymentRequired,
				402,
				RetryAction::RotateAccount,
				CredentialFailure::NoRotationCandidate,
				RetryAction::ReselectRoute,
			),
			(
				ErrorKind::Authentication,
				401,
				RetryAction::RefreshCredential,
				CredentialFailure::NotRenewable,
				RetryAction::Never,
			),
		] {
			let calls = Arc::new(AtomicUsize::new(0));
			let error = failure(AttemptService {
				inner: ProviderThenUnsent {
					calls: calls.clone(),
					provider: (kind, status, action),
					reason,
					unsent: unsent.clone(),
				},
			})
			.await;
			assert_eq!(calls.load(Ordering::SeqCst), 2, "one re-entry ran");
			assert_eq!(error.kind, kind);
			assert_eq!(error.status, Some(status));
			// Route reselection stays open when the re-entry asked for it, so a
			// planned fallback to another provider still runs.
			assert_eq!(error.action, unsent);
			assert_eq!(error.receipt().attempts.len(), 1);
			assert!(!error.receipt().attempts[0].hidden, "the failing attempt is the visible one");
		}
	}

	/// A re-entry that started and failed for a reason of its own (the next
	/// account's stored credential has the wrong kind, a refresh that ran
	/// failed) reports that reason, which the provider's answer does not name.
	#[tokio::test]
	async fn reentry_failing_for_its_own_reason_reports_it() {
		for (action, reason) in [
			(RetryAction::RotateAccount, CredentialFailure::KindMismatch {
				expected: crate::auth::CredentialKind::Bearer,
				actual:   crate::auth::CredentialKind::ApiKey,
				remedy:   crate::KindMismatchRemedy::Restart,
			}),
			(RetryAction::RefreshCredential, CredentialFailure::RefreshFailed),
		] {
			let error = failure(AttemptService {
				inner: ProviderThenUnsent {
					calls: Arc::new(AtomicUsize::new(0)),
					provider: (ErrorKind::PaymentRequired, 402, action),
					reason,
					unsent: RetryAction::ReselectRoute,
				},
			})
			.await;
			assert_eq!(error.kind, ErrorKind::Authentication, "{reason:?}");
			assert_eq!(error.status, None, "{reason:?}");
			assert!(
				matches!(
					error.detail_ref(),
					Some(ErrorDetail::Credential { reason: surfaced, .. }) if *surfaced == reason
				),
				"{reason:?}"
			);
		}
	}

	/// A re-entry that reached the wire reports its own failure.
	#[tokio::test]
	async fn sent_reentry_reports_its_own_failure() {
		let calls = Arc::new(AtomicUsize::new(0));
		let failures: Arc<[RetryAction]> =
			Arc::from([RetryAction::RotateAccount, RetryAction::Never]);
		let context =
			ExecutionContext::new(ExecutionBudget { max_attempts: 3, ..ExecutionBudget::default() });
		let mut service = AttemptService {
			inner: RefreshSequence { calls: calls.clone(), actions: Arc::default(), failures },
		};
		futures::future::poll_fn(|cx| service.poll_ready(cx))
			.await
			.expect("ready");
		let error = service
			.call(LayerCall { payload: (), context })
			.await
			.expect_err("second failure surfaces");
		assert_eq!(calls.load(Ordering::SeqCst), 2);
		assert_eq!(error.action, RetryAction::Never);
		assert_eq!(error.receipt().attempts.last().map(|attempt| attempt.index), Some(1));
	}

	#[tokio::test]
	async fn prebody_auth_reselection_reaches_registry_unchanged() {
		let inner = service_fn(|_: LayerCall<()>| async {
			Err::<(), _>(Error::new(
				ErrorKind::Authentication,
				ErrorPhase::Authentication,
				RetryAction::ReselectRoute,
				ExecutionReceipt::default(),
			))
		});
		let mut service = AttemptService { inner };
		let error = service
			.call(LayerCall {
				payload: (),
				context: ExecutionContext::new(ExecutionBudget::default()),
			})
			.await
			.unwrap_err();
		assert_eq!(error.action, RetryAction::ReselectRoute);
		assert!(error.receipt().attempts.is_empty());
	}
}
