//! `env/v1` frame dispatch for one accepted connection.

use super::{
	ApprovalAuthority, ApprovalAuthoritySlot, BlobUpload, ConnectionEditRepairRoute,
	ConnectionState, DEFAULT_TOOL_DEADLINE, EnvServer, Finished, InvocationExecutionPolicy,
	InvocationState, MAX_RESOURCE_COMPLETIONS, MAX_RESOURCE_ENTRIES, MAX_RESOURCE_LIST_BYTES,
	MAX_RESOURCE_READ_BYTES, MAX_RESOURCE_URI_BYTES, NativeCommitError, NativeLifecycle,
	RequestState, VerdictDelivery,
	forward::{
		WorkspaceSearchOwned, authorize_data_operation, authorize_visibility_receipt,
		connection_lease, connection_lease_id, effective_effects, frame_data_operation,
		mcp_operation, mcp_wire_revision, parse_mounted_resource_uri, reject_duplicate_open,
		requests_pty, resource_bound, resource_capability_wire, send_blob_error, send_body,
		send_data_response, send_document_error, send_error, send_exec_error, send_http_error,
		send_invocation_error, send_materialization_error, send_mcp_error,
		send_policy_denied_verdict, send_policy_error, send_resource_capability_error,
		send_resource_fault, send_resource_result, send_workspace_operation_error, spawn_blob_get,
		spawn_document_events, spawn_exec, spawn_lsp_events, spawn_mcp_request,
		spawn_mcp_subscription, spawn_native_invocation, spawn_process_attachment,
		spawn_resource_completion, spawn_worker_invocation, spawn_workspace_search,
		spawn_workspace_walk, verdict_delivery_provenance, workspace_walk_request,
	},
	identity::ConnectionPolicy,
	prelude::*,
	unknown_tool_message,
};

#[derive(Debug, Error)]
pub(in crate::server) enum PrivilegedDispatchError {
	#[error("{violation}")]
	Invalid {
		/// The violated dispatch rule.
		violation: &'static str,
	},
	#[error(transparent)]
	Mutation(#[from] PrivilegedMutationFault),
}

impl ApprovalAuthoritySlot {
	pub(in crate::server) fn bind(
		&self,
		_book: Option<Arc<ApprovalBook>>,
		route: Option<ApprovalRoute>,
	) {
		*self.0.write() = route.map(|route| ApprovalAuthority { route });
	}

	pub(in crate::server) async fn approve_privileged(
		&self,
		ticket: &[u8],
		invocation_id: &Str,
		target: &str,
		kind: &'static str,
	) -> bool {
		let Some(authority) = self.0.read().clone() else {
			return false;
		};
		let ticket = if ticket.is_empty() {
			authority
				.route
				.request(
					Some(invocation_id.clone()),
					vec![ApprovalSpec {
						title:         sf!("Privileged file mutation"),
						body:          sf!(
							"Approve {kind} after ordinary document mutation was refused by filesystem \
							 permissions."
						),
						subject:       Str::new(target),
						kind:          sf!("privileged_write"),
						scopes:        vec![sf!("once")],
						default:       Some(false),
						route:         sf!("local"),
						approver:      None,
						timeout_ms:    120_000,
						unreachable:   sf!("fail_closed"),
						require_human: true,
						pattern:       None,
						evidence:      vec![sf!("filesystem permission fallback")],
					}],
					now_epoch_ms(),
				)
				.await
		} else {
			let Ok(ticket_id) = str::from_utf8(ticket) else {
				return false;
			};
			let Some(ticket) = authority.route.ticket(ticket_id) else {
				return false;
			};
			ticket
		};
		if ticket.state != TicketState::Decided
			|| ticket.invocation_id.as_ref() != Some(invocation_id)
			|| !ticket
				.decision
				.as_ref()
				.is_some_and(|decision| decision.approved)
			|| !ticket
				.reasons
				.iter()
				.any(|reason| reason.kind == "privileged_write" && reason.subject == target)
		{
			return false;
		}
		let timeout = ticket
			.reasons
			.iter()
			.filter_map(|reason| (reason.timeout_ms != 0).then_some(reason.timeout_ms))
			.min()
			.unwrap_or(120_000);
		now_epoch_ms() <= ticket.created_at_ms.saturating_add(timeout)
	}
}

pub(in crate::server) fn requires_environment_host(body: &client_frame::Body) -> bool {
	matches!(
		body,
		client_frame::Body::Retire(_)
			| client_frame::Body::Shutdown(_)
			| client_frame::Body::EvalReset(_)
			| client_frame::Body::EditRepairAnswer(_)
			| client_frame::Body::AcpBind(_)
			| client_frame::Body::AcpDocumentAnswer(_)
			| client_frame::Body::RegisterPresence(_)
			| client_frame::Body::ReleasePresence(_)
			| client_frame::Body::OpenSession(_)
			| client_frame::Body::CloseSession(_)
			| client_frame::Body::Exec(_)
			| client_frame::Body::Stdin(_)
			| client_frame::Body::Signal(_)
			| client_frame::Body::Resize(_)
			| client_frame::Body::StartProcess(_)
			| client_frame::Body::GetProcess(_)
			| client_frame::Body::RestartProcess(_)
			| client_frame::Body::HttpRequest(_)
			| client_frame::Body::ListProcesses(_)
			| client_frame::Body::AttachOutput(_)
			| client_frame::Body::SendInput(_)
			| client_frame::Body::SignalProcess(_)
			| client_frame::Body::StopProcess(_)
	)
}

pub(in crate::server) fn now_epoch_ms() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_millis()
		.try_into()
		.unwrap_or(u64::MAX)
}

pub(in crate::server) fn privileged_presence(value: i32) -> Result<bool, PrivilegedDispatchError> {
	match pb::ExpectedPresence::try_from(value).ok() {
		Some(pb::ExpectedPresence::Present) => Ok(true),
		Some(pb::ExpectedPresence::Missing) => Ok(false),
		_ => Err(PrivilegedDispatchError::Invalid {
			violation: "privileged mutation requires an explicit expected presence",
		}),
	}
}

pub(in crate::server) fn privileged_revision_hash(
	revision: Option<&document_pb::Revision>,
) -> Result<Option<[u8; 32]>, PrivilegedDispatchError> {
	revision
		.map(|revision| {
			revision
				.content_hash
				.as_ref()
				.try_into()
				.map_err(|_| PrivilegedDispatchError::Invalid {
					violation: "privileged mutation revision hash must contain 32 bytes",
				})
		})
		.transpose()
}

pub(in crate::server) fn canonical_privileged_target(
	root: &Path,
	input: &str,
) -> Result<(String, PathBuf), String> {
	let uri = Url::parse(input).map_err(|_| "privileged target is not a valid URI".to_owned())?;
	if uri.scheme() != "file" {
		return Err("privileged target must be a canonical file URI".to_owned());
	}
	let target = uri
		.to_file_path()
		.map_err(|()| "privileged target is not a local file URI".to_owned())?;
	let name = target
		.file_name()
		.ok_or_else(|| "privileged target must name a final filesystem entry".to_owned())?;
	let parent = target
		.parent()
		.ok_or_else(|| "privileged target has no parent directory".to_owned())?;
	let parent = fs::canonicalize(parent)
		.map_err(|error| fault_in("privileged target parent is not canonical", error).to_string())?;
	let root = fs::canonicalize(root)
		.map_err(|error| fault_in("Environment root is not canonical", error).to_string())?;
	if parent != root && !parent.starts_with(&root) {
		return Err("privileged target escapes the Environment root".to_owned());
	}
	let target = parent.join(name);
	let canonical_uri = Url::from_file_path(&target)
		.map_err(|()| "privileged target cannot be represented as a file URI".to_owned())?
		.to_string();
	if canonical_uri != uri.as_str() {
		return Err(format!("privileged target is not canonical; expected {canonical_uri}"));
	}
	Ok((canonical_uri, target))
}

pub(in crate::server) fn privileged_dispatch_error(
	error: PrivilegedDispatchError,
) -> (pb::ProtocolErrorCode, String) {
	match error {
		PrivilegedDispatchError::Invalid { violation: message } => {
			(pb::ProtocolErrorCode::InvalidArgument, message.to_owned())
		},
		PrivilegedDispatchError::Mutation(PrivilegedMutationFault::StaleRevision) => (
			pb::ProtocolErrorCode::PreconditionFailed,
			"privileged mutation expected state is stale".to_owned(),
		),
		PrivilegedDispatchError::Mutation(PrivilegedMutationFault::OperationNotPermitted {
			source,
		}) => (pb::ProtocolErrorCode::PermissionDenied, format!("EPERM: {source}")),
		PrivilegedDispatchError::Mutation(PrivilegedMutationFault::PermissionDenied { source }) => {
			(pb::ProtocolErrorCode::PermissionDenied, format!("EACCES: {source}"))
		},
		PrivilegedDispatchError::Mutation(PrivilegedMutationFault::ReadOnlyFilesystem { source }) => {
			(pb::ProtocolErrorCode::PermissionDenied, format!("EROFS: {source}"))
		},
		PrivilegedDispatchError::Mutation(PrivilegedMutationFault::Other { source }) => {
			(pb::ProtocolErrorCode::Internal, source.to_string())
		},
	}
}

pub(in crate::server) const fn worker_operation(request: &pb::WorkerOp) -> &'static str {
	use pb::worker_op::Op;

	match request.op.as_ref() {
		Some(Op::Open(_)) => "omp.env.worker.open",
		Some(Op::Close(_)) => "omp.env.worker.close",
		Some(Op::Data(_)) => "omp.env.worker.data",
		Some(Op::Info(_)) => "omp.env.worker.info",
		Some(Op::List(_)) | None => "omp.env.worker.list",
	}
}

pub(in crate::server) fn worker_info(route: &WorkerRoute) -> pb::WorkerInfo {
	pb::WorkerInfo {
		name: route.key.name.to_string(),
		generation: route.generation,
		state: pb::WorkerState::Ready as i32,
		..pb::WorkerInfo::default()
	}
}

