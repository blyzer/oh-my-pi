//! Per-connection invocation, editor, and exec state.

use super::{
	ACP_MAX_PENDING, ConnectionAcpDocumentRoute, ConnectionEditRepairRoute, ConnectionState,
	EDIT_REPAIR_TIMEOUT, Finished, InvocationAcpRoutes, InvocationRefused, InvocationState,
	NATIVE_COMMITTED, NATIVE_TERMINAL, NativeCommitError, NativeLifecycle, PendingEditRepair,
	RequestState, VerdictDelivery, WRITE_BACK_REQUESTS,
	control_factories::ProductionResolverTable,
	forward::{send_abort_verdict, send_error, server_frame},
	identity::{AcceptedHello, ConnectionPolicy},
	prelude::*,
};

impl ConnectionEditRepairRoute {
	pub(in crate::server) fn client(&self) -> omp_tools::edit::observer::EditRepairClient {
		let route = self.clone();
		omp_tools::edit::observer::EditRepairClient::from_completion(move |prompt| {
			let route = route.clone();
			async move { route.complete(prompt).await }
		})
	}

	pub(in crate::server) async fn complete(
		&self,
		prompt: omp_tools::edit::observer::EditRepairPrompt,
	) -> Result<Str, omp_tools::edit::observer::EditRepairError> {
		use omp_tools::edit::observer::EditRepairError;

		let (reply, response) = flume::bounded(1);
		let generation = self.next_query.fetch_add(1, Ordering::Relaxed);
		{
			let mut pending = self.pending.lock();
			if pending.is_some() {
				return Err(EditRepairError::Unavailable);
			}
			*pending = Some(PendingEditRepair { generation, reply });
		}
		let query = pb::EditRepairQuery {
			invocation_id: self.invocation_id.to_string(),
			prompt:        Some(pb::EditRepairPrompt {
				language:         prompt.language.to_string(),
				before:           prompt.before.to_string(),
				after:            prompt.after.to_string(),
				previous_attempt: prompt.previous_attempt.map(|attempt| attempt.to_string()),
			}),
		};
		if self
			.responses
			.send_async(server_frame(self.request_id, server_frame::Body::EditRepairQuery(query)))
			.await
			.is_err()
		{
			self.disconnect();
			return Err(EditRepairError::Unavailable);
		}
		let result = match time::timeout(EDIT_REPAIR_TIMEOUT, response.recv_async()).await {
			Ok(Ok(result)) => result,
			Ok(Err(_)) | Err(_) => Err(EditRepairError::Unavailable),
		};
		let mut pending = self.pending.lock();
		if pending
			.as_ref()
			.is_some_and(|pending| pending.generation == generation)
		{
			pending.take();
		}
		result
	}

	pub(in crate::server) fn answer(
		&self,
		answer: pb::EditRepairAnswer,
	) -> Result<(), (pb::ProtocolErrorCode, &'static str)> {
		use omp_tools::edit::observer::EditRepairError;

		if answer.invocation_id != self.invocation_id.as_str() {
			return Err((
				pb::ProtocolErrorCode::InvalidArgument,
				"edit repair answer invocation_id does not match the open request",
			));
		}
		let result = match answer.body {
			Some(pb::edit_repair_answer::Body::Content(content)) => Ok(Str::from(content)),
			Some(pb::edit_repair_answer::Body::Failure(failure)) => {
				match pb::EditRepairFailureCode::try_from(failure.code) {
					Ok(pb::EditRepairFailureCode::Unavailable) => Err(EditRepairError::Unavailable),
					Ok(pb::EditRepairFailureCode::Completion) => {
						Err(EditRepairError::Completion { message: Str::from(failure.message) })
					},
					Ok(pb::EditRepairFailureCode::Unspecified) | Err(_) => {
						return Err((
							pb::ProtocolErrorCode::InvalidArgument,
							"edit repair answer failure code is unknown or unspecified",
						));
					},
				}
			},
			None => {
				return Err((
					pb::ProtocolErrorCode::InvalidArgument,
					"edit repair answer body is missing",
				));
			},
		};
		let Some(pending) = self.pending.lock().take() else {
			return Err((
				pb::ProtocolErrorCode::PreconditionFailed,
				"no edit repair query is pending for this invocation",
			));
		};
		pending.reply.send(result).map_err(|_| {
			(pb::ProtocolErrorCode::PreconditionFailed, "edit repair query is no longer pending")
		})
	}