impl EnvServer {
	pub(in crate::server) async fn dispatch(
		&self,
		frame: pb::ClientFrame,
		responses: &flume::Sender<pb::ServerFrame>,
		finished: &flume::Sender<Finished>,
		connection: &mut ConnectionState,
		policy: &ConnectionPolicy,
	) {
		let scope = frame.scope.clone();
		let Some(body) = frame.body else {
			send_error(
				responses,
				frame.request_id,
				pb::ProtocolErrorCode::InvalidArgument,
				"client frame body is missing",
			)
			.await;
			return;
		};
		if self.environment_authorities().is_none() && requires_environment_host(&body) {
			send_error(
				responses,
				frame.request_id,
				pb::ProtocolErrorCode::Unsupported,
				"environment authority frame reached a session-only host",
			)
			.await;
			return;
		}
		let worker_scope = connection.host.as_ref().is_some_and(|host| {
			scope.as_ref().is_some_and(|scope| {
				connection
					.authority
					.is_worker_invocation(host, &scope.invocation_id)
			})
		});
		if matches!(&body, client_frame::Body::InvokeTool(_))
			&& (!connection.grants.contains("invocation") || worker_scope)
		{
			send_error(
				responses,
				frame.request_id,
				pb::ProtocolErrorCode::PermissionDenied,
				"connection was not granted invocation dispatch",
			)
			.await;
			return;
		}
		if let client_frame::Body::AcpBind(binding) = body {
			if frame.request_id != 0 {
				send_error(
					responses,
					frame.request_id,
					pb::ProtocolErrorCode::InvalidArgument,
					"ACP bind control frames must use request_id 0",
				)
				.await;
			} else {
				connection.bind_acp(binding);
			}
			return;
		}
		// Handled in frame order, so a command this connection issues after it
		// never sees a revoked grant. A connection that relays no approvals
		// holds no grant to revoke.
		if let client_frame::Body::RevokeApprovalGrants(_) = body {
			if frame.request_id != 0 {
				send_error(
					responses,
					frame.request_id,
					pb::ProtocolErrorCode::InvalidArgument,
					"approval grant revocations must use request_id 0",
				)
				.await;
			} else if let Some(approvals) = &connection.approvals {
				approvals.revoke_grants();
			}
			return;
		}
		if let client_frame::Body::Cancel(cancel) = body {
			if frame.request_id != 0 {
				send_error(
					responses,
					frame.request_id,
					pb::ProtocolErrorCode::InvalidArgument,
					"cancel control frames must use request_id 0",
				)
				.await;
				return;
			}
			connection
				.cancel(cancel, &self.exec, responses, finished)
				.await;
			return;
		}
		if frame.request_id == 0 {
			send_error(
				responses,
				0,
				pb::ProtocolErrorCode::InvalidArgument,
				"ordinary frames must use a nonzero request_id",
			)
			.await;
			return;
		}
		if let Some((operation, capability)) = frame_data_operation(&body)
			&& !authorize_data_operation(
				connection,
				scope.as_ref(),
				operation,
				capability,
				responses,
				frame.request_id,
			)
			.await
		{
			return;
		}
		if scope.as_ref().is_some_and(|scope| scope.pty_denied) && requests_pty(&body) {
			send_error(
				responses,
				frame.request_id,
				pb::ProtocolErrorCode::PermissionDenied,
				"PTY allocation is denied by the authenticated invocation scope",
			)
			.await;
			return;
		}
		let continuation = matches!(
			&body,
			client_frame::Body::ArgText(_)
				| client_frame::Body::Admission(_)
				| client_frame::Body::EditRepairAnswer(_)
				| client_frame::Body::AcpDocumentAnswer(_)
				| client_frame::Body::ApprovalAnswer(_)
				| client_frame::Body::ReflectionAnswer(_)
				| client_frame::Body::ArgsCommitted(_)
				| client_frame::Body::Interrupt(_)
				| client_frame::Body::Stdin(_)
				| client_frame::Body::Signal(_)
				| client_frame::Body::Resize(_)
				| client_frame::Body::BlobPutChunk(_)
				| client_frame::Body::BlobPutCommit(_)
		);
		if !continuation && connection.requests.contains_key(&frame.request_id) {
			send_error(
				responses,
				frame.request_id,
				pb::ProtocolErrorCode::AlreadyExists,
				"request_id is already open",
			)
			.await;
			return;
		}

		match body {
			client_frame::Body::Hello(_) => {
				send_error(
					responses,
					frame.request_id,
					pb::ProtocolErrorCode::AlreadyExists,
					"the connection hello is already complete",
				)
				.await;
			},
			client_frame::Body::AcpBind(_) => {
				unreachable!("ACP binding handled before ordinary dispatch")
			},
			client_frame::Body::RevokeApprovalGrants(_) => {
				unreachable!("approval grant revocation handled before ordinary dispatch")
			},
			client_frame::Body::Retire(_) => {
				if policy.retire.is_some() {
					send_body(
						responses,
						frame.request_id,
						server_frame::Body::RetireStarted(pb::RetireStarted::default()),
					)
					.await;
				} else {
					send_error(
						responses,
						frame.request_id,
						pb::ProtocolErrorCode::Unsupported,
						"retire is not available on this transport",
					)
					.await;
				}
			},
			client_frame::Body::EvalReset(_) => {
				self.eval_control.request_reset();
				send_body(
					responses,
					frame.request_id,
					server_frame::Body::EvalReset(pb::EvalResetResponse::default()),
				)
				.await;
			},
			client_frame::Body::Shutdown(request) => {
				let accepted_at_ms = SystemTime::now()
					.duration_since(UNIX_EPOCH)
					.unwrap_or_default()
					.as_millis()
					.try_into()
					.unwrap_or(u64::MAX);
				let grace = Duration::from_millis(request.grace_ms);
				let summary = self.exec.shutdown_managed(grace).await;
				let acknowledgement = ShutdownAcknowledgement {
					accepted_at_ms,
					stopped: summary.stopped,
					spared: summary.spared,
				};
				if self
					.environment
					.as_ref()
					.expect("environment frames are guarded on session-only hosts")
					.process_store
					.record_shutdown(acknowledgement)
					.is_err()
				{
					send_error(
						responses,
						frame.request_id,
						pb::ProtocolErrorCode::Internal,
						"failed to durably record process shutdown acknowledgement",
					)
					.await;
				} else {
					send_body(
						responses,
						frame.request_id,
						server_frame::Body::ShutdownAcknowledged(pb::ShutdownAcknowledged {
							accepted_at_ms,
							props: None,
						}),
					)
					.await;
				}
			},
			client_frame::Body::WorkspaceUpdateCheck(request) => {
				let report = self.check_workspace_updates(request).await;
				send_body(
					responses,
					frame.request_id,
					server_frame::Body::WorkspaceUpdateReport(report),
				)
				.await;
			},
			client_frame::Body::RegisterPresence(request) => {
				if policy.host.is_some() {
					send_error(
						responses,
						frame.request_id,
						pb::ProtocolErrorCode::PermissionDenied,
						"presence registration is unavailable on extension transports",
					)
					.await;
					return;
				}
				if connection.presence.is_some() {
					send_error(
						responses,
						frame.request_id,
						pb::ProtocolErrorCode::AlreadyExists,
						"this connection already owns a presence lease",
					)
					.await;
					return;
				}
				let Ok(client_id) = str::from_utf8(&request.client_id) else {
					send_error(
						responses,
						frame.request_id,
						pb::ProtocolErrorCode::InvalidArgument,
						"presence client_id must be UTF-8",
					)
					.await;
					return;
				};
				if client_id.is_empty() || client_id.len() > 256 {
					send_error(
						responses,
						frame.request_id,
						pb::ProtocolErrorCode::InvalidArgument,
						"presence client_id must contain 1..=256 bytes",
					)
					.await;
					return;
				}
				if request.kind.is_empty() || request.kind.len() > 64 {
					send_error(
						responses,
						frame.request_id,
						pb::ProtocolErrorCode::InvalidArgument,
						"presence kind must contain 1..=64 bytes",
					)
					.await;
					return;
				}
				match self
					.presence
					.register(Str::from(client_id), request.pid, Str::from(request.kind))
				{
					Ok(lease) => {
						let lease_id = Bytes::copy_from_slice(lease.id().as_bytes());
						connection.presence = Some(lease);
						send_body(
							responses,
							frame.request_id,
							server_frame::Body::PresenceRegistered(pb::PresenceRegistered {
								lease_id,
								props: None,
							}),
						)
						.await;
					},
					Err(PresenceError::ClientNotLive { .. }) => {
						send_error(
							responses,
							frame.request_id,
							pb::ProtocolErrorCode::InvalidArgument,
							"presence pid is not a live process",
						)
						.await;
					},
					Err(error) => {
						tracing::warn!(%error, "failed to register client presence");
						send_error(
							responses,
							frame.request_id,
							pb::ProtocolErrorCode::Internal,
							"failed to persist client presence",
						)
						.await;
					},
				}
			},
			client_frame::Body::ReleasePresence(request) => {
				let Some(lease) = connection.presence.as_ref() else {
					send_error(
						responses,
						frame.request_id,
						pb::ProtocolErrorCode::NotFound,
						"this connection has no presence lease",
					)
					.await;
					return;
				};
				if request.lease_id.as_ref() != lease.id().as_bytes() {
					send_error(
						responses,
						frame.request_id,
						pb::ProtocolErrorCode::PermissionDenied,
						"presence lease does not belong to this connection",
					)
					.await;
					return;
				}
				let lease = connection
					.presence
					.take()
					.expect("presence lease checked immediately above");
				match lease.release() {
					Ok(()) => {
						send_body(
							responses,
							frame.request_id,
							server_frame::Body::PresenceReleased(pb::PresenceReleased {
								lease_id: request.lease_id,
								props:    None,
							}),
						)
						.await;
					},
					Err(error) => {
						tracing::warn!(%error, "failed to release client presence");
						send_error(
							responses,
							frame.request_id,
							pb::ProtocolErrorCode::Internal,
							"failed to remove client presence",
						)
						.await;
					},
				}
			},
			client_frame::Body::InvokeTool(request) => {
				self
					.open_invocation(
						frame.request_id,
						request,
						scope.as_ref(),
						responses,
						finished,
						connection,
					)
					.await;
			},
			client_frame::Body::ArgText(request) => {
				let result = connection.invocation_mut(frame.request_id, &request.invocation_id);
				let query = match result {
					Ok(InvocationState::Native { feed, lifecycle, admission, effects, .. })
						if !lifecycle.is_committed() && !lifecycle.is_terminal() =>
					{
						let query = admission.push_fragment(
							&request.fragment,
							self.workspace.root(),
							self.workspace.root(),
							effects,
						);
						if !admission.requires_external_answer()
							&& feed.arg_text(Str::from(request.fragment)).is_err()
						{
							send_error(
								responses,
								frame.request_id,
								pb::ProtocolErrorCode::Cancelled,
								"invocation input is closed",
							)
							.await;
							None
						} else {
							query
						}
					},
					Ok(InvocationState::Worker {
						invocation: Some(invocation),
						committed,
						admission,
						effects,
						..
					}) if !*committed => {
						let query = admission.push_fragment(
							&request.fragment,
							self.workspace.root(),
							self.workspace.root(),
							effects,
						);
						if invocation.streams_args()
							&& !admission.requires_external_answer()
							&& let Err(error) = invocation.arg_text(request)
						{
							send_error(
								responses,
								frame.request_id,
								pb::ProtocolErrorCode::PreconditionFailed,
								&error.to_string(),
							)
							.await;
							None
						} else {
							query
						}
					},
					Ok(_) => {
						send_error(
							responses,
							frame.request_id,
							pb::ProtocolErrorCode::PreconditionFailed,
							"ArgText cannot follow ArgsCommitted",
						)
						.await;
						None
					},
					Err((code, message)) => {
						send_error(responses, frame.request_id, code, message).await;
						None
					},
				};
				if let Some(query) = query {
					send_body(responses, frame.request_id, server_frame::Body::AdmitInvocation(query))
						.await;
				}
			},
			client_frame::Body::Admission(admission) => {
				let result = connection.invocation_mut(frame.request_id, &admission.invocation_id);
				let pending = match result {
					Ok(
						InvocationState::Native { admission: gate, pending_commit, .. }
						| InvocationState::Worker { admission: gate, pending_commit, .. },
					) => {
						if let Err(error) = gate.answer(admission) {
							send_error(
								responses,
								frame.request_id,
								pb::ProtocolErrorCode::PreconditionFailed,
								&error,
							)
							.await;
							None
						} else {
							pending_commit.take()
						}
					},
					Err((code, message)) => {
						send_error(responses, frame.request_id, code, message).await;
						None
					},
				};
				if let Some(request) = pending {
					self
						.commit_invocation(frame.request_id, request, responses, finished, connection)
						.await;
				}
			},
			client_frame::Body::EditRepairAnswer(answer) => {
				if let Err((code, message)) = connection.answer_edit_repair(frame.request_id, answer) {
					send_error(responses, frame.request_id, code, message).await;
				}
			},
			client_frame::Body::AcpDocumentAnswer(answer) => {
				if let Err((code, message)) = connection.answer_acp_document(frame.request_id, answer) {
					send_error(responses, frame.request_id, code, message).await;
				}
			},
			// An answer that matches nothing is dropped without a reply on every
			// host: an error on its request_id would end the issuing command's
			// stream.
			client_frame::Body::ApprovalAnswer(answer) => {
				if let Some(approvals) = &connection.approvals {
					approvals.answer(frame.request_id, answer);
				} else {
					tracing::debug!(
						request_id = frame.request_id,
						query_id = answer.query_id,
						"ignored an approval answer on a connection that relays no approvals"
					);
				}
			},
			client_frame::Body::ReflectionAnswer(answer) => {
				if let Some(reflection) = &connection.reflection {
					reflection.answer(frame.request_id, answer);
				} else {
					tracing::debug!(
						request_id = frame.request_id,
						query_id = answer.query_id,
						"ignored a reflection answer on a connection that relays no reflections"
					);
				}
			},
			client_frame::Body::ArgsCommitted(request) => {
				match connection.scope_authenticates(
					frame.request_id,
					&request.invocation_id,
					&request.effect_token,
					scope.as_ref(),
				) {
					Ok(true) => {},
					Ok(false) => {
						send_error(
							responses,
							frame.request_id,
							pb::ProtocolErrorCode::PermissionDenied,
							"invocation scope is not authenticated by the committed effect token",
						)
						.await;
						return;
					},
					Err((code, message)) => {
						send_error(responses, frame.request_id, code, message).await;
						return;
					},
				}
				// The gate accepts the committed arguments before anything is
				// judged from them: a commit it refuses leaves the call unjudged,
				// its policy still pending and its stream still withheld.
				let staged = match connection.invocation_mut(frame.request_id, &request.invocation_id) {
					Ok(
						InvocationState::Native { admission, .. }
						| InvocationState::Worker { admission, .. },
					) => admission.stage(&request.raw),
					Err((code, message)) => {
						send_error(responses, frame.request_id, code, message).await;
						return;
					},
				};
				if let Err(error) = staged {
					send_error(
						responses,
						frame.request_id,
						pb::ProtocolErrorCode::InvalidArgument,
						&error,
					)
					.await;
					return;
				}
				let refused = match connection.scope_committed(
					frame.request_id,
					&request.invocation_id,
					&self.registry,
					&self.resources,
					&request.raw,
				) {
					Ok(refused) => refused,
					Err((code, message)) => {
						send_error(responses, frame.request_id, code, message).await;
						return;
					},
				};
				if let Some(refused) = refused {
					let invocation_id = Str::from(request.invocation_id.as_str());
					send_invocation_error(
						responses,
						frame.request_id,
						pb::ProtocolErrorCode::PermissionDenied,
						&refused.to_string(),
					)
					.await;
					connection.abandon_admission(frame.request_id, &invocation_id);
					return;
				}
				let query = match connection.invocation_mut(frame.request_id, &request.invocation_id) {
					Ok(
						InvocationState::Native { admission, effects, .. }
						| InvocationState::Worker { admission, effects, .. },
					) => admission.emit(self.workspace.root(), self.workspace.root(), effects),
					Err((code, message)) => {
						send_error(responses, frame.request_id, code, message).await;
						return;
					},
				};
				if let Some(query) = query {
					send_body(responses, frame.request_id, server_frame::Body::AdmitInvocation(query))
						.await;
				}
				self
					.commit_invocation(frame.request_id, request, responses, finished, connection)
					.await;
			},
			client_frame::Body::Interrupt(request) => {
				connection
					.interrupt(frame.request_id, request, responses, finished)
					.await;
			},
			client_frame::Body::OpenSession(request) => {
				if reject_duplicate_open(connection, frame.request_id, responses).await {
					return;
				}
				if let Err(error) = connection.quotas.reserve_exec() {
					send_policy_error(responses, frame.request_id, error).await;
					return;
				}
				match self.exec.open_session(request).await {
					Ok(response) => {
						send_body(
							responses,
							frame.request_id,
							server_frame::Body::SessionOpened(response),
						)
						.await;
					},
					Err(error) => {
						connection.quotas.release_exec();
						send_exec_error(responses, frame.request_id, &error).await;
					},
				}
			},
			client_frame::Body::CloseSession(request) => {
				match self.exec.close_session(&request.session) {
					Ok(response) => {
						connection.quotas.release_exec();
						send_body(
							responses,
							frame.request_id,
							server_frame::Body::SessionClosed(response),
						)
						.await;
					},
					Err(error) => send_exec_error(responses, frame.request_id, &error).await,
				}
			},
			client_frame::Body::Exec(request) => {
				if reject_duplicate_open(connection, frame.request_id, responses).await {
					return;
				}
				if let Err(error) = connection.quotas.reserve_exec() {
					send_policy_error(responses, frame.request_id, error).await;
					return;
				}
				let principal = scope
					.as_ref()
					.filter(|scope| !scope.session_id.is_empty() && !scope.agent_id.is_empty());
				let owner = Some(connection.invocation_owner(principal));
				let approvals = connection.owned_approvals(frame.request_id);
				let reflection = connection.owned_reflection(frame.request_id);
				match self
					.exec
					.exec_relayed(request, None, owner, approvals, reflection)
					.await
				{
					Ok((started, run)) => {
						let exec = Bytes::copy_from_slice(run.id());
						let cancel = CancellationToken::new();
						connection
							.requests
							.insert(frame.request_id, RequestState::Exec {
								exec:   exec.clone(),
								cancel: cancel.clone(),
							});
						send_body(responses, frame.request_id, server_frame::Body::ExecStarted(started))
							.await;
						spawn_exec(frame.request_id, run, cancel, responses.clone(), finished.clone());
					},
					Err(error) => {
						connection.quotas.release_exec();
						send_exec_error(responses, frame.request_id, &error).await;
					},
				}
			},
			client_frame::Body::Stdin(request) => {
				if let Some(exec) = connection
					.exec_id(frame.request_id, &request.exec, responses)
					.await
				{
					let data = match request.input {
						Some(stdin_frame::Input::Data(data)) => Some(data),
						Some(stdin_frame::Input::Eof(true)) => None,
						_ => {
							send_error(
								responses,
								frame.request_id,
								pb::ProtocolErrorCode::InvalidArgument,
								"stdin frame has no data or eof marker",
							)
							.await;
							return;
						},
					};
					if let Err(error) = self.exec.stdin(&exec, data.as_deref()) {
						send_exec_error(responses, frame.request_id, &error).await;
					}
				}
			},
			client_frame::Body::Signal(request) => {
				if let Some(exec) = connection
					.exec_id(frame.request_id, &request.exec, responses)
					.await && let Err(error) = self.exec.signal(&exec, &request.signal)
				{
					send_exec_error(responses, frame.request_id, &error).await;
				}
			},
			client_frame::Body::Resize(request) => {
				if let Some(exec) = connection
					.exec_id(frame.request_id, &request.exec, responses)
					.await && let Err(error) = self.exec.resize(&exec, request.rows, request.columns)
				{
					send_exec_error(responses, frame.request_id, &error).await;
				}
			},
			client_frame::Body::StartProcess(request) => {
				if let Err(error) = connection.quotas.charge_process_start() {
					send_policy_error(responses, frame.request_id, error).await;
					return;
				}
				match self.exec.start_process(request).await {
					Ok(response) => {
						send_body(
							responses,
							frame.request_id,
							server_frame::Body::ProcessStarted(response),
						)
						.await;
					},
					Err(error) => send_exec_error(responses, frame.request_id, &error).await,
				}
			},
			client_frame::Body::GetProcess(request) => match self.exec.get_process(&request) {
				Ok(response) => {
					send_body(responses, frame.request_id, server_frame::Body::ProcessInfo(response))
						.await;
				},
				Err(error) => send_exec_error(responses, frame.request_id, &error).await,
			},
			client_frame::Body::RestartProcess(request) => {
				if let Err(error) = connection.quotas.charge_process_start() {
					send_policy_error(responses, frame.request_id, error).await;
					return;
				}
				match self.exec.restart_process(request).await {
					Ok(response) => {
						send_body(
							responses,
							frame.request_id,
							server_frame::Body::ProcessRestarted(response),
						)
						.await;
					},
					Err(error) => send_exec_error(responses, frame.request_id, &error).await,
				}
			},
			client_frame::Body::HttpRequest(request) => {
				match self.http_egress.request(request).await {
					Ok(response) => {
						send_body(
							responses,
							frame.request_id,
							server_frame::Body::HttpResponse(response),
						)
						.await;
					},
					Err(error) => send_http_error(responses, frame.request_id, &error).await,
				}
			},
			client_frame::Body::ListProcesses(_) => {
				send_body(
					responses,
					frame.request_id,
					server_frame::Body::ProcessList(self.exec.list_processes()),
				)
				.await;
			},
			client_frame::Body::AttachOutput(request) => {
				if reject_duplicate_open(connection, frame.request_id, responses).await {
					return;
				}
				if let Err(error) = connection.quotas.reserve_stream() {
					send_policy_error(responses, frame.request_id, error).await;
					return;
				}
				match self.exec.attach_output(&request) {
					Ok(attachment) => {
						let cancel = CancellationToken::new();
						let process_name = Str::from(request.name);
						connection
							.requests
							.insert(frame.request_id, RequestState::ProcessAttach {
								cancel: cancel.clone(),
							});
						send_body(
							responses,
							frame.request_id,
							server_frame::Body::OutputAttached(attachment.attached),
						)
						.await;
						for output in attachment.backlog {
							send_body(
								responses,
								frame.request_id,
								server_frame::Body::ProcessOutput(output),
							)
							.await;
						}
						send_body(
							responses,
							frame.request_id,
							server_frame::Body::ProcessState(pb::ProcessStateEvent {
								process: Some(attachment.state),
								props:   Default::default(),
							}),
						)
						.await;
						spawn_process_attachment(
							frame.request_id,
							process_name,
							attachment.events,
							cancel,
							responses.clone(),
							finished.clone(),
						);
					},
					Err(error) => {
						connection.quotas.release_stream();
						send_exec_error(responses, frame.request_id, &error).await;
					},
				}
			},
			client_frame::Body::SendInput(request) => {
				let data = match request.input {
					Some(send_input::Input::Data(data)) => Some(data),
					Some(send_input::Input::Eof(true)) => None,
					_ => {
						send_error(
							responses,
							frame.request_id,
							pb::ProtocolErrorCode::InvalidArgument,
							"process input has no data or eof marker",
						)
						.await;
						return;
					},
				};
				match self
					.exec
					.send_process_input(&request.name, request.generation, data.as_deref())
				{
					Ok(response) => {
						send_body(
							responses,
							frame.request_id,
							server_frame::Body::ProcessCommandAccepted(response),
						)
						.await;
					},
					Err(error) => send_exec_error(responses, frame.request_id, &error).await,
				}
			},
			client_frame::Body::SignalProcess(request) => {
				match self
					.exec
					.signal_process(&request.name, request.generation, &request.signal)
				{
					Ok(response) => {
						send_body(
							responses,
							frame.request_id,
							server_frame::Body::ProcessCommandAccepted(response),
						)
						.await;
					},
					Err(error) => send_exec_error(responses, frame.request_id, &error).await,
				}
			},
			client_frame::Body::StopProcess(request) => {
				match self.exec.stop_process(
					&request.name,
					request.generation,
					Duration::from_millis(request.grace_ms),
				) {
					Ok(response) => {
						send_body(
							responses,
							frame.request_id,
							server_frame::Body::ProcessCommandAccepted(response),
						)
						.await;
					},
					Err(error) => send_exec_error(responses, frame.request_id, &error).await,
				}
			},
			client_frame::Body::BlobStat(request) => match self.blobs.stat(&request.hash) {
				Ok(response) => {
					send_body(responses, frame.request_id, server_frame::Body::BlobStat(response)).await;
				},
				Err(error) => send_blob_error(responses, frame.request_id, &error).await,
			},
			client_frame::Body::Data(request) => {
				self
					.dispatch_data(
						frame.request_id,
						request,
						scope.as_ref(),
						responses,
						finished,
						connection,
					)
					.await;
			},
			client_frame::Body::BlobGet(request) => {
				if reject_duplicate_open(connection, frame.request_id, responses).await {
					return;
				}
				let delivery = scope.as_ref().and_then(verdict_delivery_provenance);
				if let Err(error) = connection.quotas.reserve_stream() {
					send_policy_error(responses, frame.request_id, error).await;
					return;
				}
				match self.blobs.get_request(&request) {
					Ok(read) => {
						if let Some(delivery) = delivery.as_ref()
							&& let Err(error) = self.blobs.renew_verdict_delivery(
								Some(delivery.session_id.as_str()),
								delivery.invocation_id.as_str(),
								read.id(),
							) {
							connection.quotas.release_stream();
							send_blob_error(responses, frame.request_id, &error).await;
							return;
						}
						let cancel = CancellationToken::new();
						connection
							.requests
							.insert(frame.request_id, RequestState::BlobGet { cancel: cancel.clone() });
						spawn_blob_get(
							frame.request_id,
							read,
							delivery,
							cancel,
							responses.clone(),
							finished.clone(),
							self.blobs.clone(),
						);
					},
					Err(error) => {
						connection.quotas.release_stream();
						send_blob_error(responses, frame.request_id, &error).await;
					},
				}
			},
			client_frame::Body::BlobPutChunk(chunk) => {
				self
					.put_chunk(frame.request_id, chunk, responses, connection)
					.await;
			},
			client_frame::Body::BlobPutCommit(_) => {
				self
					.commit_blob(frame.request_id, responses, connection)
					.await;
			},
			client_frame::Body::BlobDelete(request) => match self.blobs.delete(&request.hash) {
				Ok(response) => {
					send_body(responses, frame.request_id, server_frame::Body::BlobDeleted(response))
						.await;
				},
				Err(error) => send_blob_error(responses, frame.request_id, &error).await,
			},
			client_frame::Body::Cancel(_) => unreachable!("cancel handled before ordinary dispatch"),
		}
	}