	pub(in crate::server) fn disconnect(&self) {
		if let Some(pending) = self.pending.lock().take() {
			let _ = pending
				.reply
				.send(Err(omp_tools::edit::observer::EditRepairError::Unavailable));
		}
	}
}

impl ConnectionAcpDocumentRoute {
	pub(in crate::server) async fn query(
		&self,
		body: impl FnOnce(u64) -> server_frame::Body,
		wait: Duration,
	) -> Result<pb::acp_document_answer::Body, EditorIoError> {
		let query_id = self.next_query.fetch_add(1, Ordering::Relaxed);
		let (reply, answer) = flume::bounded(1);
		{
			let mut pending = self.pending.lock();
			if pending.len() >= ACP_MAX_PENDING {
				return Err(EditorIoError::Busy);
			}
			pending.insert(query_id, reply);
		}
		if self
			.responses
			.send_async(pb::ServerFrame {
				request_id: self.request_id,
				body: Some(body(query_id)),
				..pb::ServerFrame::default()
			})
			.await
			.is_err()
		{
			self.pending.lock().remove(&query_id);
			return Err(EditorIoError::Disconnected);
		}
		let result = match time::timeout(wait, answer.recv_async()).await {
			Ok(Ok(result)) => result,
			Ok(Err(_)) => Err(EditorIoError::Disconnected),
			Err(_) => Err(EditorIoError::Timeout),
		};
		self.pending.lock().remove(&query_id);
		result
	}

	pub(in crate::server) async fn read(&self, path: Str) -> Result<Str, EditorIoError> {
		let invocation_id = self.invocation_id.to_string();
		let body = move |query_id| {
			server_frame::Body::AcpReadQuery(pb::AcpReadQuery {
				query_id,
				invocation_id,
				path: path.to_string(),
			})
		};
		match self.query(body, self.deadline).await? {
			pb::acp_document_answer::Body::Content(content) => Ok(Str::from(content)),
			_ => Err(EditorIoError::Malformed),
		}
	}

	pub(in crate::server) async fn write_back(
		&self,
		write_back: WriteBack,
	) -> Result<WriteBackOutcome, EditorIoError> {
		let invocation_id = self.invocation_id.to_string();
		let body = move |query_id| {
			server_frame::Body::AcpWriteQuery(pb::AcpWriteQuery {
				query_id,
				invocation_id,
				path: write_back.path.to_string(),
				content: write_back.content.to_string(),
				base: write_back.base.map(|base| base.to_string()),
			})
		};
		let wait = self.deadline.saturating_mul(WRITE_BACK_REQUESTS);
		match self.query(body, wait).await? {
			pb::acp_document_answer::Body::WriteBack(outcome) => WriteBackOutcome::from_wire(outcome),
			_ => Err(EditorIoError::Malformed),
		}
	}

	pub(in crate::server) fn answer(
		&self,
		answer: pb::AcpDocumentAnswer,
	) -> Result<(), (pb::ProtocolErrorCode, &'static str)> {
		if answer.invocation_id.as_str() != self.invocation_id.as_str() {
			return Err((
				pb::ProtocolErrorCode::InvalidArgument,
				"ACP document answer invocation_id does not match the open request",
			));
		}
		let Some(reply) = self.pending.lock().remove(&answer.query_id) else {
			return Err((
				pb::ProtocolErrorCode::PreconditionFailed,
				"ACP document query is not pending",
			));
		};
		let result = match answer.body {
			Some(pb::acp_document_answer::Body::Error(error)) => {
				Err(EditorIoError::from_protocol_code(error.code))
			},
			Some(body) => Ok(body),
			None => Err(EditorIoError::Malformed),
		};
		reply.send(result).map_err(|_| {
			(pb::ProtocolErrorCode::PreconditionFailed, "ACP document query is no longer pending")
		})
	}

	pub(in crate::server) fn disconnect(&self) {
		self.pending.lock().clear();
	}
}

impl AcpDocumentBackend for ConnectionAcpDocumentRoute {
	fn deadline(&self) -> Duration {
		self.deadline
	}

	fn capabilities(&self) -> EditorCapabilities {
		self.capabilities
	}

	fn read_text(
		&self,
		absolute_path: Str,
	) -> pin::Pin<Box<dyn future::Future<Output = Result<Str, EditorIoError>> + Send + '_>> {
		Box::pin(self.read(absolute_path))
	}

	fn write_back(
		&self,
		write_back: WriteBack,
	) -> pin::Pin<
		Box<dyn future::Future<Output = Result<WriteBackOutcome, EditorIoError>> + Send + '_>,
	> {
		Box::pin(self.write_back(write_back))
	}
}

impl InvocationAcpRoutes {
	pub(in crate::server) fn disconnect(&self) {
		if let Some((route, _)) = &self.documents {
			route.disconnect();
		}
	}

	pub(in crate::server) fn context(&self) -> crate::tools::InvocationAcpBackends {
		crate::tools::InvocationAcpBackends::new(self.documents.as_ref().map(|(route, session)| {
			EditorRoute::new(Arc::clone(route) as Arc<dyn AcpDocumentBackend>, Arc::clone(session))
		}))
	}
}

impl NativeLifecycle {
	pub(in crate::server) fn commit(&self) -> Result<(), NativeCommitError> {
		self
			.state
			.try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
				(state & (NATIVE_COMMITTED | NATIVE_TERMINAL) == 0).then_some(state | NATIVE_COMMITTED)
			})
			.map(|_| ())
			.map_err(|state| {
				if state & NATIVE_COMMITTED != 0 {
					NativeCommitError::AlreadyCommitted
				} else {
					NativeCommitError::Terminal
				}
			})
	}

	pub(in crate::server) fn is_committed(&self) -> bool {
		self.state.load(Ordering::Acquire) & NATIVE_COMMITTED != 0
	}

	pub(in crate::server) fn is_terminal(&self) -> bool {
		self.state.load(Ordering::Acquire) & NATIVE_TERMINAL != 0
	}

	pub(in crate::server) fn claim_terminal(&self) -> bool {
		self
			.state
			.try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
				(state & NATIVE_TERMINAL == 0).then_some(state | NATIVE_TERMINAL)
			})
			.is_ok()
	}

	pub(in crate::server) fn claim_precommit_terminal(&self) -> bool {
		self
			.state
			.compare_exchange(0, NATIVE_TERMINAL, Ordering::AcqRel, Ordering::Acquire)
			.is_ok()
	}
}

impl ConnectionState {
	pub(in crate::server) fn new(
		exec_host: ExecHost,
		hello: AcceptedHello,
		base_settings: &ToolSettings,
		authority: Arc<AuthorityTable>,
		policy: &ConnectionPolicy,
	) -> Self {
		let connection_owner = authority.connection_owner();
		let owner = policy.host.as_ref().map_or_else(
			|| sf!("env-connection-{connection_owner}"),
			|host| {
				let [layer, tier, extension] = host.fields();
				sf!("extension:{layer}:{tier}:{extension}")
			},
		);
		let quotas = QuotaAccount::new(Arc::clone(&authority), policy.host.clone());
		let tool_settings = base_settings
			.clone()
			.with_approval_mode_override(hello.approval_mode);
		Self {
			owner,
			requests: HashMap::new(),
			invocation_ids: HashMap::new(),
			tool_settings,
			exec_host,
			document_leases: HashMap::new(),
			grants: hello.grants,
			capabilities: hello.capabilities,
			hello_props: hello.props,
			acp_documents: None,
			approvals: None,
			reflection: None,
			host: policy.host.clone(),
			authority,
			connection_owner,
			quotas,
			presence: None,
		}
	}

	pub(in crate::server) fn next_admission_deadline(&self) -> Option<Instant> {
		self
			.requests
			.values()
			.filter_map(|state| match state {
				RequestState::Invocation(invocation) => invocation.pending_admission_deadline(),
				_ => None,
			})
			.min()
	}

	pub(in crate::server) fn take_expired_admissions(
		&mut self,
	) -> Vec<(u64, Str, VerdictDelivery, policy_pb::PolicyDenied)> {
		let now = Instant::now();
		self
			.requests
			.iter_mut()
			.filter_map(|(request_id, state)| match state {
				RequestState::Invocation(invocation) => {
					invocation.expire_admission(now).map(|denied| {
						(*request_id, Str::from(invocation.id()), invocation.delivery().clone(), denied)
					})
				},
				_ => None,
			})
			.collect()
	}

	pub(in crate::server) fn grants(&self, capability: &str) -> bool {
		self.grants.contains(capability)
	}

	pub(in crate::server) fn supports_edit_repair(&self) -> bool {
		self.capabilities.contains(omp_env::EDIT_REPAIR_CAPABILITY)
	}