	pub(in crate::server) async fn dispatch_data(
		&self,
		request_id: u64,
		request: pb::DataRequest,
		scope: Option<&pb::InvocationScope>,
		responses: &flume::Sender<pb::ServerFrame>,
		finished: &flume::Sender<Finished>,
		connection: &mut ConnectionState,
	) {
		use data_request::Body;

		let environment_only = matches!(
			request.body.as_ref(),
			Some(
				Body::Document(_)
					| Body::Walk(_)
					| Body::Search(_)
					| Body::Workspace(_)
					| Body::Worktree(_)
					| Body::HostInfo(_)
					| Body::WorkspaceRoots(_)
					| Body::RepositorySnapshot(_)
					| Body::ExecSession(_)
					| Body::PrivilegedMutation(_)
					| Body::DapLaunch(_)
					| Body::DapAttach(_)
					| Body::DapAction(_)
					| Body::DetachExec(_)
					| Body::Visibility(_)
			)
		);
		if environment_only && self.environment_authorities().is_none() {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::Unsupported,
				"environment data authority is unavailable on a session-only host",
			)
			.await;
			return;
		}

		match request.body {
			Some(Body::Visibility(request)) => {
				if connection.host.is_some() || scope.is_some() || !connection.grants("env.fs.read") {
					send_error(
						responses,
						request_id,
						pb::ProtocolErrorCode::PermissionDenied,
						"visibility receipts require an application document-read authority",
					)
					.await;
					return;
				}
				match authorize_visibility_receipt(&self.registry, request) {
					Ok(()) => {
						send_data_response(
							responses,
							request_id,
							data_response::Body::Visibility(pb::VisibilityAuthorized {}),
						)
						.await;
					},
					Err((code, message)) => {
						send_error(responses, request_id, code, message).await;
					},
				}
			},
			Some(Body::Worker(request)) => {
				if !authorize_data_operation(
					connection,
					scope,
					worker_operation(&request),
					"env.worker",
					responses,
					request_id,
				)
				.await
				{
					return;
				}
				self.dispatch_worker(request_id, request, responses).await;
			},
			Some(Body::Document(request)) => {
				self
					.dispatch_document(request_id, request, scope, responses, finished, connection)
					.await;
			},
			Some(Body::Walk(request)) => {
				if !authorize_data_operation(
					connection,
					scope,
					"omp.env.find.walk",
					"env.search",
					responses,
					request_id,
				)
				.await
				{
					return;
				}
				let walk = match workspace_walk_request(&self.workspace, &request) {
					Ok(walk) => walk,
					Err((code, message)) => {
						send_error(responses, request_id, code, &message).await;
						return;
					},
				};
				if let Err(error) = connection.quotas.reserve_stream() {
					send_policy_error(responses, request_id, error).await;
					return;
				}
				let cancel = CancellationToken::new();
				connection
					.requests
					.insert(request_id, RequestState::DataStream { cancel: cancel.clone() });
				spawn_workspace_walk(
					request_id,
					self.workspace.clone(),
					walk,
					cancel,
					responses.clone(),
					finished.clone(),
				);
			},
			Some(Body::Search(request)) => {
				if !authorize_data_operation(
					connection,
					scope,
					"omp.env.find.search",
					"env.search",
					responses,
					request_id,
				)
				.await
				{
					return;
				}
				let Some(wire_walk) = request.walk.as_ref() else {
					send_error(
						responses,
						request_id,
						pb::ProtocolErrorCode::InvalidArgument,
						"search walk request is missing",
					)
					.await;
					return;
				};
				let walk = match workspace_walk_request(&self.workspace, wire_walk) {
					Ok(walk) => walk,
					Err((code, message)) => {
						send_error(responses, request_id, code, &message).await;
						return;
					},
				};
				let pattern = match str::from_utf8(&request.pattern) {
					Ok(pattern) if !pattern.is_empty() => Str::from(pattern),
					_ => {
						send_error(
							responses,
							request_id,
							pb::ProtocolErrorCode::InvalidArgument,
							"search pattern must be nonempty UTF-8",
						)
						.await;
						return;
					},
				};
				let options = WorkspaceSearchOwned {
					pattern,
					case: if request.case_sensitive {
						WorkspaceSearchCase::Sensitive
					} else {
						WorkspaceSearchCase::Insensitive
					},
					limit: request.limit,
				};
				if let Err(error) = connection.quotas.reserve_stream() {
					send_policy_error(responses, request_id, error).await;
					return;
				}
				let cancel = CancellationToken::new();
				connection
					.requests
					.insert(request_id, RequestState::DataStream { cancel: cancel.clone() });
				spawn_workspace_search(
					request_id,
					self.workspace.clone(),
					walk,
					options,
					cancel,
					responses.clone(),
					finished.clone(),
				);
			},
			Some(Body::Workspace(request)) => {
				self
					.dispatch_workspace(request_id, request, scope, responses, connection)
					.await;
			},
			Some(Body::Worktree(request)) => {
				self
					.dispatch_worktree(request_id, request, scope, responses, connection)
					.await;
			},
			Some(Body::HostInfo(request)) => {
				if request.wire_revision != omp_proto::SCHEMA_REV {
					send_error(
						responses,
						request_id,
						pb::ProtocolErrorCode::InvalidArgument,
						"host-info wire revision does not match the Environment schema",
					)
					.await;
					return;
				}
				let info = self.host_info.snapshot(request.max_field_bytes).await;
				send_data_response(responses, request_id, data_response::Body::HostInfo(info)).await;
			},
			Some(Body::WorkspaceRoots(request)) => {
				if request.wire_revision != omp_proto::SCHEMA_REV {
					send_error(
						responses,
						request_id,
						pb::ProtocolErrorCode::InvalidArgument,
						"workspace-root wire revision does not match the Environment schema",
					)
					.await;
					return;
				}
				send_data_response(
					responses,
					request_id,
					data_response::Body::WorkspaceRoots(self.workspace_roots.snapshot()),
				)
				.await;
			},
			Some(Body::Mcp(request)) => {
				if !authorize_data_operation(
					connection,
					scope,
					mcp_operation(&request),
					"env.mcp",
					responses,
					request_id,
				)
				.await
				{
					return;
				}
				self
					.dispatch_mcp(request_id, request, responses, finished, connection)
					.await;
			},
			Some(Body::RepositorySnapshot(request)) => {
				if !authorize_data_operation(
					connection,
					scope,
					"omp.env.find.walk",
					"env.search",
					responses,
					request_id,
				)
				.await
				{
					return;
				}
				self
					.dispatch_repository_snapshot(request_id, request, responses)
					.await;
			},
			Some(Body::ExecSession(request)) => {
				let operation = match request.op.as_ref() {
					Some(exec_session_op::Op::Materialize(_)) => "omp.env.sh.exec",
					Some(exec_session_op::Op::ReleaseMaterialization(_)) => "omp.env.sh.close_session",
					Some(exec_session_op::Op::Control(_) | exec_session_op::Op::Signal(_)) => {
						"omp.env.sh.signal"
					},
					Some(exec_session_op::Op::Stdin(_)) => "omp.env.sh.stdin",
					Some(exec_session_op::Op::Resize(_)) => "omp.env.sh.resize",
					Some(exec_session_op::Op::Capabilities(_))
					| Some(exec_session_op::Op::FinalCwd(_)) => "omp.env.sh.open_session",
					None => {
						send_error(
							responses,
							request_id,
							pb::ProtocolErrorCode::InvalidArgument,
							"exec-session operation is missing",
						)
						.await;
						return;
					},
				};
				if !authorize_data_operation(
					connection, scope, operation, "env.exec", responses, request_id,
				)
				.await
				{
					return;
				}
				self
					.dispatch_exec_session(request_id, request, responses)
					.await;
			},
			Some(Body::PrivilegedMutation(request)) => {
				if !authorize_data_operation(
					connection,
					scope,
					"omp.env.fs.privileged_mutation",
					"env.fs.write",
					responses,
					request_id,
				)
				.await
				{
					return;
				}
				self
					.dispatch_privileged_mutation(request_id, request, scope, responses)
					.await;
			},
			Some(request @ (Body::DapLaunch(_) | Body::DapAttach(_) | Body::DapAction(_))) => {
				self
					.dispatch_dap(request_id, request, scope, responses, finished, connection)
					.await;
			},
			Some(Body::Site(request)) => {
				if !authorize_data_operation(
					connection,
					scope,
					"omp.env.site.materialize",
					"env.site",
					responses,
					request_id,
				)
				.await
				{
					return;
				}
				if connection.host.is_some() {
					send_error(
						responses,
						request_id,
						pb::ProtocolErrorCode::PermissionDenied,
						"site trees and their store are installer-owned and read-only to extensions",
					)
					.await;
					return;
				}
				let module_paths = record_modules(&request.files);
				match self.sites.materialize(request) {
					Ok(materialized) => {
						for module in module_paths {
							if let Err(error) = self.require_record_owner(
								&materialized.site_key,
								module,
								&materialized.site_key,
							) {
								send_error(
									responses,
									request_id,
									pb::ProtocolErrorCode::PermissionDenied,
									&error,
								)
								.await;
								return;
							}
						}
						send_data_response(
							responses,
							request_id,
							data_response::Body::Site(materialized),
						)
						.await;
					},
					Err(
						error @ (SiteError::InvalidSiteKey
						| SiteError::InvalidFilePath { path: _ }
						| SiteError::InvalidBlobHash),
					) => {
						send_error(responses, request_id, pb::ProtocolErrorCode::InvalidArgument, &error)
							.await;
					},
					Err(error @ SiteError::TrustedLoad(_)) => {
						send_error(
							responses,
							request_id,
							pb::ProtocolErrorCode::PermissionDenied,
							&error,
						)
						.await;
					},
					Err(error) => {
						send_error(responses, request_id, pb::ProtocolErrorCode::Internal, &error).await;
					},
				}
			},
			Some(Body::DetachExec(request)) => {
				if !authorize_data_operation(
					connection,
					scope,
					"omp.env.sh.detach",
					"env.exec",
					responses,
					request_id,
				)
				.await
				{
					return;
				}
				match self.exec.detach_exec(&request.exec, &request.name) {
					Ok(response) => {
						send_data_response(
							responses,
							request_id,
							data_response::Body::DetachedExec(response),
						)
						.await;
					},
					Err(error) => send_exec_error(responses, request_id, &error).await,
				}
			},
			Some(Body::Resource(request)) => {
				self
					.dispatch_resource(request_id, request, scope, responses, finished, connection)
					.await;
			},
			None => {
				send_error(
					responses,
					request_id,
					pb::ProtocolErrorCode::InvalidArgument,
					"DATA request body is missing",
				)
				.await;
			},
		}
	}

	pub(in crate::server) async fn dispatch_resource(
		&self,
		request_id: u64,
		request: pb::ResourceOp,
		scope: Option<&pb::InvocationScope>,
		responses: &flume::Sender<pb::ServerFrame>,
		finished: &flume::Sender<Finished>,
		connection: &mut ConnectionState,
	) {
		use pb::resource_op::Op;

		if !authorize_data_operation(
			connection,
			scope,
			"omp.env.docs.read",
			"env.doc.read",
			responses,
			request_id,
		)
		.await
		{
			return;
		}
		let Some(operation) = request.op else {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::InvalidArgument,
				"resource operation is missing",
			)
			.await;
			return;
		};
		let wire_revision = match &operation {
			Op::Read(request) => request.wire_revision,
			Op::List(request) => request.wire_revision,
			Op::Path(request) => request.wire_revision,
			Op::Complete(request) => request.wire_revision,
		};
		if wire_revision != omp_proto::SCHEMA_REV {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::InvalidArgument,
				"resource wire revision does not match the Environment schema",
			)
			.await;
			return;
		}

		match operation {
			Op::Read(request) => {
				let Some(uri) = parse_mounted_resource_uri(&request.uri, responses, request_id).await
				else {
					return;
				};
				let max_bytes = match resource_bound(
					request.max_bytes,
					MAX_RESOURCE_READ_BYTES,
					"resource read max_bytes",
				) {
					Ok(bound) => bound,
					Err(message) => {
						send_error(
							responses,
							request_id,
							pb::ProtocolErrorCode::InvalidArgument,
							message,
						)
						.await;
						return;
					},
				};
				let Some(result) = self
					.resources
					.read_bounded(uri.scheme, uri.resource, &uri.selector, max_bytes, request.path_only)
					.await
				else {
					send_resource_capability_error(responses, request_id, "read").await;
					return;
				};
				match result {
					Ok(result) => {
						let capability = self
							.resources
							.capability(uri.scheme)
							.expect("mounted resource keeps capability metadata");
						send_resource_result(responses, request_id, pb::ResourceResult {
							uri:                request.uri,
							data:               Bytes::copy_from_slice(&result.data),
							entries:            Vec::new(),
							canonical_path_uri: result
								.canonical_path_uri
								.map_or_else(String::new, |uri| uri.to_string()),
							capability:         Some(resource_capability_wire(capability)),
							truncated:          result.truncated,
						})
						.await;
					},
					Err(fault) => send_resource_fault(responses, request_id, &fault).await,
				}
			},
			Op::List(request) => {
				let Some(uri) = parse_mounted_resource_uri(&request.uri, responses, request_id).await
				else {
					return;
				};
				if !matches!(uri.selector, omp_tools::read::selector::ParsedSelector::None) {
					send_error(
						responses,
						request_id,
						pb::ProtocolErrorCode::InvalidArgument,
						"resource list URI cannot include a read selector",
					)
					.await;
					return;
				}
				let max_entries = match resource_bound(
					u64::from(request.max_entries),
					MAX_RESOURCE_ENTRIES,
					"resource list max_entries",
				) {
					Ok(bound) => bound,
					Err(message) => {
						send_error(
							responses,
							request_id,
							pb::ProtocolErrorCode::InvalidArgument,
							message,
						)
						.await;
						return;
					},
				};
				let max_bytes = match resource_bound(
					request.max_bytes,
					MAX_RESOURCE_LIST_BYTES,
					"resource list max_bytes",
				) {
					Ok(bound) => bound,
					Err(message) => {
						send_error(
							responses,
							request_id,
							pb::ProtocolErrorCode::InvalidArgument,
							message,
						)
						.await;
						return;
					},
				};
				let Some(result) = self
					.resources
					.list(uri.scheme, uri.resource, max_entries, max_bytes)
					.await
				else {
					send_resource_capability_error(responses, request_id, "list").await;
					return;
				};
				match result {
					Ok(result) => {
						let capability = self
							.resources
							.capability(uri.scheme)
							.expect("mounted resource keeps capability metadata");
						send_resource_result(responses, request_id, pb::ResourceResult {
							uri:                request.uri,
							data:               Bytes::new(),
							entries:            result
								.entries
								.into_iter()
								.map(|entry| pb::ResourceEntry {
									uri:       entry.uri.to_string(),
									name:      entry.name.to_string(),
									directory: entry.directory,
									size:      entry.size,
								})
								.collect(),
							canonical_path_uri: String::new(),
							capability:         Some(resource_capability_wire(capability)),
							truncated:          result.truncated,
						})
						.await;
					},
					Err(fault) => send_resource_fault(responses, request_id, &fault).await,
				}
			},
			Op::Path(request) => {
				let Some(uri) = parse_mounted_resource_uri(&request.uri, responses, request_id).await
				else {
					return;
				};
				let Some(result) = self.resources.path(uri.scheme, uri.resource).await else {
					send_resource_capability_error(responses, request_id, "path").await;
					return;
				};
				match result {
					Ok(result) => {
						let capability = self
							.resources
							.capability(uri.scheme)
							.expect("mounted resource keeps capability metadata");
						send_resource_result(responses, request_id, pb::ResourceResult {
							uri:                request.uri,
							data:               Bytes::new(),
							entries:            Vec::new(),
							canonical_path_uri: result
								.canonical_path_uri
								.map_or_else(String::new, |uri| uri.to_string()),
							capability:         Some(resource_capability_wire(capability)),
							truncated:          false,
						})
						.await;
					},
					Err(fault) => send_resource_fault(responses, request_id, &fault).await,
				}
			},
			Op::Complete(request) => {
				let max_results = match resource_bound(
					u64::from(request.max_results),
					MAX_RESOURCE_COMPLETIONS,
					"resource completion max_results",
				) {
					Ok(bound) => bound,
					Err(message) => {
						send_error(
							responses,
							request_id,
							pb::ProtocolErrorCode::InvalidArgument,
							message,
						)
						.await;
						return;
					},
				};
				if request.input.len() > MAX_RESOURCE_URI_BYTES {
					send_error(
						responses,
						request_id,
						pb::ProtocolErrorCode::InvalidArgument,
						"resource completion input exceeds the 8192-byte limit",
					)
					.await;
					return;
				}
				if request.catalog_revision != 0
					&& request.catalog_revision != self.resources.revision()
				{
					send_error(
						responses,
						request_id,
						pb::ProtocolErrorCode::PreconditionFailed,
						"resource completion catalog revision is stale",
					)
					.await;
					return;
				}
				if let Err(error) = connection.quotas.reserve_stream() {
					send_policy_error(responses, request_id, error).await;
					return;
				}
				let cancel = CancellationToken::new();
				connection
					.requests
					.insert(request_id, RequestState::DataStream { cancel: cancel.clone() });
				spawn_resource_completion(
					request_id,
					request.input,
					max_results,
					Arc::clone(&self.resources),
					cancel,
					responses.clone(),
					finished.clone(),
				);
			},
		}
	}

	pub(in crate::server) async fn dispatch_privileged_mutation(
		&self,
		request_id: u64,
		request: pb::PrivilegedMutationIntent,
		scope: Option<&pb::InvocationScope>,
		responses: &flume::Sender<pb::ServerFrame>,
	) {
		if request.wire_revision != omp_proto::SCHEMA_REV {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::InvalidArgument,
				"privileged-mutation wire revision does not match the Environment schema",
			)
			.await;
			return;
		}
		let Some(scope) = scope.filter(|scope| scope.invocation_id == request.invocation_id) else {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::PermissionDenied,
				"privileged mutation invocation attribution does not match its Environment scope",
			)
			.await;
			return;
		};
		let attributed = !scope.effect_token.is_empty()
			&& request
				.session
				.as_ref()
				.is_some_and(|session| !session.value.is_empty())
			&& request
				.effect
				.as_ref()
				.is_some_and(|effect| !effect.value.is_empty());
		if !attributed {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::PermissionDenied,
				"privileged mutation requires session, invocation, effect, and approval attribution",
			)
			.await;
			return;
		}
		let Some(mutation) = request.mutation else {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::InvalidArgument,
				"privileged mutation omitted its write or unlink intent",
			)
			.await;
			return;
		};
		let target_uri = match &mutation {
			privileged_mutation_intent::Mutation::Write(intent) => {
				intent.canonical_target_uri.as_str()
			},
			privileged_mutation_intent::Mutation::Unlink(intent) => {
				intent.canonical_target_uri.as_str()
			},
		};
		let (canonical_uri, target) =
			match canonical_privileged_target(self.workspace.root(), target_uri) {
				Ok(target) => target,
				Err(message) => {
					send_error(responses, request_id, pb::ProtocolErrorCode::InvalidArgument, &message)
						.await;
					return;
				},
			};
		let kind = match &mutation {
			privileged_mutation_intent::Mutation::Write(_) => "write",
			privileged_mutation_intent::Mutation::Unlink(_) => "unlink",
		};
		if !self
			.approvals
			.approve_privileged(
				&request.approval_ticket,
				&Str::from(request.invocation_id.as_str()),
				&canonical_uri,
				kind,
			)
			.await
		{
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::PermissionDenied,
				"privileged mutation approval ticket is absent, denied, expired, or inauthentic",
			)
			.await;
			return;
		}
		let root = self.workspace.root().to_path_buf();
		let operation = task::spawn_blocking(move || match mutation {
			privileged_mutation_intent::Mutation::Write(intent) => {
				let expected_present = privileged_presence(intent.expected_presence)?;
				let expected = privileged_revision_hash(intent.expected_revision.as_ref())?;
				privileged_write(
					&root,
					&target,
					intent.content,
					expected_present,
					expected.as_ref(),
					intent.mode,
				)
				.map_err(PrivilegedDispatchError::Mutation)?;
				Ok((document_pb::DocumentPresence::Present, None))
			},
			privileged_mutation_intent::Mutation::Unlink(intent) => {
				let expected_present = privileged_presence(intent.expected_presence)?;
				let expected = privileged_revision_hash(intent.expected_revision.as_ref())?;
				privileged_unlink(
					&root,
					&target,
					expected_present,
					expected.as_ref(),
					intent.recursive,
				)
				.map_err(PrivilegedDispatchError::Mutation)?;
				Ok((document_pb::DocumentPresence::Missing, None))
			},
		})
		.await;
		match operation {
			Ok(Ok((presence, committed_revision))) => {
				send_data_response(
					responses,
					request_id,
					data_response::Body::PrivilegedMutation(pb::PrivilegedMutationResult {
						canonical_target_uri: canonical_uri,
						presence: presence as i32,
						committed_revision,
					}),
				)
				.await;
			},
			Ok(Err(error)) => {
				let (code, message) = privileged_dispatch_error(error);
				send_error(responses, request_id, code, &message).await;
			},
			Err(error) => {
				send_error(
					responses,
					request_id,
					pb::ProtocolErrorCode::Internal,
					fault_in("privileged mutation worker failed", error),
				)
				.await;
			},
		}
	}

	pub(in crate::server) async fn dispatch_dap(
		&self,
		request_id: u64,
		request: data_request::Body,
		scope: Option<&pb::InvocationScope>,
		responses: &flume::Sender<pb::ServerFrame>,
		finished: &flume::Sender<Finished>,
		connection: &mut ConnectionState,
	) {
		use data_request::Body;

		if self.environment_authorities().is_none() {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::Unsupported,
				"DAP authority is unavailable on a session-only host",
			)
			.await;
			return;
		}

		let (operation, capability) = match &request {
			Body::DapLaunch(_) => ("omp.env.dap.launch", "env.dap.execute"),
			Body::DapAttach(_) => ("omp.env.dap.attach", "env.dap.execute"),
			Body::DapAction(request) => {
				("omp.env.dap.action", dap_command_capability(&request.command))
			},
			_ => unreachable!("DAP dispatch receives only DAP request arms"),
		};
		if !authorize_data_operation(connection, scope, operation, capability, responses, request_id)
			.await
		{
			return;
		}
		if let Err(error) = connection.quotas.reserve_stream() {
			send_policy_error(responses, request_id, error).await;
			return;
		}
		let cancel = CancellationToken::new();
		connection
			.requests
			.insert(request_id, RequestState::DataStream { cancel: cancel.clone() });
		let documents = self.documents().clone();
		let responses = responses.clone();
		let finished = finished.clone();
		tokio::spawn(async move {
			let result = match request {
				Body::DapLaunch(request) => documents
					.dap_launch(request, &cancel)
					.await
					.map(|(response, events)| (data_response::Body::DapSession(response), events)),
				Body::DapAttach(request) => documents
					.dap_attach(request, &cancel)
					.await
					.map(|(response, events)| (data_response::Body::DapSession(response), events)),
				Body::DapAction(request) => documents
					.dap_action(request, &cancel)
					.await
					.map(|(response, events)| (data_response::Body::DapAction(response), events)),
				_ => unreachable!("DAP dispatch receives only DAP request arms"),
			};
			match result {
				Ok((response, events)) => {
					for event in events {
						let body = match event {
							DapRegistryEvent::Output(output) => data_event::Body::DapOutput(output),
							DapRegistryEvent::Event(event) => data_event::Body::DapEvent(event),
						};
						send_body(
							&responses,
							request_id,
							server_frame::Body::DataEvent(pb::DataEvent {
								body: Some(body),
								..pb::DataEvent::default()
							}),
						)
						.await;
					}
					send_data_response(&responses, request_id, response).await;
				},
				Err(error) => send_document_error(&responses, request_id, &error).await,
			}
			let _ = finished
				.send_async(Finished { request_id, invocation_id: None })
				.await;
		});
	}

	pub(in crate::server) async fn dispatch_exec_session(
		&self,
		request_id: u64,
		request: pb::ExecSessionOp,
		responses: &flume::Sender<pb::ServerFrame>,
	) {
		use pb::{exec_session_op::Op, exec_session_result::Result};

		let result = match request.op {
			Some(Op::Materialize(request)) => {
				if request.wire_revision != omp_proto::SCHEMA_REV {
					send_error(
						responses,
						request_id,
						pb::ProtocolErrorCode::InvalidArgument,
						"materialization wire revision does not match the Environment schema",
					)
					.await;
					return;
				}
				if request.session.is_empty() || !self.exec.contains_session(&request.session) {
					send_error(
						responses,
						request_id,
						pb::ProtocolErrorCode::NotFound,
						"materialization exec session was not found",
					)
					.await;
					return;
				}
				match self.materializations.materialize(request).await {
					Ok(lease) => Result::Materialized(lease),
					Err(error) => {
						send_materialization_error(responses, request_id, &error).await;
						return;
					},
				}
			},
			Some(Op::ReleaseMaterialization(request)) => {
				if request.wire_revision != omp_proto::SCHEMA_REV {
					send_error(
						responses,
						request_id,
						pb::ProtocolErrorCode::InvalidArgument,
						"materialization release wire revision does not match the Environment schema",
					)
					.await;
					return;
				}
				match self.materializations.release(request).await {
					Ok(released) => Result::MaterializationReleased(released),
					Err(error) => {
						send_materialization_error(responses, request_id, &error).await;
						return;
					},
				}
			},
			Some(Op::Control(request)) => match self.exec.control(&request) {
				Ok(controlled) => Result::Controlled(controlled),
				Err(error) => {
					send_exec_error(responses, request_id, &error).await;
					return;
				},
			},
			Some(Op::Stdin(request)) => {
				match self
					.exec
					.stdin(&request.exec, match request.input.as_ref() {
						Some(stdin_frame::Input::Data(data)) => Some(data.as_ref()),
						Some(stdin_frame::Input::Eof(_)) => None,
						None => {
							send_error(
								responses,
								request_id,
								pb::ProtocolErrorCode::InvalidArgument,
								"exec stdin operation omitted input",
							)
							.await;
							return;
						},
					}) {
					Ok(()) => Result::Controlled(pb::ExecControlResult {
						exec:     request.exec,
						accepted: true,
					}),
					Err(error) => {
						send_exec_error(responses, request_id, &error).await;
						return;
					},
				}
			},
			Some(Op::Resize(request)) => {
				match self
					.exec
					.resize(&request.exec, request.rows, request.columns)
				{
					Ok(()) => Result::Controlled(pb::ExecControlResult {
						exec:     request.exec,
						accepted: true,
					}),
					Err(error) => {
						send_exec_error(responses, request_id, &error).await;
						return;
					},
				}
			},
			Some(Op::Signal(request)) => match self.exec.signal(&request.exec, &request.signal) {
				Ok(()) => {
					Result::Controlled(pb::ExecControlResult { exec: request.exec, accepted: true })
				},
				Err(error) => {
					send_exec_error(responses, request_id, &error).await;
					return;
				},
			},
			Some(Op::Capabilities(request)) => match self.exec.capabilities(&request) {
				Ok(capabilities) => Result::Capabilities(capabilities),
				Err(error) => {
					send_exec_error(responses, request_id, &error).await;
					return;
				},
			},
			Some(Op::FinalCwd(request)) => match self.exec.final_cwd(&request) {
				Ok(final_cwd) => Result::FinalCwd(final_cwd),
				Err(error) => {
					send_exec_error(responses, request_id, &error).await;
					return;
				},
			},
			None => {
				send_error(
					responses,
					request_id,
					pb::ProtocolErrorCode::InvalidArgument,
					"exec-session operation is missing",
				)
				.await;
				return;
			},
		};
		send_data_response(
			responses,
			request_id,
			data_response::Body::ExecSession(pb::ExecSessionResult { result: Some(result) }),
		)
		.await;
	}

	pub(in crate::server) async fn dispatch_mcp(
		&self,
		request_id: u64,
		request: pb::McpOp,
		responses: &flume::Sender<pb::ServerFrame>,
		finished: &flume::Sender<Finished>,
		connection: &mut ConnectionState,
	) {
		use mcp_op::Op;

		let Some(operation) = request.op else {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::InvalidArgument,
				"MCP operation is missing",
			)
			.await;
			return;
		};
		if mcp_wire_revision(&operation) != omp_proto::SCHEMA_REV {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::Unsupported,
				"MCP operation uses an unsupported wire revision",
			)
			.await;
			return;
		}
		if let Op::Status(request) = operation {
			send_data_response(
				responses,
				request_id,
				data_response::Body::Mcp(pb::McpResult {
					result: Some(mcp_result::Result::Status(self.mcp.status(request.name.as_deref()))),
				}),
			)
			.await;
			return;
		}
		if let Err(error) = connection.quotas.reserve_stream() {
			send_policy_error(responses, request_id, error).await;
			return;
		}
		let cancel = CancellationToken::new();
		connection
			.requests
			.insert(request_id, RequestState::DataStream { cancel: cancel.clone() });
		match operation {
			Op::Subscribe(request) => match self
				.mcp
				.subscribe(request.name.as_deref(), request.after_sequence)
			{
				Ok(subscription) => spawn_mcp_subscription(
					request_id,
					subscription,
					cancel,
					responses.clone(),
					finished.clone(),
				),
				Err(error) => {
					connection.requests.remove(&request_id);
					connection.quotas.release_stream();
					send_mcp_error(responses, request_id, &error).await;
				},
			},
			operation => spawn_mcp_request(
				request_id,
				Arc::clone(&self.mcp),
				operation,
				cancel,
				responses.clone(),
				finished.clone(),
			),
		}
	}

	pub(in crate::server) async fn dispatch_repository_snapshot(
		&self,
		request_id: u64,
		request: pb::RepositorySnapshotRequest,
		responses: &flume::Sender<pb::ServerFrame>,
	) {
		if request.wire_revision != omp_proto::SCHEMA_REV {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::InvalidArgument,
				"repository snapshot wire revision does not match the Environment schema",
			)
			.await;
			return;
		}
		let requested_root = if request.root_uri.is_empty() {
			self.workspace.root().to_path_buf()
		} else {
			let parsed = match Url::parse(&request.root_uri) {
				Ok(parsed) => parsed,
				Err(_) => {
					send_error(
						responses,
						request_id,
						pb::ProtocolErrorCode::InvalidArgument,
						"repository root is not a valid URI",
					)
					.await;
					return;
				},
			};
			match parsed.to_file_path() {
				Ok(path) => path,
				Err(()) => {
					send_error(
						responses,
						request_id,
						pb::ProtocolErrorCode::InvalidArgument,
						"repository root is not a local file URI",
					)
					.await;
					return;
				},
			}
		};
		let requested_root = match tokio::fs::canonicalize(&requested_root).await {
			Ok(root) if root.starts_with(self.workspace.root()) => root,
			Ok(_) => {
				send_error(
					responses,
					request_id,
					pb::ProtocolErrorCode::PermissionDenied,
					"repository root is outside the Environment workspace grant",
				)
				.await;
				return;
			},
			Err(_) => {
				send_error(
					responses,
					request_id,
					pb::ProtocolErrorCode::InvalidArgument,
					"repository root does not exist",
				)
				.await;
				return;
			},
		};
		let cancel = CancellationToken::new();
		let snapshot = match vcs::snapshot(&requested_root, &cancel).await {
			Ok(snapshot) => snapshot,
			Err(_) => {
				send_error(
					responses,
					request_id,
					pb::ProtocolErrorCode::Internal,
					"repository snapshot could not be captured",
				)
				.await;
				return;
			},
		};
		let availability = match snapshot.availability {
			RepositoryAvailability::Available => pb::RepositoryAvailability::Available,
			RepositoryAvailability::NotRepository => pb::RepositoryAvailability::NotRepository,
			RepositoryAvailability::GitUnavailable => pb::RepositoryAvailability::GitUnavailable,
		};
		let worktree_root_uri = snapshot
			.worktree_root
			.as_deref()
			.and_then(|path| Url::from_directory_path(path).ok())
			.map_or_else(String::new, |url| url.to_string());
		let primary_root_uri = snapshot
			.primary_root
			.as_deref()
			.and_then(|path| Url::from_directory_path(path).ok())
			.map_or_else(String::new, |url| url.to_string());
		self
			.send_repository_snapshot(
				request_id,
				pb::RepositorySnapshot {
					availability: availability as i32,
					worktree_root_uri,
					primary_root_uri,
					head: snapshot
						.head
						.map_or_else(String::new, |head| head.to_string()),
					branch: snapshot
						.branch
						.map_or_else(String::new, |branch| branch.to_string()),
					staged: snapshot.status_counts.staged,
					unstaged: snapshot.status_counts.unstaged,
					untracked: snapshot.status_counts.untracked,
					revision: self.repository_revision.fetch_add(1, Ordering::Relaxed) + 1,
					truncated: false,
				},
				responses,
			)
			.await;
	}

	pub(in crate::server) async fn send_repository_snapshot(
		&self,
		request_id: u64,
		snapshot: pb::RepositorySnapshot,
		responses: &flume::Sender<pb::ServerFrame>,
	) {
		send_data_response(responses, request_id, data_response::Body::RepositorySnapshot(snapshot))
			.await;
	}

	pub(in crate::server) async fn dispatch_worker(
		&self,
		request_id: u64,
		request: pb::WorkerOp,
		responses: &flume::Sender<pb::ServerFrame>,
	) {
		use pb::{worker_op::Op, worker_result::Result as WorkerResult};

		let result = match request.op {
			Some(Op::Open(open)) => {
				let key = WorkerKey {
					extension: sf!("env"),
					name:      Str::from(open.name.as_str()),
					site:      sf!("env"),
				};
				match self.workers.open(key) {
					Ok((route, lease)) => {
						lease.relinquish();
						Ok(WorkerResult::Opened(pb::WorkerOpened {
							name: route.key.name.to_string(),
							generation: route.generation,
							..pb::WorkerOpened::default()
						}))
					},
					Err(WorkerUnavailable::LayerCeiling | WorkerUnavailable::SpawnCeiling) => {
						Err((pb::ProtocolErrorCode::ResourceExhausted, "WorkerUnavailable"))
					},
					Err(WorkerUnavailable::StaleGeneration) => {
						Err((pb::ProtocolErrorCode::InvalidArgument, "stale worker generation"))
					},
				}
			},
			Some(Op::Close(close)) => {
				if self.workers.close(&close.name, close.generation) {
					Ok(WorkerResult::Closed(pb::ProcessCommandAccepted::default()))
				} else {
					Err((pb::ProtocolErrorCode::InvalidArgument, "stale worker generation"))
				}
			},
			Some(Op::Data(data)) => match self.workers.demux(data) {
				Ok(accepted) => Ok(WorkerResult::Data(pb::WorkerData {
					name: accepted.route.key.name.to_string(),
					generation: accepted.route.generation,
					channel: accepted.channel,
					data: Bytes::copy_from_slice(&accepted.data),
					..pb::WorkerData::default()
				})),
				Err(WorkerUnavailable::StaleGeneration) => {
					Err((pb::ProtocolErrorCode::InvalidArgument, "stale worker generation"))
				},
				Err(WorkerUnavailable::LayerCeiling | WorkerUnavailable::SpawnCeiling) => {
					Err((pb::ProtocolErrorCode::ResourceExhausted, "WorkerUnavailable"))
				},
			},
			Some(Op::Info(info)) => match self.workers.route(&info.name) {
				Some(route) => Ok(WorkerResult::Info(worker_info(&route))),
				None => Err((pb::ProtocolErrorCode::InvalidArgument, "unknown worker")),
			},
			Some(Op::List(_)) => Ok(WorkerResult::List(pb::WorkerList {
				workers: self.workers.routes().iter().map(worker_info).collect(),
				..pb::WorkerList::default()
			})),
			None => Err((pb::ProtocolErrorCode::InvalidArgument, "worker operation is missing")),
		};
		match result {
			Ok(result) => {
				send_body(
					responses,
					request_id,
					server_frame::Body::Data(pb::DataResponse {
						body: Some(data_response::Body::Worker(pb::WorkerResult {
							result: Some(result),
							..pb::WorkerResult::default()
						})),
						..pb::DataResponse::default()
					}),
				)
				.await;
			},
			Err((code, message)) => send_error(responses, request_id, code, message).await,
		}
	}

	pub(in crate::server) async fn dispatch_workspace(
		&self,
		request_id: u64,
		request: pb::WorkspaceOp,
		scope: Option<&pb::InvocationScope>,
		responses: &flume::Sender<pb::ServerFrame>,
		connection: &mut ConnectionState,
	) {
		use pb::workspace_op::Op;

		if self.environment_authorities().is_none() {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::Unsupported,
				"workspace authority is unavailable on a session-only host",
			)
			.await;
			return;
		}

		let Some(operation) = request.op else {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::InvalidArgument,
				"workspace operation is missing",
			)
			.await;
			return;
		};
		let operation_name = match &operation {
			Op::Snapshot(_) => "omp.env.workspace.snapshot",
			Op::List(_) => "omp.env.workspace.list",
			Op::Restore(_) => "omp.env.workspace.restore",
			Op::CaptureInputs(_) => "omp.env.workspace.capture_inputs",
		};
		if !authorize_data_operation(
			connection,
			scope,
			operation_name,
			"env.workspace.snapshot",
			responses,
			request_id,
		)
		.await
		{
			return;
		}
		let cancel = CancellationToken::new();
		let result = match operation {
			Op::Snapshot(request) => self
				.environment_authorities()
				.expect("workspace operation reached session-only host")
				.workspace_ops
				.snapshot(&request, &cancel)
				.map(workspace_result::Result::Snapshot),
			Op::List(request) => self
				.environment_authorities()
				.expect("workspace operation reached session-only host")
				.workspace_ops
				.list_snapshots(&request)
				.map(workspace_result::Result::List),
			Op::Restore(request) => self
				.environment_authorities()
				.expect("workspace operation reached session-only host")
				.workspace_ops
				.restore(&request, &cancel)
				.await
				.map(workspace_result::Result::Restored),
			Op::CaptureInputs(request) => {
				let operations = self
					.environment_authorities()
					.expect("workspace operation reached session-only host")
					.workspace_ops
					.clone();
				let cancel = cancel.clone();
				match task::spawn_blocking(move || operations.capture_inputs(&request, &cancel)).await {
					Ok(captured) => captured.map(workspace_result::Result::InputsCaptured),
					Err(source) => {
						send_error(
							responses,
							request_id,
							pb::ProtocolErrorCode::Internal,
							fault_in("input capture worker failed", source),
						)
						.await;
						return;
					},
				}
			},
		};
		match result {
			Ok(result) => {
				send_data_response(
					responses,
					request_id,
					data_response::Body::Workspace(pb::WorkspaceResult {
						result: Some(result),
						props:  Default::default(),
					}),
				)
				.await;
			},
			Err(error) => send_workspace_operation_error(responses, request_id, &error).await,
		}
	}

	pub(in crate::server) async fn dispatch_worktree(
		&self,
		request_id: u64,
		request: pb::WorktreeOp,
		scope: Option<&pb::InvocationScope>,
		responses: &flume::Sender<pb::ServerFrame>,
		connection: &mut ConnectionState,
	) {
		use pb::worktree_op::Op;

		if self.environment_authorities().is_none() {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::Unsupported,
				"worktree authority is unavailable on a session-only host",
			)
			.await;
			return;
		}

		let Some(operation) = request.op else {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::InvalidArgument,
				"worktree operation is missing",
			)
			.await;
			return;
		};
		let operation_name = match &operation {
			Op::Create(_) => "omp.env.worktree.create",
			Op::Destroy(_) => "omp.env.worktree.destroy",
			Op::Merge(_) => "omp.env.worktree.merge",
			Op::Current(_) => "omp.env.worktree",
		};
		if let Op::Current(request) = &operation
			&& request.wire_revision != omp_proto::SCHEMA_REV
		{
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::Unsupported,
				"unsupported current-worktree wire revision",
			)
			.await;
			return;
		}
		if !authorize_data_operation(
			connection,
			scope,
			operation_name,
			"env.worktree",
			responses,
			request_id,
		)
		.await
		{
			return;
		}
		let cancel = CancellationToken::new();
		let result = match operation {
			Op::Create(request) => self
				.environment_authorities()
				.expect("workspace operation reached session-only host")
				.workspace_ops
				.create_worktree(&request, &cancel)
				.map(|worktree| pb::WorktreeResult {
					worktree:      Some(worktree),
					conflicts:     Vec::new(),
					artifact_hash: Bytes::new(),
					artifact_size: 0,
					branch:        None,
					current:       None,
					props:         Default::default(),
				}),
			Op::Destroy(request) => self
				.environment_authorities()
				.expect("workspace operation reached session-only host")
				.workspace_ops
				.destroy_worktree(&request, &cancel)
				.map(|worktree| pb::WorktreeResult {
					worktree:      Some(worktree),
					conflicts:     Vec::new(),
					artifact_hash: Bytes::new(),
					artifact_size: 0,
					branch:        None,
					current:       None,
					props:         Default::default(),
				}),
			Op::Merge(request) => self
				.environment_authorities()
				.expect("workspace operation reached session-only host")
				.workspace_ops
				.merge_worktree(&request, &cancel)
				.await
				.map(|merge| pb::WorktreeResult {
					worktree:      Some(merge.worktree),
					conflicts:     merge.conflicts,
					artifact_hash: merge
						.artifact
						.map_or_else(Bytes::new, |artifact| Bytes::copy_from_slice(&artifact.hash)),
					artifact_size: merge.artifact.map_or(0, |artifact| artifact.size),
					branch:        merge.branch.map(|branch| branch.to_string()),
					current:       None,
					props:         Default::default(),
				}),
			Op::Current(_request) => self
				.environment_authorities()
				.expect("workspace operation reached session-only host")
				.workspace_ops
				.current_worktree()
				.map(|primary| pb::WorktreeResult {
					worktree:      None,
					conflicts:     Vec::new(),
					artifact_hash: Bytes::new(),
					artifact_size: 0,
					branch:        None,
					current:       Some(pb::CurrentWorktreeResult {
						primary,
						wire_revision: omp_proto::SCHEMA_REV,
						props: Default::default(),
					}),
					props:         Default::default(),
				}),
		};
		match result {
			Ok(result) => {
				send_data_response(responses, request_id, data_response::Body::Worktree(result)).await;
			},
			Err(error) => send_workspace_operation_error(responses, request_id, &error).await,
		}
	}

	pub(in crate::server) async fn dispatch_document(
		&self,
		request_id: u64,
		request: pb::DocumentOp,
		scope: Option<&pb::InvocationScope>,
		responses: &flume::Sender<pb::ServerFrame>,
		finished: &flume::Sender<Finished>,
		connection: &mut ConnectionState,
	) {
		use pb::document_op::Op;

		if self.environment_authorities().is_none() {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::Unsupported,
				"document authority is unavailable on a session-only host",
			)
			.await;
			return;
		}

		let Some(operation) = request.op else {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::InvalidArgument,
				"document operation is missing",
			)
			.await;
			return;
		};
		if !self
			.environment_authorities()
			.expect("LSP operation reached session-only host")
			.lsp_settings
			.enabled
			&& matches!(
				&operation,
				Op::GetLspBindings(_) | Op::LspStatus(_) | Op::LspRequest(_) | Op::LspNotification(_)
			) {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::Unsupported,
				"LSP operations are disabled by the resolved project settings",
			)
			.await;
			return;
		}
		let (operation_name, required) = match &operation {
			Op::Open(_) => ("omp.env.docs.open", "env.doc.read"),
			Op::Close(_) => ("omp.env.docs.close", "env.doc.read"),
			Op::Read(_) => ("omp.env.docs.read", "env.doc.read"),
			Op::Summarize(_) => ("omp.env.docs.summarize", "env.doc.read"),
			Op::CommitTransaction(_) => ("omp.env.docs.commit_transaction", "env.doc.write"),
			Op::Canonicalize(_) => ("omp.env.fs.canonicalize", "env.fs.read"),
			Op::Stat(_) => ("omp.env.fs.stat", "env.fs.read"),
			Op::ListDirectory(_) => ("omp.env.fs.list_directory", "env.fs.read"),
			Op::CreateDirectory(_) => ("omp.env.fs.create_directory", "env.fs.write"),
			Op::Remove(_) => ("omp.env.fs.remove", "env.fs.write"),
			Op::Rename(_) => ("omp.env.fs.rename", "env.fs.write"),
			Op::Copy(_) => ("omp.env.fs.copy", "env.fs.write"),
			Op::ReadLink(_) => ("omp.env.fs.read_link", "env.fs.read"),
			Op::CreateSymlink(_) => ("omp.env.fs.create_symlink", "env.fs.write"),
			Op::CreateHardLink(_) => ("omp.env.fs.create_hard_link", "env.fs.write"),
			Op::SetPermissions(_) => ("omp.env.fs.set_permissions", "env.fs.write"),
			Op::GetLspBindings(_) => ("omp.env.lsp.get_bindings", "env.lsp"),
			Op::LspStatus(_) => ("omp.env.lsp.status", "env.lsp"),
			Op::LspRequest(request) => {
				("omp.env.lsp.request", lsp_tier_capability(lsp_request_tier(&request.method)))
			},
			Op::LspNotification(request) => (
				"omp.env.lsp.notification",
				lsp_tier_capability(lsp_notification_tier(&request.method)),
			),
		};
		if !authorize_data_operation(
			connection,
			scope,
			operation_name,
			required,
			responses,
			request_id,
		)
		.await
		{
			return;
		}

		let cancel = CancellationToken::new();
		let mut opened_events: Option<(DocumentEvents, CancellationToken)> = None;
		let mut lsp_events: Option<(LspEvents, CancellationToken)> = None;
		let result = match operation {
			Op::Open(request) => {
				if let Err(error) = connection.quotas.reserve_document_lease() {
					send_policy_error(responses, request_id, error).await;
					return;
				}
				match self.documents().open_request(request, &cancel).await {
					Ok((mut lease, response)) => {
						if let Some(events) = lease.take_events() {
							if let Err(error) = connection.quotas.reserve_stream() {
								connection.quotas.release_document_lease();
								send_policy_error(responses, request_id, error).await;
								return;
							}
							let stream_cancel = CancellationToken::new();
							connection
								.requests
								.insert(request_id, RequestState::DocumentEvents {
									lease_id: lease.id().clone(),
									cancel:   stream_cancel.clone(),
								});
							opened_events = Some((events, stream_cancel));
						}
						self
							.authority
							.register_lease(lease.id().clone(), connection.connection_owner);
						connection.document_leases.insert(lease.id().clone(), lease);
						Ok(document_result::Result::Opened(response))
					},
					Err(error) => {
						connection.quotas.release_document_lease();
						Err(error)
					},
				}
			},
			Op::Close(request) => {
				if let Err(error) = self
					.authority
					.check_lease(&request.lease_id, connection.connection_owner)
				{
					send_policy_error(responses, request_id, error).await;
					return;
				}
				let lease_id = request.lease_id.clone();
				let Some(mut lease) = connection.document_leases.remove(&request.lease_id) else {
					send_error(
						responses,
						request_id,
						pb::ProtocolErrorCode::NotFound,
						"document lease is not owned by this connection",
					)
					.await;
					return;
				};
				match self
					.documents()
					.close_request(&mut lease, request, &cancel)
					.await
				{
					Ok(response) => {
						let stream_request = connection.requests.iter().find_map(|(request, state)| {
							matches!(
								state,
								RequestState::DocumentEvents { lease_id: owned, .. }
									if owned == &lease_id
							)
							.then_some(*request)
						});
						if let Some(stream_request) = stream_request
							&& let Some(RequestState::DocumentEvents { cancel, .. }) =
								connection.requests.remove(&stream_request)
						{
							cancel.cancel();
							connection.quotas.release_stream();
						}
						self
							.authority
							.release_lease(&lease_id, connection.connection_owner);
						connection.quotas.release_document_lease();
						Ok(document_result::Result::Closed(response))
					},
					Err(error) => {
						connection.document_leases.insert(lease.id().clone(), lease);
						Err(error)
					},
				}
			},
			Op::Read(request) => {
				if let Some(lease_id) = connection_lease_id(request.document.as_ref())
					&& let Err(error) = self
						.authority
						.check_lease(lease_id, connection.connection_owner)
				{
					send_policy_error(responses, request_id, error).await;
					return;
				}
				let Some(lease) = connection_lease(connection, request.document.as_ref()) else {
					send_error(
						responses,
						request_id,
						pb::ProtocolErrorCode::NotFound,
						"document lease is not owned by this connection",
					)
					.await;
					return;
				};
				self
					.documents()
					.read_request(lease, request, &cancel)
					.await
					.map(document_result::Result::Read)
			},
			Op::Summarize(request) => {
				if let Some(lease_id) = connection_lease_id(request.document.as_ref())
					&& let Err(error) = self
						.authority
						.check_lease(lease_id, connection.connection_owner)
				{
					send_policy_error(responses, request_id, error).await;
					return;
				}
				let Some(lease) = connection_lease(connection, request.document.as_ref()) else {
					send_error(
						responses,
						request_id,
						pb::ProtocolErrorCode::NotFound,
						"document lease is not owned by this connection",
					)
					.await;
					return;
				};
				self
					.documents()
					.summarize_request(lease, request, &cancel)
					.await
					.map(document_result::Result::Summarized)
			},
			Op::CommitTransaction(request) => {
				let lease_ids: Vec<Bytes> = request
					.operations
					.iter()
					.filter_map(|operation| connection_lease_id(operation.document.as_ref()).cloned())
					.collect();
				if let Some(error) = lease_ids.iter().find_map(|lease_id| {
					self
						.authority
						.check_lease(lease_id, connection.connection_owner)
						.err()
				}) {
					send_policy_error(responses, request_id, error).await;
					return;
				}
				if lease_ids.len() != request.operations.len()
					|| lease_ids
						.iter()
						.any(|lease_id| !connection.document_leases.contains_key(lease_id))
				{
					send_error(
						responses,
						request_id,
						pb::ProtocolErrorCode::NotFound,
						"transaction contains a document lease not owned by this connection",
					)
					.await;
					return;
				}
				match self
					.documents()
					.commit_transaction_request(request, &cancel)
					.await
				{
					Ok(response) => {
						if let Some(commit_transaction_response::Outcome::Committed(committed)) =
							&response.outcome
						{
							for operation in &committed.operations {
								let Some(lease_id) = lease_ids.get(operation.operation_index as usize)
								else {
									continue;
								};
								if let (Some(lease), Some(head)) =
									(connection.document_leases.get_mut(lease_id), operation.head.clone())
									&& let Err(error) = lease.advance(head)
								{
									return send_document_error(responses, request_id, &error).await;
								}
							}
						}
						Ok(document_result::Result::Transaction(response))
					},
					Err(error) => Err(error),
				}
			},
			Op::Canonicalize(request) => self
				.documents()
				.canonicalize(request, &cancel)
				.await
				.map(document_result::Result::Canonicalized),
			Op::Stat(request) => self
				.documents()
				.stat(request, &cancel)
				.await
				.map(document_result::Result::Stat),
			Op::ListDirectory(request) => self
				.documents()
				.list_directory(request, &cancel)
				.await
				.map(document_result::Result::Directory),
			Op::CreateDirectory(request) => self
				.documents()
				.create_directory(request, &cancel)
				.await
				.map(document_result::Result::DirectoryCreated),
			Op::Remove(request) => self
				.documents()
				.remove(request, &cancel)
				.await
				.map(document_result::Result::Removed),
			Op::Rename(request) => self
				.documents()
				.rename(request, &cancel)
				.await
				.map(document_result::Result::Renamed),
			Op::Copy(request) => self
				.documents()
				.copy(request, &cancel)
				.await
				.map(document_result::Result::Copied),
			Op::ReadLink(request) => self
				.documents()
				.read_link(request, &cancel)
				.await
				.map(document_result::Result::Link),
			Op::CreateSymlink(request) => self
				.documents()
				.create_symlink(request, &cancel)
				.await
				.map(document_result::Result::SymlinkCreated),
			Op::CreateHardLink(request) => self
				.documents()
				.create_hard_link(request, &cancel)
				.await
				.map(document_result::Result::HardLinkCreated),
			Op::SetPermissions(request) => self
				.documents()
				.set_permissions(request, &cancel)
				.await
				.map(document_result::Result::PermissionsSet),
			Op::GetLspBindings(request) => {
				match self.documents().get_lsp_bindings(request, &cancel).await {
					Ok(response) => {
						if let Some(events) = self.documents().take_lsp_events() {
							if let Err(error) = connection.quotas.reserve_stream() {
								send_policy_error(responses, request_id, error).await;
								return;
							}
							let stream_cancel = CancellationToken::new();
							connection
								.requests
								.insert(request_id, RequestState::LspEvents {
									cancel: stream_cancel.clone(),
								});
							lsp_events = Some((events, stream_cancel));
						}
						Ok(document_result::Result::LspBindings(response))
					},
					Err(error) => Err(error),
				}
			},
			Op::LspStatus(request) => self
				.documents()
				.lsp_status(request, &cancel)
				.await
				.map(document_result::Result::LspStatus),
			Op::LspRequest(request) => self
				.documents()
				.lsp_request(request, &cancel)
				.await
				.map(document_result::Result::LspResponse),
			Op::LspNotification(request) => self
				.documents()
				.lsp_notification(request, &cancel)
				.await
				.map(document_result::Result::LspNotified),
		};
		match result {
			Ok(result) => {
				send_data_response(
					responses,
					request_id,
					data_response::Body::Document(pb::DocumentResult {
						result: Some(result),
						props:  Default::default(),
					}),
				)
				.await;
				if let Some((events, cancel)) = opened_events {
					spawn_document_events(
						request_id,
						events,
						cancel,
						responses.clone(),
						finished.clone(),
					);
				}
				if let Some((events, cancel)) = lsp_events {
					spawn_lsp_events(request_id, events, cancel, responses.clone(), finished.clone());
				}
			},
			Err(error) => send_document_error(responses, request_id, &error).await,
		}
	}

	pub(in crate::server) async fn open_invocation(
		&self,
		request_id: u64,
		mut request: pb::InvokeTool,
		scope: Option<&pb::InvocationScope>,
		responses: &flume::Sender<pb::ServerFrame>,
		finished: &flume::Sender<Finished>,
		connection: &mut ConnectionState,
	) {
		if reject_duplicate_open(connection, request_id, responses).await {
			return;
		}
		if connection.host.is_some() && request.name == "eval" {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::PermissionDenied,
				"eval is denied to extension-host connections",
			)
			.await;
			return;
		}
		let invocation_id = Str::from(request.invocation_id.as_str());
		if invocation_id.is_empty() {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::InvalidArgument,
				"invocation_id must not be empty",
			)
			.await;
			return;
		}
		if scope.is_some_and(|scope| scope.invocation_id != invocation_id.as_str()) {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::InvalidArgument,
				"invocation scope does not match InvokeTool.invocation_id",
			)
			.await;
			return;
		}
		let principal =
			scope.filter(|scope| !scope.session_id.is_empty() && !scope.agent_id.is_empty());
		if scope.is_some_and(|scope| scope.session_id.is_empty() != scope.agent_id.is_empty()) {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::InvalidArgument,
				"session_id and agent_id must both be present or both be absent",
			)
			.await;
			return;
		}
		if let Some(open_request) = connection.invocation_ids.get(&invocation_id).copied() {
			// A terminal invocation only awaits its `Finished` sweep; its id is
			// free for a replacement open (the client observed the terminal
			// verdict before reopening).
			let live = match connection.requests.get(&open_request) {
				None | Some(RequestState::InvocationFinishing) => false,
				Some(RequestState::Invocation(InvocationState::Native { lifecycle, .. })) => {
					!lifecycle.is_terminal()
				},
				Some(_) => true,
			};
			if live {
				send_error(
					responses,
					request_id,
					pb::ProtocolErrorCode::AlreadyExists,
					"invocation_id is already open on this connection",
				)
				.await;
				return;
			}
			connection.invocation_ids.remove(&invocation_id);
		}
		let registry = self.registry();
		let Some((live_name, revision)) = registry.live_identity(&request.name) else {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::NotFound,
				unknown_tool_message(&request.name),
			)
			.await;
			return;
		};
		if revision.to_string() != request.rev {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::PreconditionFailed,
				"requested tool revision is not live",
			)
			.await;
			return;
		}
		// The exact tool revision that runs a native call also renders its
		// verdict for the model.
		let identity = ToolIdentity { name: live_name.clone(), rev: revision.clone() };
		let route = registry
			.route(&request.name)
			.expect("a live registry identity always has an execution route");
		let deadline = if request.deadline_ms == 0 {
			DEFAULT_TOOL_DEADLINE
		} else {
			Duration::from_millis(request.deadline_ms)
		};
		let output_request = match pb::OutputRequest::try_from(request.output_request) {
			Ok(pb::OutputRequest::Complete) => omp_tool::OutputRequest::Complete,
			Ok(pb::OutputRequest::Bounded | pb::OutputRequest::Unspecified) | Err(_) => {
				omp_tool::OutputRequest::Bounded
			},
		};
		let spec = registry
			.live_spec(&request.name)
			.expect("a routed tool has a live declaration");
		let maximum_effects = spec.effects.clone();
		let confinement = spec.confinement;
		// The client's roster snapshot for this call: tools that host nested
		// calls apply it to each `tool.<name>()` (the eval bridge), and a plan
		// file or read-only ceiling in it scopes every write the call makes.
		let restrictions = request
			.restrictions
			.take()
			.map(|restrictions| Arc::new(omp_tool::ToolRestrictions::from(restrictions)));
		let execution = InvocationExecutionPolicy::from_request(
			&request,
			restrictions.as_deref(),
			self.workspace.root(),
			&self.state_dir.join("sessions"),
			principal.map(|principal| principal.session_id.as_str()),
		);
		let approval_policy = if execution.core_admission {
			ApprovalPolicy::Prompt
		} else {
			connection
				.tool_settings
				.approval_for(
					invocation_id.clone(),
					request.name.as_str(),
					&maximum_effects,
					confinement,
					connection.exec_host.sandbox_state(),
				)
				.policy
		};
		// A tool that scopes its effects to each call is judged on the
		// committed arguments: its gate waits for them.
		let argument_scoped = registry.scopes_invocation_effects(&request.name);
		let gate = |name: Str| {
			if execution.core_admission {
				AdmissionGate::with_deferred_policy(
					invocation_id.clone(),
					name,
					deadline,
					approval_policy,
				)
			} else if argument_scoped {
				AdmissionGate::argument_scoped(invocation_id.clone(), name, deadline, approval_policy)
			} else {
				AdmissionGate::with_policy(invocation_id.clone(), name, deadline, approval_policy)
			}
		};
		let cancel = CancellationToken::new();
		let delivery = VerdictDelivery {
			blobs: self.blobs.clone(),
			retention_session: principal.map(|principal| Str::from(principal.session_id.as_str())),
			output_request,
		};
		if route == ToolRoute::Native {
			if principal.is_none() && request.name == "eval" {
				send_error(
					responses,
					request_id,
					pb::ProtocolErrorCode::PermissionDenied,
					"eval requires authenticated session_id and agent_id principals",
				)
				.await;
				return;
			}
			let owner = connection.invocation_owner(principal);
			let (feed, params) = IncomingParams::channel_for(Some(owner), Some(invocation_id.clone()));
			let params = params.with_restrictions(restrictions);
			let lifecycle = Arc::new(NativeLifecycle::default());
			let name = Str::from(request.name);
			let edit_repair = connection
				.supports_edit_repair()
				.then(|| ConnectionEditRepairRoute {
					request_id,
					invocation_id: invocation_id.clone(),
					responses: responses.clone(),
					next_query: Arc::new(AtomicU64::new(1)),
					pending: Arc::new(Mutex::new(None)),
				});
			let edit_repair_context = InvocationEditRepairContext::new(
				edit_repair.as_ref().map(ConnectionEditRepairRoute::client),
				connection.edit_model(),
			);
			let acp = connection.acp_routes(request_id, &invocation_id, responses);
			let acp_context = acp.context();
			let approvals = connection.owned_approvals(request_id);
			let reflection = connection.owned_reflection(request_id);
			let admission = gate(name.clone());
			let pins = argument_scoped.then(Arc::default);
			connection.requests.insert(
				request_id,
				RequestState::Invocation(InvocationState::Native {
					id: invocation_id.clone(),
					feed: feed.clone(),
					lifecycle: Arc::clone(&lifecycle),
					admission,
					pending_commit: None,
					effects: maximum_effects.clone(),
					pins: pins.clone(),
					confinement,
					execution: execution.clone(),
					request_scope: scope.map(|scope| scope.pty_denied),
					edit_repair,
					acp,
					delivery: delivery.clone(),
					cancel: cancel.clone(),
				}),
			);
			connection
				.invocation_ids
				.insert(invocation_id.clone(), request_id);
			send_body(
				responses,
				request_id,
				server_frame::Body::InvocationAccepted(pb::InvokeAccepted {
					invocation_id: invocation_id.to_string(),
					props:         Default::default(),
				}),
			)
			.await;

			spawn_native_invocation(
				request_id,
				invocation_id,
				name,
				identity,
				scope.is_some_and(|scope| scope.pty_denied),
				principal.map(|principal| Str::from(principal.session_id.as_str())),
				execution.write_scope.clone(),
				pins,
				edit_repair_context,
				acp_context,
				approvals,
				reflection,
				feed,
				deadline,
				params,
				Arc::clone(&registry),
				lifecycle,
				cancel,
				delivery,
				responses.clone(),
				finished.clone(),
			)
			.await;
		} else if matches!(route, ToolRoute::Worker { .. }) {
			let Some(owner) = self.worker_owner(&request.name, &request.rev) else {
				send_error(
					responses,
					request_id,
					pb::ProtocolErrorCode::NotFound,
					"tool name and revision are not registered to an extension host",
				)
				.await;
				return;
			};
			let name = Str::from(request.name);
			let invocation = match self.ext_hosts.open(ExtHostToolCall {
				invocation_id: invocation_id.clone(),
				name: name.clone(),
				rev: Str::from(request.rev),
				deadline,
			}) {
				Ok(invocation) => invocation,
				Err(error) => {
					send_error(responses, request_id, pb::ProtocolErrorCode::Internal, &error).await;
					return;
				},
			};
			let (interrupt, interrupts) = flume::unbounded();
			connection.requests.insert(
				request_id,
				RequestState::Invocation(InvocationState::Worker {
					id: invocation_id.clone(),
					owner,
					invocation: Some(invocation),
					committed: false,
					admission: gate(name),
					pending_commit: None,
					effects: maximum_effects,
					confinement,
					execution,
					request_scope: scope.map(|scope| scope.pty_denied),
					delivery,
					interrupt,
					interrupts: Some(interrupts),
					cancel,
				}),
			);
			connection
				.invocation_ids
				.insert(invocation_id.clone(), request_id);
			send_body(
				responses,
				request_id,
				server_frame::Body::InvocationAccepted(pb::InvokeAccepted {
					invocation_id: invocation_id.to_string(),
					props:         Default::default(),
				}),
			)
			.await;
		} else {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::NotFound,
				unknown_tool_message(&request.name),
			)
			.await;
		}
	}

	pub(in crate::server) async fn commit_invocation(
		&self,
		request_id: u64,
		mut request: pb::ArgsCommitted,
		responses: &flume::Sender<pb::ServerFrame>,
		finished: &flume::Sender<Finished>,
		connection: &mut ConnectionState,
	) {
		let already_committed = match connection.invocation_mut(request_id, &request.invocation_id) {
			Ok(InvocationState::Native { lifecycle, .. }) => lifecycle.is_committed(),
			Ok(InvocationState::Worker { committed, .. }) => *committed,
			Err((code, message)) => {
				send_error(responses, request_id, code, message).await;
				return;
			},
		};
		if already_committed {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::AlreadyExists,
				"ArgsCommitted was already received",
			)
			.await;
			return;
		}

		match connection.invocation_mut(request_id, &request.invocation_id) {
			Ok(
				InvocationState::Native { admission, pending_commit, .. }
				| InvocationState::Worker { admission, pending_commit, .. },
			) if !admission.is_answered() => {
				if pending_commit.is_some() {
					send_error(
						responses,
						request_id,
						pb::ProtocolErrorCode::AlreadyExists,
						"ArgsCommitted was already received",
					)
					.await;
				} else {
					*pending_commit = Some(request);
				}
				return;
			},
			Ok(_) => {},
			Err((code, message)) => {
				send_error(responses, request_id, code, message).await;
				return;
			},
		}

		let (admission, admitted_effects, tool, delivery, pins) = match connection
			.invocation_mut(request_id, &request.invocation_id)
		{
			Ok(InvocationState::Native { admission, effects, execution, delivery, pins, .. }) => (
				admission
					.decide(self.workspace.root(), self.workspace.root())
					.await,
				effects.clone(),
				execution.tool.clone(),
				delivery.clone(),
				pins.clone(),
			),
			Ok(InvocationState::Worker { admission, effects, execution, delivery, .. }) => (
				admission
					.decide(self.workspace.root(), self.workspace.root())
					.await,
				effects.clone(),
				execution.tool.clone(),
				delivery.clone(),
				None,
			),
			Err((code, message)) => {
				send_error(responses, request_id, code, message).await;
				return;
			},
		};
		let decision = match admission {
			AdmissionDecision::Allowed { raw, bash, rewritten } => {
				let _effective_bash = bash;
				// An answer that rewrote the arguments is judged again, never
				// past what was admitted; unchanged arguments keep the envelope
				// they were judged under.
				if rewritten {
					effective_effects(&self.registry, &tool, &raw, &admitted_effects, pins.as_ref())
						.map(|effects| (raw, effects))
						.ok_or_else(|| widened_effects_denial(&request.invocation_id))
				} else {
					Ok((raw, admitted_effects))
				}
			},
			AdmissionDecision::Denied(policy) => Err(policy),
		};
		let (raw, call_effects) = match decision {
			Ok(effective) => effective,
			Err(policy) => {
				let invocation_id = Str::from(request.invocation_id.as_str());
				connection.abandon_admission(request_id, &invocation_id);
				send_policy_denied_verdict(responses, request_id, &invocation_id, &delivery, policy)
					.await;
				return;
			},
		};
		request.raw = raw;
		let Some(narrowed_effects) =
			effects_narrow_or_refuse(request.effects.as_ref(), &call_effects)
		else {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::PermissionDenied,
				"ArgsCommitted effect envelope widens the declared tool authority",
			)
			.await;
			return;
		};
		request.effects = Some((&narrowed_effects).into());
		let result = connection.invocation_mut(request_id, &request.invocation_id);
		match result {
			Ok(InvocationState::Native { feed, lifecycle, .. }) => {
				let Ok(raw) = str::from_utf8(&request.raw) else {
					send_error(
						responses,
						request_id,
						pb::ProtocolErrorCode::InvalidArgument,
						"committed arguments are not UTF-8",
					)
					.await;
					return;
				};
				match lifecycle.commit() {
					Ok(()) => {},
					Err(NativeCommitError::AlreadyCommitted) => {
						send_error(
							responses,
							request_id,
							pb::ProtocolErrorCode::AlreadyExists,
							"ArgsCommitted was already received",
						)
						.await;
						return;
					},
					Err(NativeCommitError::Terminal) => {
						send_error(
							responses,
							request_id,
							pb::ProtocolErrorCode::PreconditionFailed,
							"native invocation is already terminal",
						)
						.await;
						return;
					},
				}
				if feed.args_committed(Str::from(raw)).is_err() {
					send_error(
						responses,
						request_id,
						pb::ProtocolErrorCode::Cancelled,
						"invocation input is closed",
					)
					.await;
				}
			},
			Ok(InvocationState::Worker {
				id,
				owner,
				invocation,
				committed,
				cancel,
				interrupts,
				delivery,
				..
			}) => {
				if *committed {
					send_error(
						responses,
						request_id,
						pb::ProtocolErrorCode::AlreadyExists,
						"ArgsCommitted was already received",
					)
					.await;
					return;
				}
				if request.effect_token.is_empty() || request.authorized_at_ms == 0 {
					send_policy_error(responses, request_id, PolicyError::InvalidEffectToken).await;
					return;
				}
				let Some(worker) = invocation.as_mut() else {
					send_error(
						responses,
						request_id,
						pb::ProtocolErrorCode::PreconditionFailed,
						"worker invocation was already dispatched",
					)
					.await;
					return;
				};
				if let Err(error) = worker.args_committed(request) {
					self.authority.settle(owner, id);
					send_error(responses, request_id, pb::ProtocolErrorCode::PreconditionFailed, &error)
						.await;
					return;
				}
				*committed = true;
				let Some(invocation) = invocation.take() else {
					return;
				};
				let Some(interrupts) = interrupts.take() else {
					return;
				};
				spawn_worker_invocation(
					request_id,
					id.clone(),
					invocation,
					cancel.clone(),
					interrupts,
					delivery.clone(),
					responses.clone(),
					finished.clone(),
				);
			},
			Err((code, message)) => send_error(responses, request_id, code, message).await,
		}
	}

	pub(in crate::server) fn worker_owner(&self, name: &str, rev: &str) -> Option<HostKey> {
		self
			.ext_hosts
			.registrations()
			.iter()
			.find_map(|registration| {
				let declaration = &registration.declaration;
				(declaration.rev == rev
					&& declaration
						.definition
						.as_ref()
						.is_some_and(|definition| definition.name == name))
				.then(|| registration.owner.clone())
			})
	}

	pub(in crate::server) async fn put_chunk(
		&self,
		request_id: u64,
		chunk: blob_pb::Chunk,
		responses: &flume::Sender<pb::ServerFrame>,
		connection: &mut ConnectionState,
	) {
		if let Err(error) = connection.quotas.charge_blob_bytes(chunk.data.len()) {
			send_policy_error(responses, request_id, error).await;
			return;
		}
		connection
			.requests
			.entry(request_id)
			.or_insert_with(|| RequestState::BlobPut(BlobUpload::default()));
		let Some(RequestState::BlobPut(upload)) = connection.requests.get_mut(&request_id) else {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::AlreadyExists,
				"request_id is already open for another operation",
			)
			.await;
			return;
		};
		if upload.chunks != 0 && (!chunk.hash.is_empty() || chunk.size.is_some()) {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::InvalidArgument,
				"blob hash and size metadata are legal only on the first chunk",
			)
			.await;
			return;
		}
		if upload.chunks == 0 {
			upload.expected_hash = (!chunk.hash.is_empty()).then_some(chunk.hash);
			upload.expected_size = chunk.size;
		}
		upload.data.extend_from_slice(&chunk.data);
		upload.chunks += 1;
	}

	pub(in crate::server) async fn commit_blob(
		&self,
		request_id: u64,
		responses: &flume::Sender<pb::ServerFrame>,
		connection: &mut ConnectionState,
	) {
		let upload = match connection.requests.remove(&request_id) {
			Some(RequestState::BlobPut(upload)) => upload,
			Some(other) => {
				connection.requests.insert(request_id, other);
				send_error(
					responses,
					request_id,
					pb::ProtocolErrorCode::AlreadyExists,
					"request_id is already open for another operation",
				)
				.await;
				return;
			},
			None => BlobUpload::default(),
		};
		match self.blobs.put_checked(
			&upload.data,
			upload.expected_hash.as_deref(),
			upload.expected_size,
		) {
			Ok(id) => {
				send_body(
					responses,
					request_id,
					server_frame::Body::BlobPut(blob_pb::PutResponse {
						hash: Bytes::copy_from_slice(&id.hash),
						size: id.size,
					}),
				)
				.await;
			},
			Err(error) => send_blob_error(responses, request_id, &error).await,
		}
	}
}