	/// Relays the approval prompts of this connection's commands, and the
	/// memory reflections of its `reflect` calls, to it: each when it is an
	/// application connection that advertised that capability.
	///
	/// Extension connections never get a relay: extension code would then
	/// approve the amendments of its own commands, or answer its own
	/// reflections.
	pub(in crate::server) fn bind_relays(&mut self, responses: &flume::Sender<pb::ServerFrame>) {
		if self.host.is_some() {
			return;
		}
		if self
			.capabilities
			.contains(omp_env::APPROVAL_RELAY_CAPABILITY)
		{
			self.approvals = Some(ConnectionApprovals::new(responses.clone()));
		}
		if self
			.capabilities
			.contains(omp_env::REFLECTION_RELAY_CAPABILITY)
		{
			self.reflection = Some(ConnectionReflection::new(responses.clone()));
		}
	}

	/// The owner a native call or a command issued under `principal` runs
	/// as: the durable session and agent principals when the scope
	/// authenticates them, else this connection's own identity.
	pub(in crate::server) fn invocation_owner(
		&self,
		principal: Option<&pb::InvocationScope>,
	) -> Str {
		principal.map_or_else(
			|| self.owner.clone(),
			|principal| {
				Str::from(
					serde_json::to_string(&(&principal.session_id, &principal.agent_id))
						.expect("invocation principal tuple is always serializable"),
				)
			},
		)
	}

	/// This connection's relay bound to the request that issues a command.
	pub(in crate::server) fn owned_approvals(&self, request_id: u64) -> Option<OwnedApprovals> {
		self
			.approvals
			.as_ref()
			.map(|approvals| approvals.owned(request_id))
	}

	/// This connection's reflection relay bound to the request that issues a
	/// call or a command.
	pub(in crate::server) fn owned_reflection(&self, request_id: u64) -> Option<OwnedReflection> {
		self
			.reflection
			.as_ref()
			.map(|reflection| reflection.owned(request_id))
	}

	pub(in crate::server) fn edit_model(&self) -> Option<Str> {
		self
			.hello_props
			.as_ref()?
			.fields
			.get("edit-model")
			.and_then(|value| match value.kind.as_ref() {
				Some(value::Kind::String(model)) if !model.is_empty() => {
					Some(Str::from(model.as_str()))
				},
				_ => None,
			})
	}

	pub(in crate::server) fn bind_acp(&mut self, binding: pb::AcpBind) {
		self.acp_documents = binding.documents.then(|| {
			Arc::new(EditorSession::new(
				Duration::from_millis(binding.fs_timeout_ms),
				EditorCapabilities { read: binding.read_text, write: binding.write_text },
			))
		});
	}

	pub(in crate::server) fn acp_routes(
		&self,
		request_id: u64,
		invocation_id: &Str,
		responses: &flume::Sender<pb::ServerFrame>,
	) -> InvocationAcpRoutes {
		InvocationAcpRoutes {
			documents: self.acp_documents.as_ref().map(|session| {
				let route = Arc::new(ConnectionAcpDocumentRoute {
					request_id,
					invocation_id: invocation_id.clone(),
					responses: responses.clone(),
					next_query: Arc::new(AtomicU64::new(1)),
					pending: Arc::new(Mutex::new(HashMap::new())),
					deadline: session.deadline(),
					capabilities: session.capabilities(),
				});
				(route, Arc::clone(session))
			}),
		}
	}

	pub(in crate::server) fn answer_acp_document(
		&self,
		request_id: u64,
		answer: pb::AcpDocumentAnswer,
	) -> Result<(), (pb::ProtocolErrorCode, &'static str)> {
		match self.requests.get(&request_id) {
			Some(RequestState::Invocation(InvocationState::Native { id, acp, .. }))
				if id == answer.invocation_id.as_str() =>
			{
				acp.documents
					.as_ref()
					.map(|(route, _)| route)
					.ok_or((
						pb::ProtocolErrorCode::PreconditionFailed,
						"this invocation has no ACP document route",
					))?
					.answer(answer)
			},
			Some(RequestState::Invocation(InvocationState::Native { .. })) => Err((
				pb::ProtocolErrorCode::InvalidArgument,
				"ACP document answer invocation_id does not match the open request",
			)),
			Some(RequestState::Invocation(_)) => Err((
				pb::ProtocolErrorCode::PreconditionFailed,
				"ACP document answers are only valid for native invocations",
			)),
			Some(_) => Err((
				pb::ProtocolErrorCode::PreconditionFailed,
				"request_id is not an invocation stream",
			)),
			None => Err((pb::ProtocolErrorCode::NotFound, "invocation is not open")),
		}
	}

	pub(in crate::server) fn answer_edit_repair(
		&self,
		request_id: u64,
		answer: pb::EditRepairAnswer,
	) -> Result<(), (pb::ProtocolErrorCode, &'static str)> {
		match self.requests.get(&request_id) {
			Some(RequestState::Invocation(InvocationState::Native {
				id,
				edit_repair: Some(route),
				..
			})) if id == answer.invocation_id.as_str() => route.answer(answer),
			Some(RequestState::Invocation(InvocationState::Native { id, .. }))
				if id != answer.invocation_id.as_str() =>
			{
				Err((
					pb::ProtocolErrorCode::InvalidArgument,
					"edit repair answer invocation_id does not match the open request",
				))
			},
			Some(RequestState::Invocation(InvocationState::Native { .. })) => Err((
				pb::ProtocolErrorCode::PreconditionFailed,
				"this invocation has no edit repair route",
			)),
			Some(RequestState::Invocation(_)) => Err((
				pb::ProtocolErrorCode::PreconditionFailed,
				"edit repair answers are only valid for native invocations",
			)),
			Some(_) => Err((
				pb::ProtocolErrorCode::PreconditionFailed,
				"request_id is not an invocation stream",
			)),
			None => Err((pb::ProtocolErrorCode::NotFound, "invocation is not open")),
		}
	}

	pub(in crate::server) fn invocation_mut(
		&mut self,
		request_id: u64,
		invocation_id: &str,
	) -> Result<&mut InvocationState, (pb::ProtocolErrorCode, &'static str)> {
		match self.requests.get_mut(&request_id) {
			Some(RequestState::Invocation(state)) if state.id() == invocation_id => Ok(state),
			Some(RequestState::Invocation(_)) => Err((
				pb::ProtocolErrorCode::InvalidArgument,
				"invocation_id does not match the open request",
			)),
			Some(_) => Err((
				pb::ProtocolErrorCode::PreconditionFailed,
				"request_id is not an invocation stream",
			)),
			None => Err((pb::ProtocolErrorCode::NotFound, "invocation is not open")),
		}
	}

	pub(in crate::server) fn scope_authenticates(
		&self,
		request_id: u64,
		invocation_id: &str,
		effect_token: &[u8],
		scope: Option<&pb::InvocationScope>,
	) -> Result<bool, (pb::ProtocolErrorCode, &'static str)> {
		let state = match self.requests.get(&request_id) {
			Some(RequestState::Invocation(state)) if state.id() == invocation_id => state,
			Some(RequestState::Invocation(_)) => {
				return Err((
					pb::ProtocolErrorCode::InvalidArgument,
					"invocation_id does not match the open request",
				));
			},
			Some(_) => {
				return Err((
					pb::ProtocolErrorCode::PreconditionFailed,
					"request_id is not an invocation stream",
				));
			},
			None => return Err((pb::ProtocolErrorCode::NotFound, "invocation is not open")),
		};
		let expected = match state {
			InvocationState::Native { request_scope, .. }
			| InvocationState::Worker { request_scope, .. } => *request_scope,
		};
		Ok(match (expected, scope) {
			(None, None) => true,
			(Some(expected), Some(scope)) => {
				scope.invocation_id == invocation_id
					&& !effect_token.is_empty()
					&& scope.effect_token.as_ref() == effect_token
					&& scope.pty_denied == expected
			},
			(None, Some(_)) | (Some(_), None) => false,
		})
	}

	/// Judges the open invocation by its committed arguments `raw`, once, after
	/// its gate staged them ([`AdmissionGate::stage`]) and before their query
	/// is emitted: narrows its envelope to the call's argument-scoped effects
	/// ([`Registry::invocation_effects`]), resolves a pending approval policy
	/// from that envelope, and returns the call's refusal at the write boundary
	/// ([`InvocationExecutionPolicy::denial`]), or of an envelope beyond the
	/// declared maximum, if any. When that envelope fetches, the hosts the
	/// call's fetch locators reach ([`Registry::fetch_locators`]) are named by
	/// the resolvers in `resources` that perform them, for the query: every
	/// host they can name, and whether some locator reaches one they cannot.
	/// A native call is judged inside the pins its executor runs in
	/// ([`InvocationPins`]), so the hosts named and the envelope judged are the
	/// ones it reaches.
	///
	/// Staged arguments are one JSON object, so they are UTF-8; were they not,
	/// the call would keep its declared maximum.
	pub(in crate::server) fn scope_committed(
		&mut self,
		request_id: u64,
		invocation_id: &str,
		registry: &Registry,
		resources: &ProductionResolverTable,
		raw: &[u8],
	) -> Result<Option<InvocationRefused>, (pb::ProtocolErrorCode, &'static str)> {
		let sandbox = self.exec_host.sandbox_state();
		let settings = &self.tool_settings;
		let (id, admission, effects, confinement, execution, pins) =
			match self.requests.get_mut(&request_id) {
				Some(RequestState::Invocation(state)) if state.id() == invocation_id => match state {
					InvocationState::Native {
						id,
						admission,
						effects,
						confinement,
						execution,
						pins,
						..
					} => (id, admission, effects, *confinement, execution, pins.as_ref()),
					InvocationState::Worker {
						id, admission, effects, confinement, execution, ..
					} => (id, admission, effects, *confinement, execution, None),
				},
				Some(RequestState::Invocation(_)) => {
					return Err((
						pb::ProtocolErrorCode::InvalidArgument,
						"invocation_id does not match the open request",
					));
				},
				Some(_) => {
					return Err((
						pb::ProtocolErrorCode::PreconditionFailed,
						"request_id is not an invocation stream",
					));
				},
				None => return Err((pb::ProtocolErrorCode::NotFound, "invocation is not open")),
			};
		// Only the arguments the gate staged scope the call. A commit the gate
		// refused never reaches here, and a repeated commit after the query
		// stages nothing, so it never rescopes the call; it is refused later.
		if admission.is_staged() {
			if let Ok(raw) = str::from_utf8(raw) {
				let judged = InvocationPins::judge(pins, || {
					let scoped = registry.invocation_effects(&execution.tool, raw)?;
					let named = scoped.fetch.is_some().then(|| {
						let locators = registry.fetch_locators(&execution.tool, raw);
						resolve_locators(Some(resources), &locators)
					});
					Ok((scoped, named))
				});
				match judged {
					Ok((scoped, named)) => {
						*effects = scoped;
						if let Some(named) = named {
							admission.name_fetch_hosts(named);
						}
					},
					Err(source) => return Ok(Some(InvocationRefused::Effects { source })),
				}
			}
			if admission.is_pending() {
				let resolved = settings.approval_for(
					id.clone(),
					execution.tool.clone(),
					effects,
					confinement,
					sandbox,
				);
				admission.resolve_pending(resolved.policy);
			}
		}
		Ok(execution
			.denial(effects, confinement)
			.map(|source| InvocationRefused::WriteBoundary { source }))
	}

	/// Removes a denied pre-authorization invocation before its executor sees
	/// finalized arguments.
	pub(in crate::server) fn abandon_admission(&mut self, request_id: u64, invocation_id: &Str) {
		match self.requests.remove(&request_id) {
			Some(RequestState::Invocation(InvocationState::Native {
				lifecycle,
				edit_repair,
				acp,
				cancel,
				..
			})) => {
				if let Some(route) = edit_repair {
					route.disconnect();
				}
				acp.disconnect();
				lifecycle.claim_precommit_terminal();
				cancel.cancel();
			},
			Some(RequestState::Invocation(InvocationState::Worker { owner, id, cancel, .. })) => {
				cancel.cancel();
				self.authority.settle(&owner, &id);
			},
			_ => {},
		}
		self.invocation_ids.remove(invocation_id);
	}

	pub(in crate::server) async fn exec_id(
		&self,
		request_id: u64,
		expected: &[u8],
		responses: &flume::Sender<pb::ServerFrame>,
	) -> Option<Bytes> {
		match self.requests.get(&request_id) {
			Some(RequestState::Exec { exec, .. }) if exec.as_ref() == expected => Some(exec.clone()),
			Some(RequestState::Exec { .. }) => {
				send_error(
					responses,
					request_id,
					pb::ProtocolErrorCode::InvalidArgument,
					"exec id does not match the open request",
				)
				.await;
				None
			},
			Some(_) => {
				send_error(
					responses,
					request_id,
					pb::ProtocolErrorCode::PreconditionFailed,
					"request_id is not an exec stream",
				)
				.await;
				None
			},
			None => {
				send_error(
					responses,
					request_id,
					pb::ProtocolErrorCode::NotFound,
					"execution is not open",
				)
				.await;
				None
			},
		}
	}

	pub(in crate::server) fn finish(&mut self, done: Finished) {
		match self.requests.remove(&done.request_id) {
			Some(RequestState::Invocation(InvocationState::Native { acp, .. })) => {
				acp.disconnect();
			},
			Some(RequestState::Invocation(InvocationState::Worker { owner, id, .. })) => {
				self.authority.settle(&owner, &id);
			},
			Some(RequestState::Exec { .. }) => self.quotas.release_exec(),
			Some(
				RequestState::ProcessAttach { .. }
				| RequestState::BlobGet { .. }
				| RequestState::DataStream { .. }
				| RequestState::DocumentEvents { .. }
				| RequestState::LspEvents { .. },
			) => self.quotas.release_stream(),
			_ => {},
		}
		if let Some(invocation_id) = done.invocation_id {
			self.invocation_ids.remove(&invocation_id);
		}
	}

	pub(in crate::server) async fn cancel(
		&mut self,
		request: pb::CancelRequest,
		exec_host: &ExecHost,
		responses: &flume::Sender<pb::ServerFrame>,
		finished: &flume::Sender<Finished>,
	) {
		use pb::cancel_request::Target;
		match request.target {
			Some(Target::TargetRequestId(request_id)) => {
				if let Some(RequestState::Exec { exec, .. }) = self.requests.get(&request_id) {
					let _ = exec_host.cancel(exec);
				} else {
					self
						.cancel_request(request_id, exec_host, responses, finished)
						.await;
				}
			},
			Some(Target::InvocationId(invocation_id)) => {
				if let Some(request_id) = self.invocation_ids.get(invocation_id.as_str()).copied() {
					self
						.cancel_request(request_id, exec_host, responses, finished)
						.await;
				}
			},
			Some(Target::Exec(exec_id)) => {
				let _ = exec_host.cancel(&exec_id);
			},
			None => {},
		}
	}

	pub(in crate::server) async fn cancel_request(
		&mut self,
		request_id: u64,
		exec_host: &ExecHost,
		responses: &flume::Sender<pb::ServerFrame>,
		finished: &flume::Sender<Finished>,
	) {
		if let Some(RequestState::Invocation(state)) = self.requests.get_mut(&request_id) {
			let delivery = state.delivery().clone();
			let terminal = match state {
				InvocationState::Native { id, feed, lifecycle, edit_repair, acp, cancel, .. } => {
					if let Some(route) = edit_repair {
						route.disconnect();
					}
					acp.disconnect();
					if lifecycle.is_committed() {
						let _ = feed.interrupt(Interrupt {
							class:  sf!("cancel"),
							reason: sf!("invocation cancelled by client"),
						});
						cancel.cancel();
						None
					} else if lifecycle.claim_precommit_terminal() {
						cancel.cancel();
						Some((id.clone(), omp_tool::Abort::Skipped {
							reason: sf!("invocation cancelled before argument commitment"),
						}))
					} else {
						cancel.cancel();
						None
					}
				},
				InvocationState::Worker { id, owner, committed, cancel, .. } => {
					cancel.cancel();
					(!*committed).then(|| {
						self.authority.settle(owner, id);
						(id.clone(), omp_tool::Abort::Skipped {
							reason: sf!("invocation cancelled before argument commitment"),
						})
					})
				},
			};
			if terminal.is_some() {
				self
					.requests
					.insert(request_id, RequestState::InvocationFinishing);
			}
			if let Some((invocation_id, abort)) = terminal {
				send_abort_verdict(responses, request_id, &invocation_id, &delivery, abort).await;
				let _ = finished
					.send_async(Finished { request_id, invocation_id: Some(invocation_id) })
					.await;
			}
			return;
		}
		if matches!(self.requests.get(&request_id), Some(RequestState::InvocationFinishing)) {
			return;
		}

		let Some(state) = self.requests.remove(&request_id) else {
			return;
		};
		match state {
			RequestState::Invocation(_) => unreachable!("invocations were handled without removal"),
			RequestState::InvocationFinishing => {},
			RequestState::Exec { exec, cancel } => {
				let _ = exec_host.cancel(&exec);
				cancel.cancel();
			},
			RequestState::ProcessAttach { cancel }
			| RequestState::BlobGet { cancel }
			| RequestState::DocumentEvents { cancel, .. }
			| RequestState::LspEvents { cancel } => cancel.cancel(),
			RequestState::DataStream { cancel } => {
				cancel.cancel();
				self.quotas.release_stream();
			},
			RequestState::BlobPut(_) => {},
		}
	}

	pub(in crate::server) async fn interrupt(
		&mut self,
		request_id: u64,
		request: pb::Interrupt,
		responses: &flume::Sender<pb::ServerFrame>,
		finished: &flume::Sender<Finished>,
	) {
		let mut settle = None;
		let result = self.invocation_mut(request_id, &request.invocation_id);
		let delivery = result.as_ref().ok().map(|state| state.delivery().clone());
		let terminal = match result {
			Ok(InvocationState::Native { id, feed, lifecycle, edit_repair, acp, cancel, .. }) => {
				if let Some(route) = edit_repair {
					route.disconnect();
				}
				acp.disconnect();
				let reason = Str::from(request.reason);
				let _ = feed.interrupt(Interrupt { class: sf!("immediate"), reason: reason.clone() });
				if lifecycle.is_committed() {
					None
				} else if lifecycle.claim_precommit_terminal() {
					cancel.cancel();
					Some((id.clone(), omp_tool::Abort::Interrupted { reason }))
				} else {
					cancel.cancel();
					None
				}
			},
			Ok(InvocationState::Worker { id, owner, committed, cancel, interrupt, .. }) => {
				let reason = Str::from(request.reason.as_str());
				if *committed {
					let _ = interrupt.send(request);
					None
				} else {
					settle = Some((owner.clone(), id.clone()));
					cancel.cancel();
					Some((id.clone(), omp_tool::Abort::Interrupted { reason }))
				}
			},
			Err((code, message)) => {
				send_error(responses, request_id, code, message).await;
				return;
			},
		};
		if let Some((owner, invocation_id)) = settle {
			self.authority.settle(&owner, &invocation_id);
		}
		if terminal.is_some() {
			self
				.requests
				.insert(request_id, RequestState::InvocationFinishing);
		}
		if let (Some((invocation_id, abort)), Some(delivery)) = (terminal, delivery) {
			send_abort_verdict(responses, request_id, &invocation_id, &delivery, abort).await;
			let _ = finished
				.send_async(Finished { request_id, invocation_id: Some(invocation_id) })
				.await;
		}
	}

	pub(in crate::server) fn cancel_all(&mut self, exec_host: &ExecHost) {
		// Pending prompts fail closed, pending reflections fall back to their
		// evidence, and calls and commands that outlive the connection stop
		// holding its response channel.
		if let Some(approvals) = &self.approvals {
			approvals.disconnect();
		}
		if let Some(reflection) = &self.reflection {
			reflection.disconnect();
		}
		for (_, state) in mem::take(&mut self.requests) {
			match state {
				RequestState::Invocation(InvocationState::Native {
					feed,
					lifecycle,
					edit_repair,
					acp,
					cancel,
					..
				}) => {
					if let Some(route) = edit_repair {
						route.disconnect();
					}
					acp.disconnect();
					if lifecycle.is_committed() {
						let _ = feed.interrupt(Interrupt {
							class:  sf!("disconnect"),
							reason: sf!("environment connection closed"),
						});
					}
					lifecycle.claim_terminal();
					cancel.cancel();
				},
				RequestState::Invocation(InvocationState::Worker { owner, id, cancel, .. }) => {
					self.authority.settle(&owner, &id);
					cancel.cancel();
				},
				RequestState::InvocationFinishing => {},
				RequestState::Exec { exec, cancel } => {
					let _ = exec_host.cancel(&exec);
					cancel.cancel();
					self.quotas.release_exec();
				},
				RequestState::ProcessAttach { cancel }
				| RequestState::BlobGet { cancel }
				| RequestState::DataStream { cancel }
				| RequestState::DocumentEvents { cancel, .. }
				| RequestState::LspEvents { cancel } => {
					cancel.cancel();
					self.quotas.release_stream();
				},
				RequestState::BlobPut(_) => {},
			}
		}
		self.invocation_ids.clear();
		for lease_id in self.document_leases.keys() {
			self
				.authority
				.release_lease(lease_id, self.connection_owner);
		}
		self.document_leases.clear();
	}
}

impl Drop for ConnectionState {
	fn drop(&mut self) {
		let exec_host = self.exec_host.clone();
		self.cancel_all(&exec_host);
	}
}

impl InvocationState {
	pub(in crate::server) fn id(&self) -> &str {
		match self {
			Self::Native { id, .. } | Self::Worker { id, .. } => id,
		}
	}

	pub(in crate::server) const fn delivery(&self) -> &VerdictDelivery {
		match self {
			Self::Native { delivery, .. } | Self::Worker { delivery, .. } => delivery,
		}
	}

	pub(in crate::server) fn pending_admission_deadline(&self) -> Option<Instant> {
		match self {
			Self::Native { admission, .. } | Self::Worker { admission, .. } => {
				admission.pending_deadline()
			},
		}
	}

	pub(in crate::server) fn expire_admission(
		&mut self,
		now: Instant,
	) -> Option<policy_pb::PolicyDenied> {
		match self {
			Self::Native { admission, .. } | Self::Worker { admission, .. } => admission.expire(now),
		}
	}
}
