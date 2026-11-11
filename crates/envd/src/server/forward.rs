//! Invocation forwarding, projection, and frame IO.

use super::{
	BLOB_CHUNK_BYTES, ConnectionState, FRAME_LIMIT, Finished, INVOCATION_RESPONSE_SEND_GRACE,
	MAX_RESOURCE_URI_BYTES, NATIVE_CANCEL_GRACE, NativeLifecycle, VerdictDelivery, prelude::*,
};

pub(in crate::server) async fn reject_duplicate_open(
	connection: &ConnectionState,
	request_id: u64,
	responses: &flume::Sender<pb::ServerFrame>,
) -> bool {
	if connection.requests.contains_key(&request_id) {
		send_error(
			responses,
			request_id,
			pb::ProtocolErrorCode::AlreadyExists,
			"request_id is already open",
		)
		.await;
		true
	} else {
		false
	}
}

pub(in crate::server) enum NativeForward {
	Continue,
	Terminal,
	Backpressure,
}

pub(in crate::server) async fn spawn_native_invocation(
	request_id: u64,
	invocation_id: Str,
	name: Str,
	identity: ToolIdentity,
	pty_denied: bool,
	session_id: Option<Str>,
	write_scope: Option<Arc<WriteScope>>,
	pins: Option<Arc<InvocationPins>>,
	edit_repair: InvocationEditRepairContext,
	acp: InvocationAcpBackends,
	approvals: Option<OwnedApprovals>,
	reflection: Option<OwnedReflection>,
	feed: omp_tool::InvocationFeed,
	deadline: Duration,
	params: IncomingParams<'static>,
	registry: Arc<Registry>,
	lifecycle: Arc<NativeLifecycle>,
	cancel: CancellationToken,
	delivery: VerdictDelivery,
	responses: flume::Sender<pb::ServerFrame>,
	finished: flume::Sender<Finished>,
) {
	let (started, start) = flume::bounded(1);
	// The invocation body is the large future here. It is boxed once so the
	// task-local scope wrappers around it move a pointer, not the body: nested
	// by value, six wrappers overflow a worker stack in debug builds. The
	// approval and reflection relay scopes sit inside the box for the same
	// reason. The pins the call's judgment fixed wrap it all, so the executor
	// reaches what was judged.
	let invocation = write_scope::scoped(
		write_scope,
		with_invocation_scope(
			pty_denied,
			with_output_request_scope(
				delivery.output_request,
				with_invocation_session_scope(
					session_id,
					with_edit_repair_scope(
						edit_repair,
						with_acp_scope(
							acp,
							Box::pin(with_invocation_approvals(
								approvals,
								with_invocation_reflection(reflection, async move {
									let result = registry.invoke(&name, params);
									let _ = started.send(());
									match result {
										Ok(mut stream) => {
											let mut deadline = Box::pin(time::sleep(deadline));
											let mut cancel_grace: Option<pin::Pin<Box<Sleep>>> = None;
											let mut timed_out = false;
											let mut grace_expired = false;
											loop {
												if lifecycle.is_terminal() {
													break;
												}
												if let Some(grace) = cancel_grace.as_mut() {
													tokio::select! {
														biased;
														() = grace.as_mut() => {
															grace_expired = true;
															break;
														},
														event = stream.next() => {
															let reason = if timed_out {
																"native invocation ended without reporting timeout truth"
															} else {
																"native invocation ended without reporting cancellation truth"
															};
															if matches!(
																forward_native_event(
																	event,
																	true,
																	reason,
																	request_id,
																	&invocation_id,
																	&identity,
																	&registry,
																	&lifecycle,
																	&delivery,
																	&responses,
																)
																.await,
																NativeForward::Terminal
															) {
																break;
															}
														},
													}
												} else {
													tokio::select! {
														biased;
														() = deadline.as_mut() => {
															let reason = sf!("native invocation deadline exceeded");
															let _ = feed.interrupt(Interrupt {
																class: sf!("deadline"),
																reason: reason.clone(),
															});
															if lifecycle.is_committed() {
																timed_out = true;
																cancel_grace = Some(Box::pin(time::sleep(
																	NATIVE_CANCEL_GRACE,
																)));
															} else if lifecycle.claim_precommit_terminal() {
																send_abort_verdict(
																	&responses,
																	request_id,
																	&invocation_id,
																	&delivery,
																	omp_tool::Abort::Interrupted { reason },
																)
																.await;
																break;
															} else {
																break;
															}
														},
														() = cancel.cancelled() => {
															if lifecycle.is_committed() {
																cancel_grace = Some(Box::pin(time::sleep(
																	NATIVE_CANCEL_GRACE,
																)));
															} else {
																break;
															}
														},
														event = stream.next() => {
															match forward_native_event(
																event,
																false,
																"",
																request_id,
																&invocation_id,
																&identity,
																&registry,
																&lifecycle,
																&delivery,
																&responses,
															)
															.await
															{
																NativeForward::Continue => {},
																NativeForward::Terminal => break,
																NativeForward::Backpressure => {
																	let _ = feed.interrupt(Interrupt {
																		class: sf!("backpressure"),
																		reason: sf!(
																			"invocation response consumer stopped reading",
																		),
																	});
																	if lifecycle.is_committed() {
																		cancel_grace = Some(Box::pin(time::sleep(
																			NATIVE_CANCEL_GRACE,
																		)));
																	} else {
																		lifecycle.claim_terminal();
																		break;
																	}
																},
															}
														},
													}
												}
											}
											if grace_expired
												&& lifecycle.is_committed()
												&& lifecycle.claim_terminal()
											{
												drop(stream);
												let reason = if timed_out {
													sf!(
														"native invocation exceeded its deadline and did not \
														 stop within grace",
													)
												} else {
													sf!(
														"native invocation did not stop within cancellation \
														 grace"
													)
												};
												send_abort_verdict(
													&responses,
													request_id,
													&invocation_id,
													&delivery,
													omp_tool::Abort::EffectsUnknown { reason },
												)
												.await;
											}
										},
										Err(error) => {
											if lifecycle.claim_terminal() {
												let _ = send_invocation_error(
													&responses,
													request_id,
													pb::ProtocolErrorCode::NotFound,
													&error,
												)
												.await;
											}
										},
									}
									let _ = finished
										.send_async(Finished {
											request_id,
											invocation_id: Some(invocation_id),
										})
										.await;
								}),
							)),
						),
					),
				),
			),
		),
	);
	tokio::spawn(InvocationPins::scope(pins, invocation));
	let _ = start.recv_async().await;
}

pub(in crate::server) fn authorize_visibility_receipt(
	registry: &Registry,
	request: pb::VisibilityAuthorization,
) -> Result<(), (pb::ProtocolErrorCode, &'static str)> {
	let (name, rev) = registry
		.live_identity(&request.name)
		.ok_or((pb::ProtocolErrorCode::NotFound, "visibility tool is not registered"))?;
	if rev.to_string() != request.rev {
		return Err((
			pb::ProtocolErrorCode::PreconditionFailed,
			"visibility tool revision is not live",
		));
	}
	let identity = ToolIdentity { name: name.clone(), rev: rev.clone() };
	let mut lines = request
		.lines
		.into_iter()
		.map(|line| {
			let number = usize::try_from(line.line)
				.map_err(|_| (pb::ProtocolErrorCode::InvalidArgument, "visibility line is invalid"))?;
			if number == 0 || line.source_key.is_empty() {
				return Err((pb::ProtocolErrorCode::InvalidArgument, "visibility line is invalid"));
			}
			Ok(omp_tool::VisibleSourceLine {
				source_key: Str::from(line.source_key),
				line:       number,
			})
		})
		.collect::<Result<Vec<_>, _>>()?;
	lines.sort_unstable();
	lines.dedup();
	let receipt = omp_tool::VisibilityReceipt { lines };
	let projected = registry
		.project_verdict(&identity, &request.verdict, false, &native_prompt_caps(&identity.rev))
		.map_err(|_| (pb::ProtocolErrorCode::InvalidArgument, "visibility verdict is invalid"))?;
	if receipt.lines.iter().any(|line| {
		!projected
			.visibility
			.iter()
			.any(|span| span.source_key == line.source_key && span.line == line.line)
	}) {
		return Err((
			pb::ProtocolErrorCode::PermissionDenied,
			"visibility receipt contains an unprojected source line",
		));
	}
	registry
		.authorize_visibility(&identity, &request.verdict, &receipt)
		.map_err(|source| {
			tracing::warn!(%source, tool = %identity.name, "visibility authorization failed");
			(pb::ProtocolErrorCode::PermissionDenied, "visibility authorization rejected")
		})
}

pub(in crate::server) const DEFAULT_RESULT_PROJECTION_BYTES: usize = 64 * 1024;
const COMPLETE_RESULT_PROJECTION_BYTES: usize = 8 * 1024 * 1024;

pub(in crate::server) fn utf8_projection_prefix(text: &str, maximum: usize) -> usize {
	let mut end = maximum.min(text.len());
	while end > 0 && !text.is_char_boundary(end) {
		end -= 1;
	}
	end
}

pub(in crate::server) fn valid_blob_media_type(media_type: &str) -> bool {
	if media_type.bytes().any(|byte| byte.is_ascii_control()) {
		return false;
	}
	let essence = media_type.split(';').next().unwrap_or_default().trim();
	let Some((kind, subtype)) = essence.split_once('/') else {
		return false;
	};
	!kind.is_empty()
		&& !subtype.is_empty()
		&& !essence.bytes().any(|byte| byte.is_ascii_whitespace())
}

pub(in crate::server) fn project_wire_parts(
	parts: &mut Vec<thread_pb::Part>,
	request: omp_tool::OutputRequest,
) -> (u64, u64, bool) {
	project_wire_parts_with_visibility(parts, &mut Vec::new(), request)
}

pub(in crate::server) fn project_wire_parts_with_visibility(
	parts: &mut Vec<thread_pb::Part>,
	visibility: &mut Vec<pb::ProjectionSpan>,
	request: omp_tool::OutputRequest,
) -> (u64, u64, bool) {
	let limit = match request {
		omp_tool::OutputRequest::Bounded => DEFAULT_RESULT_PROJECTION_BYTES,
		omp_tool::OutputRequest::Complete => COMPLETE_RESULT_PROJECTION_BYTES,
	};
	let mut source_bytes = 0_u64;
	let mut inline_bytes = 0_u64;
	let mut remaining = limit;
	let mut omitted = false;
	let mut remapped = (!visibility.is_empty()).then(|| Vec::with_capacity(parts.len()));
	let mut retained = 0_u64;
	parts.retain_mut(|part| {
		let original = part.encoded_len();
		source_bytes = source_bytes.saturating_add(u64::try_from(original).unwrap_or(u64::MAX));
		if original <= remaining {
			remaining -= original;
			inline_bytes = inline_bytes.saturating_add(u64::try_from(original).unwrap_or(u64::MAX));
			if let Some(remapped) = &mut remapped {
				remapped.push(Some(retained));
			}
			retained += 1;
			return true;
		}
		let text = match part.kind.as_mut() {
			Some(thread_pb::part::Kind::Text(text)) => Some(text),
			Some(thread_pb::part::Kind::Thinking(thinking)) => Some(&mut thinking.text),
			_ => None,
		};
		let Some(text) = text else {
			omitted = true;
			if let Some(remapped) = &mut remapped {
				remapped.push(None);
			}
			return false;
		};
		let overhead = original.saturating_sub(text.len());
		let keep = utf8_projection_prefix(text, remaining.saturating_sub(overhead));
		text.truncate(keep);
		let projected = part.encoded_len();
		omitted = true;
		if projected == 0 || projected > remaining {
			if let Some(remapped) = &mut remapped {
				remapped.push(None);
			}
			return false;
		}
		remaining -= projected;
		inline_bytes = inline_bytes.saturating_add(u64::try_from(projected).unwrap_or(u64::MAX));
		if let Some(remapped) = &mut remapped {
			remapped.push(Some(retained));
		}
		retained += 1;
		true
	});
	if let Some(remapped) = remapped {
		visibility.retain_mut(|span| {
			let Some(part) = usize::try_from(span.part)
				.ok()
				.and_then(|part| remapped.get(part))
				.copied()
				.flatten()
			else {
				return false;
			};
			span.part = part;
			let Some(thread_pb::part::Kind::Text(text)) = parts[part as usize].kind.as_ref() else {
				return false;
			};
			let (Ok(start), Ok(end)) =
				(usize::try_from(span.start_byte), usize::try_from(span.end_byte))
			else {
				return false;
			};
			span.line != 0
				&& !span.source_key.is_empty()
				&& start < end
				&& end <= text.len()
				&& text.is_char_boundary(start)
				&& text.is_char_boundary(end)
		});
	}
	(source_bytes, inline_bytes, omitted)
}

pub(in crate::server) fn output_projection(
	request: omp_tool::OutputRequest,
	source_bytes: u64,
	inline_bytes: u64,
	omitted: bool,
	artifact: Option<thread_pb::Blob>,
) -> pb::OutputProjection {
	pb::OutputProjection {
		request: match request {
			omp_tool::OutputRequest::Bounded => pb::OutputRequest::Bounded as i32,
			omp_tool::OutputRequest::Complete => pb::OutputRequest::Complete as i32,
		},
		source_bytes,
		inline_bytes,
		omitted,
		artifact,
		model_text: None,
	}
}

pub(in crate::server) async fn forward_native_event(
	event: Option<Result<ErasedEv, omp_tool::RegistryError>>,
	cancelling: bool,
	fallback_reason: &str,
	request_id: u64,
	invocation_id: &Str,
	identity: &ToolIdentity,
	registry: &Registry,
	lifecycle: &NativeLifecycle,
	delivery: &VerdictDelivery,
	responses: &flume::Sender<pb::ServerFrame>,
) -> NativeForward {
	match event {
		Some(Ok(ErasedEv::Update(_))) if cancelling => NativeForward::Continue,
		Some(Ok(ErasedEv::Update(_))) if lifecycle.is_terminal() => NativeForward::Terminal,
		Some(Ok(ErasedEv::Update(json))) => {
			if send_invocation_body(
				responses,
				request_id,
				server_frame::Body::Update(pb::Update {
					invocation_id: invocation_id.to_string(),
					json,
					props: Default::default(),
				}),
			)
			.await
			{
				NativeForward::Continue
			} else {
				NativeForward::Backpressure
			}
		},
		Some(Ok(ErasedEv::Done(outcome))) => {
			if lifecycle.claim_terminal() {
				send_native_outcome(
					responses,
					request_id,
					invocation_id,
					identity,
					registry,
					delivery,
					outcome,
				)
				.await;
			}
			NativeForward::Terminal
		},
		Some(Err(error)) if !cancelling => {
			if lifecycle.claim_terminal() {
				let _ = send_invocation_error(
					responses,
					request_id,
					pb::ProtocolErrorCode::Internal,
					&error.to_string(),
				)
				.await;
			}
			NativeForward::Terminal
		},
		None if !cancelling => {
			if lifecycle.claim_terminal() {
				let _ = send_invocation_stream_error(
					responses,
					request_id,
					invocation_id,
					"tool event stream closed without a terminal verdict",
				)
				.await;
			}
			NativeForward::Terminal
		},
		Some(Err(_)) | None => {
			if lifecycle.is_committed() && lifecycle.claim_terminal() {
				send_abort_verdict(
					responses,
					request_id,
					invocation_id,
					delivery,
					omp_tool::Abort::EffectsUnknown { reason: Str::from(fallback_reason) },
				)
				.await;
			}
			NativeForward::Terminal
		},
	}
}

pub(in crate::server) fn spawn_worker_invocation(
	request_id: u64,
	invocation_id: Str,
	mut invocation: ExtHostInvocation,
	cancel: CancellationToken,
	interrupts: Receiver<pb::Interrupt>,
	delivery: VerdictDelivery,
	responses: flume::Sender<pb::ServerFrame>,
	finished: flume::Sender<Finished>,
) {
	tokio::spawn(async move {
		let VerdictDelivery { blobs, retention_session, output_request } = &delivery;
		let output_request = *output_request;
		let mut cancel_requested = false;
		loop {
			let event = if cancel_requested {
				invocation.next().await.ok()
			} else {
				tokio::select! {
					biased;
					() = cancel.cancelled() => {
						invocation.cancel("environment invocation cancelled");
						cancel_requested = true;
						continue;
					},
					frame = interrupts.recv_async() => {
						if let Ok(frame) = frame {
							let _ = invocation.interrupt(frame);
						}
						continue;
					},
					event = invocation.next() => event.ok(),
				}
			};
			match event {
				Some(ExtHostEvent::Update(_)) if cancel_requested => {},
				Some(ExtHostEvent::Update(update)) => {
					if !send_invocation_body(
						&responses,
						request_id,
						server_frame::Body::Update(pb::Update {
							invocation_id: invocation_id.to_string(),
							json:          update.json,
							props:         Default::default(),
						}),
					)
					.await
					{
						invocation.cancel("invocation response consumer stopped reading");
						cancel_requested = true;
					}
				},
				Some(ExtHostEvent::ProtocolError(error)) => {
					let _ = send_invocation_error(
						&responses,
						request_id,
						pb::ProtocolErrorCode::Internal,
						&error.message,
					)
					.await;
					break;
				},
				Some(ExtHostEvent::Complete(complete)) => {
					let (json, details_blob, is_error) = match projected_worker_completion_json(
						blobs,
						&complete,
						output_request,
						retention_session.as_deref(),
						&invocation_id,
					) {
						Ok(completion) => completion,
						Err(reason) => {
							send_abort_verdict(
								&responses,
								request_id,
								&invocation_id,
								&delivery,
								omp_tool::Abort::EffectsUnknown { reason },
							)
							.await;
							break;
						},
					};
					if let Some(details) = details_blob.as_ref() {
						let hash: [u8; 32] = match details.hash.as_ref().try_into() {
							Ok(hash) => hash,
							Err(_) => {
								send_abort_verdict(
									&responses,
									request_id,
									&invocation_id,
									&delivery,
									omp_tool::Abort::EffectsUnknown {
										reason: sf!("worker verdict CAS returned an invalid hash"),
									},
								)
								.await;
								break;
							},
						};
						if let Err(error) = blobs.retain_verdict(
							retention_session.as_deref(),
							invocation_id.as_str(),
							BlobId { hash, size: details.size },
						) {
							tracing::error!(
								%error,
								invocation_id = %invocation_id,
								"could not durably retain worker verdict before publication"
							);
							send_abort_verdict(
								&responses,
								request_id,
								&invocation_id,
								&delivery,
								omp_tool::Abort::EffectsUnknown {
									reason: sf!("worker verdict could not be retained"),
								},
							)
							.await;
							break;
						}
					}
					let mut retained_media = BTreeSet::new();
					let mut invalid_media = false;
					for blob in complete
						.parts
						.iter()
						.filter_map(|part| match part.kind.as_ref() {
							Some(thread_pb::part::Kind::Blob(blob)) => Some(blob),
							_ => None,
						}) {
						if !valid_blob_media_type(&blob.mime) {
							invalid_media = true;
							break;
						}
						if !blob.inline.is_empty() {
							let size = u64::try_from(blob.inline.len()).unwrap_or(u64::MAX);
							if size != blob.size
								|| (!blob.hash.is_empty()
									&& blob.hash.as_ref() != Hash32::sum(&blob.inline).as_bytes())
							{
								invalid_media = true;
								break;
							}
							continue;
						}
						let Ok(hash) = <[u8; 32]>::try_from(blob.hash.as_ref()) else {
							invalid_media = true;
							break;
						};
						if retained_media.insert((hash, blob.size))
							&& let Err(error) = blobs.retain_verdict_blob(
								retention_session.as_deref(),
								invocation_id.as_str(),
								BlobId { hash, size: blob.size },
							) {
							tracing::error!(
								%error,
								invocation_id = %invocation_id,
								"could not durably retain worker media before publication"
							);
							invalid_media = true;
							break;
						}
					}
					if invalid_media {
						send_abort_verdict(
							&responses,
							request_id,
							&invocation_id,
							&delivery,
							omp_tool::Abort::EffectsUnknown {
								reason: sf!("worker verdict media is invalid or unavailable"),
							},
						)
						.await;
						break;
					}
					let mut parts = complete.parts;
					let _ = project_wire_parts(&mut parts, output_request);
					let source_bytes = details_blob.as_ref().map_or_else(
						|| u64::try_from(json.len()).unwrap_or(u64::MAX),
						|details| details.size,
					);
					let inline_bytes = u64::try_from(json.len()).unwrap_or(u64::MAX);
					let projection = output_projection(
						output_request,
						source_bytes,
						inline_bytes,
						inline_bytes != source_bytes,
						details_blob.clone(),
					);
					send_invocation_terminal_body(
						&responses,
						request_id,
						server_frame::Body::Verdict(pb::Verdict {
							invocation_id: invocation_id.to_string(),
							json,
							parts,
							details_blob,
							is_error,
							useless: complete.useless,
							terminate: complete.terminate.then_some(true),
							projection: Some(projection),
							visibility: Vec::new(),
							props: Default::default(),
						}),
					)
					.await;
					break;
				},
				Some(ExtHostEvent::Aborted(abort)) => {
					let reason = if cancel_requested {
						sf!("environment invocation cancelled")
					} else {
						abort.reason
					};
					let reason = if abort.effects_unknown {
						omp_tool::Abort::EffectsUnknown { reason }
					} else {
						omp_tool::Abort::Skipped { reason }
					};
					send_abort_verdict(&responses, request_id, &invocation_id, &delivery, reason).await;
					break;
				},
				None => {
					let reason = if cancel_requested {
						sf!("environment invocation cancelled")
					} else {
						sf!("extension host event stream closed after effects authorization")
					};
					send_abort_verdict(
						&responses,
						request_id,
						&invocation_id,
						&delivery,
						omp_tool::Abort::EffectsUnknown { reason },
					)
					.await;
					break;
				},
			}
		}
		let _ = finished
			.send_async(Finished { request_id, invocation_id: Some(invocation_id) })
			.await;
	});
}

/// Why one native media part cannot travel with its verdict.
#[derive(Debug, Error)]
pub(in crate::server) enum NativeMediaError {
	/// The part names a media type a client refuses.
	#[error("media type {media_type:?} is not a valid MIME type")]
	MediaType {
		/// The refused media type.
		media_type: Str,
	},
	/// The part's hash is not a SHA-256 digest in lowercase hex.
	#[error("media blob hash is not a SHA-256 hex digest")]
	Hash(#[source] blob::Error),
	/// The blob could not be copied into the verdict store or leased there.
	#[error("media blob could not be retained for delivery")]
	Retain(#[source] BlobError),
}

/// The projection budget an in-process dispatch hands a native tool
/// (`Dispatcher::finish`): every part, all text and media. The parts are
/// bounded afterwards, once here by the caller's output request and again by
/// the client's dispatcher.
pub(in crate::server) fn native_prompt_caps(rev: &omp_tool::Rev) -> PromptCaps {
	PromptCaps::for_tool(
		CapsBase {
			maximum_parts:      u16::MAX,
			maximum_text_bytes: u32::MAX,
			media:              true,
			model_class:        ModelClass::Standard,
		},
		rev,
	)
}

/// One model-visible text part.
pub(in crate::server) const fn wire_text(text: String) -> thread_pb::Part {
	thread_pb::Part { kind: Some(thread_pb::part::Kind::Text(text)) }
}

/// Retains one native media blob for this invocation's delivery, so the
/// client can replicate it under the verdict's lease, and returns its wire
/// reference. `retained` dedupes blobs a projection names twice.
pub(in crate::server) fn retain_native_media(
	delivery: &VerdictDelivery,
	invocation_id: &str,
	media: &omp_tool::BlobRef,
	retained: &mut BTreeSet<([u8; 32], u64)>,
) -> Result<thread_pb::Blob, NativeMediaError> {
	if !valid_blob_media_type(&media.media_type) {
		return Err(NativeMediaError::MediaType { media_type: media.media_type.clone() });
	}
	let id = BlobId::from(
		blob::BlobRef::parse_hex(media.hash.as_str(), media.byte_len)
			.map_err(NativeMediaError::Hash)?,
	);
	if retained.insert((id.hash, id.size)) {
		delivery
			.blobs
			.retain_verdict_blob(delivery.retention_session.as_deref(), invocation_id, id)
			.map_err(NativeMediaError::Retain)?;
	}
	Ok(thread_pb::Blob {
		hash: Bytes::copy_from_slice(&id.hash),
		mime: media.media_type.to_string(),
		size: id.size,
		..thread_pb::Blob::default()
	})
}

/// Converts a native tool's projected parts to the thread parts `Verdict`
/// carries, the way the session renders tool parts for a model: JSON as its
/// text, and media as its alternative text followed by the blob. A blob that
/// cannot be retained is left out, with a warning; its alternative text stays.
pub(in crate::server) fn native_wire_parts(
	parts: &[omp_tool::Part],
	delivery: &VerdictDelivery,
	invocation_id: &str,
	visibility: &[omp_tool::ProjectionSpan],
) -> (Vec<thread_pb::Part>, Vec<pb::ProjectionSpan>) {
	let mut wire = Vec::with_capacity(parts.len());
	let mut retained = BTreeSet::new();
	let mut remapped = (!visibility.is_empty()).then(|| Vec::with_capacity(parts.len()));
	for part in parts {
		if let Some(remapped) = &mut remapped {
			remapped.push(wire.len());
		}
		match part {
			omp_tool::Part::Text { text } => wire.push(wire_text(text.as_str().to_owned())),
			omp_tool::Part::Json { json } => {
				wire.push(wire_text(String::from_utf8_lossy(json).into_owned()));
			},
			omp_tool::Part::Blob { blob, alt } => {
				if let Some(alt) = alt {
					wire.push(wire_text(alt.as_str().to_owned()));
				}
				match retain_native_media(delivery, invocation_id, blob, &mut retained) {
					Ok(media) => {
						wire.push(thread_pb::Part { kind: Some(thread_pb::part::Kind::Blob(media)) });
					},
					Err(error) => tracing::warn!(
						%error,
						invocation_id,
						"native verdict media is unavailable; publishing the verdict without it"
					),
				}
			},
		}
	}
	let visibility = remapped.map_or_else(Vec::new, |remapped| {
		visibility
			.iter()
			.filter_map(|span| {
				let part = *remapped.get(span.part)?;
				matches!(
					parts.get(span.part)?,
					omp_tool::Part::Text { .. } | omp_tool::Part::Json { .. }
				)
				.then(|| pb::ProjectionSpan {
					part:       part as u64,
					start_byte: span.start_byte as u64,
					end_byte:   span.end_byte as u64,
					source_key: span.source_key.as_str().to_owned(),
					line:       span.line as u64,
				})
			})
			.collect()
	});
	(wire, visibility)
}

/// Model-facing text for a native verdict its tool could not project. The
/// verdict itself is still the tool's own and is published unchanged.
pub(in crate::server) const UNPROJECTED_NATIVE_RESULT: &str =
	"The tool finished, but its result could not be rendered for the model.";

/// Publishes a native tool's terminal with the model-facing parts its own
/// projection renders, as an in-process dispatch would (`Dispatcher::finish`),
/// so an environment-run tool reaches the model with its result. A verdict its
/// tool cannot project is still published as the tool's own outcome, with a
/// harness note in place of the parts: the call completed, so it is never
/// reported as an abort with unknown effects.
pub(in crate::server) async fn send_native_outcome(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	invocation_id: &Str,
	identity: &ToolIdentity,
	registry: &Registry,
	delivery: &VerdictDelivery,
	outcome: ErasedOutcome,
) {
	match outcome {
		ErasedOutcome::Done { verdict, useless } => {
			let projected = match registry.project_verdict(
				identity,
				&verdict,
				useless,
				&native_prompt_caps(&identity.rev),
			) {
				Ok(projected) => projected,
				Err(error) => {
					tracing::error!(
						%error,
						invocation_id = %invocation_id,
						tool = %identity.name,
						"could not project the native verdict; publishing it with a harness note"
					);
					// The client checks the error flag against the outcome's own
					// branch, so it comes from the verdict, not the failure.
					let is_error = !matches!(
						serde_json::from_slice::<CallOutcome<serde::de::IgnoredAny, serde::de::IgnoredAny>>(
							&verdict
						),
						Ok(CallOutcome::Ok(_))
					);
					send_verdict_json(
						responses,
						request_id,
						invocation_id,
						delivery,
						verdict,
						vec![wire_text(UNPROJECTED_NATIVE_RESULT.to_owned())],
						is_error,
						false,
						Vec::new(),
					)
					.await;
					return;
				},
			};
			let (mut parts, mut visibility) =
				native_wire_parts(&projected.parts, delivery, invocation_id, &projected.visibility);
			let _ = project_wire_parts_with_visibility(
				&mut parts,
				&mut visibility,
				delivery.output_request,
			);
			send_verdict_json(
				responses,
				request_id,
				invocation_id,
				delivery,
				verdict,
				parts,
				projected.is_error,
				projected.useless,
				visibility,
			)
			.await;
		},
		ErasedOutcome::Detached(job) => {
			let parts = vec![wire_text(format!("detached job {}", job.id))];
			let json = serde_json::to_vec(
				&ToolTerminal::<serde_json::Value, serde_json::Value>::Detached(job),
			)
			.map(Bytes::from)
			.unwrap_or_default();
			send_verdict_json(
				responses,
				request_id,
				invocation_id,
				delivery,
				json,
				parts,
				false,
				false,
				Vec::new(),
			)
			.await;
		},
	}
}

/// Publishes one terminal verdict with its model-facing `parts`. Every
/// terminal, including aborts and policy denials, stores its serialized
/// `CallOutcome` in the environment CAS and carries the projection facts; the
/// client refuses a verdict without them as an invalid outcome artifact. Aborts
/// and policy denials carry no parts: the client renders those harness-owned
/// branches from the verdict itself.
pub(in crate::server) async fn send_verdict_json(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	invocation_id: &Str,
	delivery: &VerdictDelivery,
	json: Bytes,
	parts: Vec<thread_pb::Part>,
	is_error: bool,
	useless: bool,
	visibility: Vec<pb::ProjectionSpan>,
) {
	let details_blob = match delivery.blobs.put_verdict_bytes(
		delivery.retention_session.as_deref(),
		invocation_id,
		&json,
	) {
		Ok(reference) => reference,
		Err(error) => {
			tracing::error!(
				%error,
				invocation_id = %invocation_id,
				"could not retain the verdict before publication"
			);
			let _ = send_invocation_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::Internal,
				"verdict could not be retained",
			)
			.await;
			return;
		},
	};
	let source_bytes = u64::try_from(json.len()).unwrap_or(u64::MAX);
	let limit = match delivery.output_request {
		omp_tool::OutputRequest::Bounded => DEFAULT_RESULT_PROJECTION_BYTES,
		omp_tool::OutputRequest::Complete => COMPLETE_RESULT_PROJECTION_BYTES,
	};
	let omitted = json.len() > limit;
	let inline = if omitted { Bytes::new() } else { json };
	let inline_bytes = u64::try_from(inline.len()).unwrap_or(u64::MAX);
	let projection = output_projection(
		delivery.output_request,
		source_bytes,
		inline_bytes,
		omitted,
		Some(details_blob.clone()),
	);
	send_invocation_terminal_body(
		responses,
		request_id,
		server_frame::Body::Verdict(pb::Verdict {
			invocation_id: invocation_id.to_string(),
			json: inline,
			details_blob: Some(details_blob),
			parts,
			is_error,
			useless,
			terminate: None,
			projection: Some(projection),
			visibility,
			props: Default::default(),
		}),
	)
	.await;
}

pub(in crate::server) async fn send_abort_verdict(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	invocation_id: &Str,
	delivery: &VerdictDelivery,
	abort: omp_tool::Abort,
) {
	let verdict = CallOutcome::<serde_json::Value, serde_json::Value>::aborted(abort);
	let Ok(json) = serde_json::to_vec(&verdict) else {
		let _ = send_invocation_stream_error(
			responses,
			request_id,
			invocation_id,
			"failed to serialize invocation abort verdict",
		)
		.await;
		return;
	};
	send_verdict_json(
		responses,
		request_id,
		invocation_id,
		delivery,
		Bytes::from(json),
		Vec::new(),
		true,
		false,
		Vec::new(),
	)
	.await;
}

pub(in crate::server) async fn send_policy_denied_verdict(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	invocation_id: &Str,
	delivery: &VerdictDelivery,
	denied: policy_pb::PolicyDenied,
) {
	tracing::warn!(
		request_id,
		invocation_id = %invocation_id,
		code = %denied.code,
		rules = denied.rules.len(),
		"environment admission denied",
	);
	let reason = Str::from(denied.reason);
	let policy = omp_tool::PolicyDenied {
		reason:      reason.clone(),
		code:        (!denied.code.is_empty()).then(|| Str::from(denied.code)),
		decision_id: Str::from(denied.decision_id),
		rules:       Arc::from(
			denied
				.rules
				.into_iter()
				.map(|rule| Str::from(rule.as_str()))
				.collect::<Vec<_>>(),
		),
	};
	let verdict = CallOutcome::<serde_json::Value, serde_json::Value>::policy_denied(
		omp_tool::Abort::Skipped { reason },
		policy,
	);
	let Ok(json) = serde_json::to_vec(&verdict) else {
		let _ = send_invocation_stream_error(
			responses,
			request_id,
			invocation_id,
			"failed to serialize policy denial verdict",
		)
		.await;
		return;
	};
	send_verdict_json(
		responses,
		request_id,
		invocation_id,
		delivery,
		Bytes::from(json),
		Vec::new(),
		true,
		false,
		Vec::new(),
	)
	.await;
}

/// The effects of the call its executor will run, judged from the effective
/// arguments `raw` inside the call's `pins`; `None` when they are not a subset
/// of the envelope the call was admitted under.
pub(in crate::server) fn effective_effects(
	registry: &Registry,
	tool: &str,
	raw: &[u8],
	admitted: &Effects,
	pins: Option<&Arc<InvocationPins>>,
) -> Option<Effects> {
	let raw = str::from_utf8(raw).ok()?;
	let effects = InvocationPins::judge(pins, || registry.invocation_effects(tool, raw)).ok()?;
	effects.is_subset_of(admitted).then_some(effects)
}

/// Sends an invocation-terminal protocol error, rendering the message once at
/// this model-facing fault boundary.
pub(in crate::server) async fn send_invocation_error(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	code: pb::ProtocolErrorCode,
	message: impl std::fmt::Display,
) -> bool {
	let body = server_frame::Body::Error(pb::ProtocolError {
		code:    code as i32,
		message: message.to_string(),
		props:   Default::default(),
	});
	send_invocation_terminal_body(responses, request_id, body).await;
	true
}

pub(in crate::server) async fn send_invocation_stream_error(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	invocation_id: &str,
	message: &str,
) -> bool {
	let body = server_frame::Body::EventStreamError(pb::EventStreamError {
		stream:         pb::EventStreamKind::Invocation as i32,
		failure:        pb::EventStreamFailure::Closed as i32,
		invocation_id:  invocation_id.to_owned(),
		exec:           Bytes::new(),
		process_name:   String::new(),
		skipped_events: 0,
		message:        message.to_owned(),
		props:          Default::default(),
	});
	send_invocation_terminal_body(responses, request_id, body).await;
	true
}

pub(in crate::server) fn spawn_exec(
	request_id: u64,
	run: ExecRun,
	cancel: CancellationToken,
	responses: flume::Sender<pb::ServerFrame>,
	finished: flume::Sender<Finished>,
) {
	tokio::spawn(async move {
		let exec = Bytes::copy_from_slice(run.id());
		let mut terminal = false;
		loop {
			let event = tokio::select! {
				() = cancel.cancelled() => break,
				event = run.next_event() => event,
			};
			match event {
				Some(ExecEvent::Started { .. }) => {},
				Some(ExecEvent::Output(output)) => {
					send_body(&responses, request_id, server_frame::Body::Output(output)).await;
				},
				Some(ExecEvent::Exit(exit)) => {
					terminal = true;
					send_body(&responses, request_id, server_frame::Body::Exit(*exit)).await;
					break;
				},
				None => break,
			}
		}
		if !terminal && !cancel.is_cancelled() {
			send_stream_error(
				&responses,
				request_id,
				pb::EventStreamKind::Exec,
				"",
				&exec,
				"",
				"exec event stream closed without ExitEvent",
			)
			.await;
		}
		let _ = finished
			.send_async(Finished { request_id, invocation_id: None })
			.await;
	});
}

pub(in crate::server) fn spawn_process_attachment(
	request_id: u64,
	process_name: Str,
	events: Receiver<ProcessEvent>,
	cancel: CancellationToken,
	responses: flume::Sender<pb::ServerFrame>,
	finished: flume::Sender<Finished>,
) {
	tokio::spawn(async move {
		loop {
			let event = tokio::select! {
				() = cancel.cancelled() => break,
				event = events.recv_async() => event.ok(),
			};
			match event {
				Some(ProcessEvent::Output(output)) => {
					send_body(&responses, request_id, server_frame::Body::ProcessOutput(output)).await;
				},
				Some(ProcessEvent::State(process)) => {
					send_body(
						&responses,
						request_id,
						server_frame::Body::ProcessState(pb::ProcessStateEvent {
							process: Some(process),
							props:   Default::default(),
						}),
					)
					.await;
				},
				None => {
					send_stream_error(
						&responses,
						request_id,
						pb::EventStreamKind::ProcessOutput,
						"",
						&[],
						&process_name,
						"named-process output stream closed",
					)
					.await;
					break;
				},
			}
		}
		let _ = finished
			.send_async(Finished { request_id, invocation_id: None })
			.await;
	});
}

pub(in crate::server) struct WorkspaceSearchOwned {
	pub(in crate::server) pattern: Str,
	pub(in crate::server) case:    WorkspaceSearchCase,
	pub(in crate::server) limit:   Option<u64>,
}

pub(in crate::server) async fn parse_mounted_resource_uri<'a>(
	input: &'a str,
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
) -> Option<ParsedUri<'a>> {
	if input.len() > MAX_RESOURCE_URI_BYTES {
		send_error(
			responses,
			request_id,
			pb::ProtocolErrorCode::InvalidArgument,
			"resource URI exceeds the 8192-byte limit",
		)
		.await;
		return None;
	}
	match selector::parse_uri(input) {
		Ok(Some(uri))
			if !matches!(
				uri.scheme,
				omp_tools::read::resolver::Scheme::Unknown
					| omp_tools::read::resolver::Scheme::File
					| omp_tools::read::resolver::Scheme::Http
			) =>
		{
			Some(uri)
		},
		Ok(Some(_)) => {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::Unsupported,
				"resource URI scheme is not mounted on the internal resource plane",
			)
			.await;
			None
		},
		Ok(None) => {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::InvalidArgument,
				"resource URI must use hierarchical scheme:// syntax",
			)
			.await;
			None
		},
		Err(error) => {
			send_error(responses, request_id, pb::ProtocolErrorCode::InvalidArgument, &error).await;
			None
		},
	}
}

pub(in crate::server) fn resource_bound(
	value: u64,
	ceiling: usize,
	name: &'static str,
) -> Result<usize, &'static str> {
	let Ok(value) = usize::try_from(value) else {
		return Err("resource operation bound does not fit this host");
	};
	if value == 0 {
		return Err(match name {
			"resource read max_bytes" => "resource read max_bytes must be nonzero",
			"resource list max_entries" => "resource list max_entries must be nonzero",
			"resource list max_bytes" => "resource list max_bytes must be nonzero",
			"resource completion max_results" => "resource completion max_results must be nonzero",
			_ => "resource operation bound must be nonzero",
		});
	}
	if value > ceiling {
		return Err(match name {
			"resource read max_bytes" => "resource read max_bytes exceeds the 8 MiB ceiling",
			"resource list max_entries" => "resource list max_entries exceeds the 4096-entry ceiling",
			"resource list max_bytes" => "resource list max_bytes exceeds the 2 MiB ceiling",
			"resource completion max_results" => {
				"resource completion max_results exceeds the 100-result ceiling"
			},
			_ => "resource operation bound exceeds its Environment ceiling",
		});
	}
	Ok(value)
}

pub(in crate::server) fn resource_capability_wire(
	capability: ResourceCapability,
) -> pb::ResourceCapability {
	pb::ResourceCapability {
		scheme:      capability.scheme.to_owned(),
		read:        capability.read,
		list:        capability.list,
		path:        capability.path,
		complete:    capability.complete,
		device_hash: Bytes::copy_from_slice(&capability.stamp.device_hash),
		revision:    capability.stamp.revision,
	}
}

pub(in crate::server) async fn send_resource_result(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	result: pb::ResourceResult,
) {
	send_data_response(
		responses,
		request_id,
		data_response::Body::Resource(pb::ResourceOpResult { result: Some(result) }),
	)
	.await;
}

pub(in crate::server) async fn send_resource_fault(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	fault: &read::Fault,
) {
	let code = match fault {
		read::Fault::Invalid { .. } => pb::ProtocolErrorCode::InvalidArgument,
		read::Fault::UnknownScheme { .. }
		| read::Fault::SchemeNotReadable { .. }
		| read::Fault::Unsupported { .. } => pb::ProtocolErrorCode::Unsupported,
		read::Fault::Source { .. } => pb::ProtocolErrorCode::NotFound,
		read::Fault::Web { .. } | read::Fault::Blob { .. } => pb::ProtocolErrorCode::Internal,
	};
	send_error(responses, request_id, code, fault.message()).await;
}

pub(in crate::server) async fn send_resource_capability_error(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	operation: &str,
) {
	send_error(
		responses,
		request_id,
		pb::ProtocolErrorCode::Unsupported,
		&format!("mounted resource does not support {operation}"),
	)
	.await;
}

pub(in crate::server) const fn mcp_operation(request: &pb::McpOp) -> &'static str {
	use mcp_op::Op;

	match request.op.as_ref() {
		Some(Op::Status(_)) => "omp.env.mcp.status",
		Some(Op::Subscribe(_)) => "omp.env.mcp.subscribe",
		Some(Op::Reset(_)) => "omp.env.mcp.reset",
		Some(Op::LiveHeader(_)) => "omp.env.mcp.live_header",
		Some(Op::Resource(_)) => "omp.env.mcp.resource",
		Some(Op::Prompt(_)) => "omp.env.mcp.prompt",
		Some(Op::Invoke(_)) => "omp.env.mcp.invoke",
		Some(Op::Config(_)) => "omp.env.mcp.config",
		None => "omp.env.mcp.invalid",
	}
}

pub(in crate::server) const fn mcp_wire_revision(operation: &mcp_op::Op) -> u32 {
	use mcp_op::Op;

	match operation {
		Op::Status(request) => request.wire_revision,
		Op::Subscribe(request) => request.wire_revision,
		Op::Reset(request) => request.wire_revision,
		Op::LiveHeader(request) => request.wire_revision,
		Op::Resource(request) => request.wire_revision,
		Op::Prompt(request) => request.wire_revision,
		Op::Invoke(request) => request.wire_revision,
		Op::Config(request) => request.wire_revision,
	}
}

pub(in crate::server) fn spawn_mcp_request(
	request_id: u64,
	service: Arc<McpService>,
	operation: mcp_op::Op,
	cancel: CancellationToken,
	responses: flume::Sender<pb::ServerFrame>,
	finished: flume::Sender<Finished>,
) {
	tokio::spawn(async move {
		use pb::{mcp_op::Op, mcp_result::Result as McpResult};
		let result = match operation {
			Op::Reset(request) => service
				.reset(request, cancel.clone())
				.await
				.map(McpResult::Reset),
			Op::LiveHeader(request) => service
				.live_header(request, cancel.clone())
				.await
				.map(McpResult::LiveHeader),
			Op::Resource(request) => service
				.resource(request, cancel.clone())
				.await
				.map(McpResult::Resource),
			Op::Prompt(request) => service
				.prompt(request, cancel.clone())
				.await
				.map(McpResult::Prompt),
			Op::Invoke(request) => service
				.invoke(request, cancel.clone())
				.await
				.map(McpResult::Invoke),
			Op::Config(request) => service.config(request).await.map(McpResult::Config),
			Op::Status(_) | Op::Subscribe(_) => Err(McpServiceError::InvalidRequest),
		};
		if !cancel.is_cancelled() {
			match result {
				Ok(result) => {
					send_data_response(
						&responses,
						request_id,
						data_response::Body::Mcp(pb::McpResult { result: Some(result) }),
					)
					.await;
				},
				Err(error) => send_mcp_error(&responses, request_id, &error).await,
			}
		}
		let _ = finished
			.send_async(Finished { request_id, invocation_id: None })
			.await;
	});
}

pub(in crate::server) fn spawn_mcp_subscription(
	request_id: u64,
	subscription: ServiceSubscription,
	cancel: CancellationToken,
	responses: flume::Sender<pb::ServerFrame>,
	finished: flume::Sender<Finished>,
) {
	tokio::spawn(async move {
		loop {
			match subscription.next(&cancel).await {
				Ok(Some(SubscriptionEvent::Notification(notification))) => {
					if !send_data_event_sync(
						&responses,
						request_id,
						data_event::Body::McpNotification(notification),
					) {
						break;
					}
				},
				Ok(Some(SubscriptionEvent::Status(status))) => {
					if !send_data_event_sync(&responses, request_id, data_event::Body::McpStatus(status))
					{
						break;
					}
				},
				Err(McpServiceError::Cancelled) => break,
				Ok(None) | Err(_) => {
					let _ = responses
						.send_async(server_frame(
							request_id,
							server_frame::Body::EventStreamError(pb::EventStreamError {
								stream:         pb::EventStreamKind::McpNotification.into(),
								failure:        pb::EventStreamFailure::Synchronization.into(),
								invocation_id:  String::new(),
								exec:           Bytes::new(),
								process_name:   String::new(),
								skipped_events: 0,
								message:        "MCP notification continuity was lost".to_owned(),
								props:          Default::default(),
							}),
						))
						.await;
					break;
				},
			}
		}
		let _ = finished
			.send_async(Finished { request_id, invocation_id: None })
			.await;
	});
}

pub(in crate::server) async fn send_mcp_error(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	error: &McpServiceError,
) {
	use crate::mcp::McpServiceError;
	let code = match error {
		McpServiceError::InvalidRequest => pb::ProtocolErrorCode::InvalidArgument,
		McpServiceError::Config(error) => match error {
			crate::mcp::config_store::ConfigStoreError::Io { .. }
			| crate::mcp::config_store::ConfigStoreError::NoParent { .. }
			| crate::mcp::config_store::ConfigStoreError::LockTimeout { .. } => {
				pb::ProtocolErrorCode::Internal
			},
			crate::mcp::config_store::ConfigStoreError::Json { .. }
			| crate::mcp::config_store::ConfigStoreError::Validation { .. }
			| crate::mcp::config_store::ConfigStoreError::AlreadyExists { .. }
			| crate::mcp::config_store::ConfigStoreError::NotFound { .. } => {
				pb::ProtocolErrorCode::InvalidArgument
			},
		},
		McpServiceError::ServerNotFound => pb::ProtocolErrorCode::NotFound,
		McpServiceError::StaleDefinitionEpoch { .. }
		| McpServiceError::StaleGeneration
		| McpServiceError::StaleSequence
		| McpServiceError::LeafReplacement(_) => pb::ProtocolErrorCode::PreconditionFailed,
		McpServiceError::ContinuityLost => pb::ProtocolErrorCode::PreconditionFailed,
		McpServiceError::Cancelled => pb::ProtocolErrorCode::Cancelled,
		McpServiceError::EpochExhausted | McpServiceError::Backend => pb::ProtocolErrorCode::Internal,
	};
	send_error(responses, request_id, code, &error.to_string()).await;
}

pub(in crate::server) fn spawn_resource_completion(
	request_id: u64,
	input: String,
	max_results: usize,
	resources: Arc<ResolverTable<UrlResolver>>,
	cancel: CancellationToken,
	responses: flume::Sender<pb::ServerFrame>,
	finished: flume::Sender<Finished>,
) {
	tokio::spawn(async move {
		use omp_proto::env::v1::{ResourceCapability, ResourceCompletion};
		let result = tokio::select! {
			() = cancel.cancelled() => None,
			result = resource_completions(&resources, &input, max_results) => Some(result),
		};
		if let Some(result) = result {
			match result {
				Ok((completions, truncated)) => {
					let mut emitted = 0u32;
					for (completion, capability) in completions {
						if cancel.is_cancelled() {
							break;
						}
						let event = checked_server_frame(
							request_id,
							server_frame::Body::DataEvent(pb::DataEvent {
								body:  Some(data_event::Body::ResourceCompletion(ResourceCompletion {
									value:       completion.value.to_string(),
									description: completion.description.to_string(),
									capability:  Some(ResourceCapability {
										scheme:      capability.scheme.to_owned(),
										read:        capability.read,
										list:        capability.list,
										path:        capability.path,
										complete:    capability.complete,
										device_hash: Bytes::copy_from_slice(&capability.stamp.device_hash),
										revision:    capability.stamp.revision,
									}),
									score:       completion.score,
								})),
								props: Default::default(),
							}),
						);
						if responses.send_async(event).await.is_err() {
							break;
						}
						emitted = emitted.saturating_add(1);
					}
					if !cancel.is_cancelled() {
						let terminal = checked_server_frame(
							request_id,
							server_frame::Body::DataEvent(pb::DataEvent {
								body:  Some(data_event::Body::ResourceCompletionComplete(
									pb::ResourceCompletionComplete {
										emitted,
										truncated,
										catalog_revision: resources.revision(),
									},
								)),
								props: Default::default(),
							}),
						);
						let _ = responses.send_async(terminal).await;
					}
				},
				Err(message) => {
					let _ = responses
						.send_async(server_frame(
							request_id,
							server_frame::Body::EventStreamError(pb::EventStreamError {
								stream: pb::EventStreamKind::ResourceCompletion.into(),
								failure: pb::EventStreamFailure::Closed.into(),
								invocation_id: String::new(),
								exec: Bytes::new(),
								process_name: String::new(),
								skipped_events: 0,
								message,
								props: Default::default(),
							}),
						))
						.await;
				},
			}
		}
		let _ = finished
			.send_async(Finished { request_id, invocation_id: None })
			.await;
	});
}

pub(in crate::server) async fn resource_completions(
	resources: &ResolverTable<UrlResolver>,
	input: &str,
	max_results: usize,
) -> Result<(Vec<(ResourceCompletion, ResourceCapability)>, bool), String> {
	use omp_tools::read::resolver::fuzzy_score;

	if let Some((raw_scheme, query)) = input.split_once("://") {
		let scheme = resolver::Scheme::parse(raw_scheme);
		if scheme == resolver::Scheme::Unknown {
			return Err(format!("resource completion scheme is not mounted: {raw_scheme}"));
		}
		let capability = resources
			.capability(scheme)
			.filter(|capability| capability.complete)
			.ok_or_else(|| format!("{raw_scheme}:// does not support completion"))?;
		let (matches, truncated) = resources
			.complete(scheme, query, max_results)
			.await
			.ok_or_else(|| format!("{raw_scheme}:// does not support completion"))?
			.map_err(|fault| fault.message().to_string())?;
		return Ok((
			matches
				.into_iter()
				.map(|completion| (completion, capability.clone()))
				.collect(),
			truncated,
		));
	}

	let mut matches = resources
		.capabilities()
		.filter_map(|capability| {
			let score = fuzzy_score(input.trim_end_matches(':'), capability.scheme)?;
			Some((
				ResourceCompletion {
					value: Str::new(format!("{}://", capability.scheme)),
					description: capability.description.clone(),
					score,
				},
				capability,
			))
		})
		.collect::<Vec<_>>();
	matches.sort_unstable_by(|(left, _), (right, _)| {
		right
			.score
			.cmp(&left.score)
			.then_with(|| left.value.cmp(&right.value))
	});
	let truncated = matches.len() > max_results;
	matches.truncate(max_results);
	Ok((matches, truncated))
}

pub(in crate::server) fn spawn_document_events(
	request_id: u64,
	events: DocumentEvents,
	cancel: CancellationToken,
	responses: flume::Sender<pb::ServerFrame>,
	finished: flume::Sender<Finished>,
) {
	tokio::spawn(async move {
		loop {
			let event = tokio::select! {
				() = cancel.cancelled() => break,
				event = events.next_event() => event,
			};
			match event {
				Ok(event) => {
					if responses
						.send_async(server_frame(
							request_id,
							server_frame::Body::DataEvent(pb::DataEvent {
								body:  Some(data_event::Body::Document(event)),
								props: Default::default(),
							}),
						))
						.await
						.is_err()
					{
						break;
					}
				},
				Err(error) => {
					let _ = responses
						.send_async(server_frame(
							request_id,
							server_frame::Body::EventStreamError(pb::EventStreamError {
								stream:         pb::EventStreamKind::Document.into(),
								failure:        error.failure.into(),
								invocation_id:  String::new(),
								exec:           Bytes::new(),
								process_name:   String::new(),
								skipped_events: error.skipped_events,
								message:        error.message.to_string(),
								props:          Default::default(),
							}),
						))
						.await;
					break;
				},
			}
		}
		let _ = finished
			.send_async(Finished { request_id, invocation_id: None })
			.await;
	});
}

pub(in crate::server) fn spawn_lsp_events(
	request_id: u64,
	events: LspEvents,
	cancel: CancellationToken,
	responses: flume::Sender<pb::ServerFrame>,
	finished: flume::Sender<Finished>,
) {
	tokio::spawn(async move {
		loop {
			let event = tokio::select! {
				() = cancel.cancelled() => break,
				event = events.next_event() => event,
			};
			match event {
				Ok(event) => {
					let body = match event {
						LspRegistryEvent::Event(event) => data_event::Body::Lsp(event),
						LspRegistryEvent::Binding(event) => data_event::Body::LspBinding(event),
					};
					if responses
						.send_async(server_frame(
							request_id,
							server_frame::Body::DataEvent(pb::DataEvent {
								body:  Some(body),
								props: Default::default(),
							}),
						))
						.await
						.is_err()
					{
						break;
					}
				},
				Err(error) => {
					let _ = responses
						.send_async(server_frame(
							request_id,
							server_frame::Body::EventStreamError(pb::EventStreamError {
								stream:         pb::EventStreamKind::LspRegistry.into(),
								failure:        error.failure.into(),
								invocation_id:  String::new(),
								exec:           Bytes::new(),
								process_name:   String::new(),
								skipped_events: error.skipped_events,
								message:        error.message.to_string(),
								props:          Default::default(),
							}),
						))
						.await;
					break;
				},
			}
		}
		let _ = finished
			.send_async(Finished { request_id, invocation_id: None })
			.await;
	});
}

pub(in crate::server) fn spawn_workspace_walk(
	request_id: u64,
	workspace: WorkspaceHost,
	request: WalkRequest,
	cancel: CancellationToken,
	responses: flume::Sender<pb::ServerFrame>,
	finished: flume::Sender<Finished>,
) {
	task::spawn_blocking(move || {
		let mut emitted = 0_u64;
		let result = workspace.walk_stream(&request, &cancel, |entry| {
			if cancel.is_cancelled() {
				return ControlFlow::Break(());
			}
			let kind = match entry.file_type {
				FileType::File => document_pb::FileKind::RegularFile,
				FileType::Dir => document_pb::FileKind::Directory,
				FileType::Symlink => document_pb::FileKind::SymbolicLink,
			};
			let event = data_event::Body::WalkEntry(pb::WalkEntry {
				path:     entry.relative_path.to_owned(),
				kind:     kind as i32,
				mtime_ms: entry.mtime,
				size:     entry.size,
				depth:    u64::try_from(entry.depth).unwrap_or(u64::MAX),
				props:    Default::default(),
			});
			if send_data_event_sync(&responses, request_id, event) {
				emitted += 1;
				ControlFlow::Continue(())
			} else {
				ControlFlow::Break(())
			}
		});
		match result {
			Ok(status) if !cancel.is_cancelled() => {
				let _ = send_data_event_sync(
					&responses,
					request_id,
					data_event::Body::WalkComplete(pb::WalkComplete {
						scanned_entries:  emitted,
						filtered_entries: 0,
						limited_entries:  u64::from(status == WalkStatus::Stopped),
						cache_age_ms:     0,
						cached:           false,
						props:            Default::default(),
					}),
				);
			},
			Ok(_) => {},
			Err(error) => {
				send_workspace_stream_error_sync(
					&responses,
					request_id,
					pb::EventStreamKind::Walk,
					&error,
				);
			},
		}
		let _ = finished.send(Finished { request_id, invocation_id: None });
	});
}

pub(in crate::server) fn spawn_workspace_search(
	request_id: u64,
	workspace: WorkspaceHost,
	request: WalkRequest,
	options: WorkspaceSearchOwned,
	cancel: CancellationToken,
	responses: flume::Sender<pb::ServerFrame>,
	finished: flume::Sender<Finished>,
) {
	task::spawn_blocking(move || {
		let borrowed = WorkspaceSearchOptions {
			pattern: options.pattern.as_str(),
			case:    options.case,
			limit:   options.limit,
		};
		let result = workspace.search_stream(&request, &borrowed, &cancel, |matched| {
			if cancel.is_cancelled() {
				return ControlFlow::Break(());
			}
			if send_data_event_sync(
				&responses,
				request_id,
				data_event::Body::SearchMatch(pb::SearchMatchMsg {
					path:        matched.path.to_string(),
					line:        matched.line,
					byte_offset: matched.byte_offset,
					line_bytes:  matched.line_bytes,
					props:       Default::default(),
				}),
			) {
				ControlFlow::Continue(())
			} else {
				ControlFlow::Break(())
			}
		});
		match result {
			Ok(outcome) if !cancel.is_cancelled() => {
				let _ = send_data_event_sync(
					&responses,
					request_id,
					data_event::Body::SearchComplete(pb::SearchComplete {
						files_scanned: outcome.files_scanned,
						matches:       outcome.matches,
						limited:       outcome.limited,
						props:         Default::default(),
					}),
				);
			},
			Ok(_) => {},
			Err(error) => {
				send_workspace_stream_error_sync(
					&responses,
					request_id,
					pb::EventStreamKind::Search,
					&error,
				);
			},
		}
		let _ = finished.send(Finished { request_id, invocation_id: None });
	});
}

pub(in crate::server) fn send_data_event_sync(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	body: data_event::Body,
) -> bool {
	responses
		.send(checked_server_frame(
			request_id,
			server_frame::Body::DataEvent(pb::DataEvent {
				body:  Some(body),
				props: Default::default(),
			}),
		))
		.is_ok()
}

pub(in crate::server) fn send_workspace_stream_error_sync(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	kind: pb::EventStreamKind,
	error: &WorkspaceError,
) {
	let _ = responses.send(checked_server_frame(
		request_id,
		server_frame::Body::EventStreamError(pb::EventStreamError {
			stream:         kind as i32,
			failure:        pb::EventStreamFailure::Closed as i32,
			invocation_id:  String::new(),
			exec:           Bytes::new(),
			process_name:   String::new(),
			skipped_events: 0,
			message:        error.to_string(),
			props:          Default::default(),
		}),
	));
}

#[derive(Clone, Debug)]
pub(in crate::server) struct VerdictDeliveryProvenance {
	pub(in crate::server) session_id:    Str,
	pub(in crate::server) invocation_id: Str,
}

pub(in crate::server) fn verdict_delivery_provenance(
	scope: &pb::InvocationScope,
) -> Option<VerdictDeliveryProvenance> {
	(!scope.session_id.is_empty() && !scope.agent_id.is_empty() && !scope.invocation_id.is_empty())
		.then(|| VerdictDeliveryProvenance {
			session_id:    Str::from(scope.session_id.as_str()),
			invocation_id: Str::from(scope.invocation_id.as_str()),
		})
}

pub(in crate::server) fn spawn_blob_get(
	request_id: u64,
	read: BlobRead,
	delivery: Option<VerdictDeliveryProvenance>,
	cancel: CancellationToken,
	responses: flume::Sender<pb::ServerFrame>,
	finished: flume::Sender<Finished>,
	blobs: BlobHost,
) {
	tokio::spawn(async move {
		let id = read.id();
		let range_offset = read.offset();
		let range_length = read.len();
		let mut file = tokio::fs::File::from_std(read.into_file());
		let mut sent = 0_u64;
		let mut complete = true;

		if range_length == 0 {
			let send = responses.send_async(checked_server_frame(
				request_id,
				server_frame::Body::BlobChunk(blob_pb::Chunk {
					data: Bytes::new(),
					hash: Bytes::copy_from_slice(&id.hash),
					size: Some(id.size),
				}),
			));
			tokio::select! {
				() = cancel.cancelled() => {},
				result = send => complete = result.is_ok(),
			}
		}

		let mut buffer = vec![0_u8; BLOB_CHUNK_BYTES].into_boxed_slice();
		while sent < range_length {
			let remaining = range_length - sent;
			let wanted = usize::try_from(remaining.min(BLOB_CHUNK_BYTES as u64))
				.expect("blob chunk bound fits usize");
			let read_result = tokio::select! {
				() = cancel.cancelled() => break,
				result = file.read(&mut buffer[..wanted]) => result,
			};
			let count = match read_result {
				Ok(0) => {
					let error = BlobError::ReadTruncated { expected: range_length, actual: sent };
					send_blob_error(&responses, request_id, &error).await;
					complete = false;
					break;
				},
				Ok(count) => count,
				Err(source) => {
					let error = BlobError::Store(source.into());
					send_blob_error(&responses, request_id, &error).await;
					complete = false;
					break;
				},
			};
			let first = sent == 0;
			let send = responses.send_async(checked_server_frame(
				request_id,
				server_frame::Body::BlobChunk(blob_pb::Chunk {
					data: Bytes::copy_from_slice(&buffer[..count]),
					hash: if first {
						Bytes::copy_from_slice(&id.hash)
					} else {
						Bytes::new()
					},
					size: first.then_some(id.size),
				}),
			));
			tokio::select! {
				() = cancel.cancelled() => break,
				result = send => {
					if result.is_err() {
						complete = false;
						break;
					}
					sent += u64::try_from(count).expect("blob chunk count fits u64");
				},
			}
		}

		if complete && !cancel.is_cancelled() && sent == range_length {
			let completion_delivered = responses
				.send_async(checked_server_frame(
					request_id,
					server_frame::Body::BlobGetComplete(pb::BlobGetComplete {
						hash:       Bytes::copy_from_slice(&id.hash),
						bytes_sent: sent,
						props:      Default::default(),
					}),
				))
				.await
				.is_ok();
			if completion_delivered
				&& range_offset.saturating_add(sent) == id.size
				&& let Some(delivery) = delivery.as_ref()
			{
				match blobs.verdict_downloaded(
					Some(delivery.session_id.as_str()),
					delivery.invocation_id.as_str(),
					id,
				) {
					Ok(_) => {},
					Err(error) => tracing::warn!(
						%error,
						hash = %Hash32::new(id.hash),
						session_id = %delivery.session_id,
						invocation_id = %delivery.invocation_id,
						"could not release completed verdict download lease"
					),
				}
			}
		}
		let _ = finished
			.send_async(Finished { request_id, invocation_id: None })
			.await;
	});
}

pub(in crate::server) fn materialize_worker_outcome(
	blobs: &BlobHost,
	complete: &ExtHostCompletion,
) -> Result<Bytes, Str> {
	let Some(blob) = complete.details_blob.as_ref() else {
		return worker_completion_json(complete).map(|(json, ..)| json);
	};
	if blob.size > u64::try_from(DEFAULT_MAX_FRAME_BYTES).expect("frame bound fits u64") {
		return Err(sf!(
			"dynamic worker result exceeds the bounded inline projection; read its artifact instead"
		));
	}
	let hash: [u8; 32] = blob
		.hash
		.as_ref()
		.try_into()
		.map_err(|_| sf!("worker result blob hash is invalid"))?;
	let bytes = blobs
		.get(BlobId { hash, size: blob.size })
		.map_err(|_| sf!("worker result blob is unavailable"))?;
	if serde_json::from_slice::<CallOutcome<serde_json::Value, serde_json::Value>>(&bytes).is_ok() {
		return Ok(bytes);
	}
	match complete.kind {
		ExtHostOutcomeKind::Ok | ExtHostOutcomeKind::Faulted => {
			worker_verdict_json(bytes, complete.kind == ExtHostOutcomeKind::Faulted)
				.map_err(|_| sf!("worker result blob is invalid"))
		},
		ExtHostOutcomeKind::ArgsRejected | ExtHostOutcomeKind::Aborted => {
			Err(sf!("worker result blob omitted its terminal outcome envelope"))
		},
	}
}

pub(in crate::server) fn materialize_worker_details(
	blobs: &BlobHost,
	complete: &ExtHostCompletion,
) -> Result<Bytes, Str> {
	if complete.details_blob.is_none() {
		return complete
			.details_json
			.clone()
			.ok_or_else(|| sf!("worker completion omitted structured details"));
	}
	let outcome = materialize_worker_outcome(blobs, complete)?;
	let mut outcome = serde_json::from_slice::<serde_json::Value>(&outcome)
		.map_err(|_| sf!("worker result artifact is invalid"))?;
	let value = outcome
		.as_object_mut()
		.and_then(|outcome| outcome.remove("value"))
		.ok_or_else(|| sf!("worker result artifact omitted its value"))?;
	serde_json::to_vec(&value)
		.map(Bytes::from)
		.map_err(|_| sf!("worker result value could not be encoded"))
}

pub(in crate::server) fn materialize_worker_completion(
	blobs: &BlobHost,
	complete: &ExtHostCompletion,
) -> Result<Bytes, Str> {
	materialize_worker_outcome(blobs, complete)
}

pub(in crate::server) fn projected_worker_completion_json(
	blobs: &BlobHost,
	complete: &ExtHostCompletion,
	request: omp_tool::OutputRequest,
	retention_session: Option<&str>,
	invocation_id: &str,
) -> Result<(Bytes, Option<thread_pb::Blob>, bool), Str> {
	let (mut json, mut details_blob, is_error) = worker_completion_json(complete)?;
	let inline_limit = match request {
		omp_tool::OutputRequest::Bounded => DEFAULT_RESULT_PROJECTION_BYTES,
		omp_tool::OutputRequest::Complete => COMPLETE_RESULT_PROJECTION_BYTES,
	};
	if json.is_empty()
		&& details_blob
			.as_ref()
			.is_some_and(|details| details.size <= u64::try_from(inline_limit).unwrap_or(u64::MAX))
	{
		json = materialize_worker_outcome(blobs, complete)?;
	}
	// A worker small enough to answer inline sends no artifact of its own, but
	// the verdict is still canonical content the caller may fetch by address,
	// so it is retained here exactly as the native path retains its own. The
	// store is content-addressed, so an identical verdict costs no new bytes.
	// Size does not decide: only a verdict with no body has nothing to retain.
	if details_blob.is_none() && !json.is_empty() {
		details_blob = Some(
			blobs
				.put_verdict_bytes(retention_session, invocation_id, &json)
				.map_err(|error| {
					tracing::error!(
						%error,
						invocation_id = %invocation_id,
						"could not retain worker verdict before publication"
					);
					sf!("worker verdict could not be retained")
				})?,
		);
	}
	if json.len() > inline_limit {
		json = Bytes::new();
	}
	Ok((json, details_blob, is_error))
}

pub(in crate::server) fn worker_completion_json(
	complete: &ExtHostCompletion,
) -> Result<(Bytes, Option<thread_pb::Blob>, bool), Str> {
	let is_error = complete.kind != ExtHostOutcomeKind::Ok;
	if let Some(blob) = &complete.details_blob {
		return Ok((Bytes::new(), Some(blob.clone()), is_error));
	}
	let details = complete
		.details_json
		.clone()
		.ok_or_else(|| sf!("worker completion omitted structured details"))?;
	let json = match complete.kind {
		ExtHostOutcomeKind::Ok | ExtHostOutcomeKind::Faulted => {
			worker_verdict_json(details, is_error).map_err(fault)?
		},
		ExtHostOutcomeKind::ArgsRejected => {
			let issue = complete
				.args_issue
				.as_ref()
				.ok_or_else(|| sf!("worker omitted its argument issue"))?;
			let kind = issue
				.kind
				.parse()
				.map_err(|_| sf!("worker argument issue kind is invalid"))?;
			let issue = ArgIssue {
				path: issue
					.path
					.iter()
					.map(|segment| ArgPath::Key(Str::from(segment.as_str())))
					.collect(),
				expected: Str::from(issue.expected.as_str()),
				kind,
				example: issue.example.as_deref().map(Str::from),
				found: issue.found.as_deref().map(Str::from),
			};
			Bytes::from(
				serde_json::to_vec(&CallOutcome::<serde_json::Value, serde_json::Value>::ArgsRejected(
					issue,
				))
				.map_err(fault)?,
			)
		},
		ExtHostOutcomeKind::Aborted => {
			let abort: Abort = serde_json::from_slice(&details).map_err(fault)?;
			Bytes::from(
				serde_json::to_vec(&CallOutcome::<serde_json::Value, serde_json::Value>::aborted(
					abort,
				))
				.map_err(fault)?,
			)
		},
	};
	Ok((json, None, is_error))
}

pub(in crate::server) async fn send_workspace_operation_error(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	error: &WorkspaceOperationError,
) {
	match error {
		WorkspaceOperationError::Document(error) => {
			send_document_error(responses, request_id, error).await;
		},
		WorkspaceOperationError::Blob(error) => {
			send_blob_error(responses, request_id, error).await;
		},
		WorkspaceOperationError::WorktreeNotFound { name: _ } => {
			send_error(responses, request_id, pb::ProtocolErrorCode::NotFound, &error.to_string())
				.await;
		},
		WorkspaceOperationError::OutsideRoot
		| WorkspaceOperationError::WireRevision
		| WorkspaceOperationError::InvalidGeneration(_)
		| WorkspaceOperationError::IsolationBaselineTooLarge(_)
		| WorkspaceOperationError::NotARepository
		| WorkspaceOperationError::DenyGlob { .. }
		| WorkspaceOperationError::InvalidWorktreeName => {
			send_error(
				responses,
				request_id,
				pb::ProtocolErrorCode::InvalidArgument,
				&error.to_string(),
			)
			.await;
		},
		WorkspaceOperationError::Workspace(_)
		| WorkspaceOperationError::Io(_)
		| WorkspaceOperationError::IsolationPreflight(_)
		| WorkspaceOperationError::InvalidSnapshotRecord(_)
		| WorkspaceOperationError::InputPaths { .. }
		| WorkspaceOperationError::DenyGlobSet(_)
		| WorkspaceOperationError::CredentialRules(_)
		| WorkspaceOperationError::InvalidWorktreeRecord(_) => {
			send_error(responses, request_id, pb::ProtocolErrorCode::Internal, &error.to_string())
				.await;
		},
	}
}
pub(in crate::server) fn workspace_walk_request(
	workspace: &WorkspaceHost,
	request: &pb::WalkRequest,
) -> Result<WalkRequest, (pb::ProtocolErrorCode, String)> {
	let root = if request.root_uri.is_empty() {
		workspace.root().to_path_buf()
	} else {
		Url::parse(&request.root_uri)
			.map_err(|error| {
				(
					pb::ProtocolErrorCode::InvalidArgument,
					fault_in("walk root is not a valid URI", error).to_string(),
				)
			})?
			.to_file_path()
			.map_err(|()| {
				(pb::ProtocolErrorCode::InvalidArgument, "walk root is not a local file URI".to_owned())
			})?
	};
	if !request.exclude.is_empty() {
		return Err((
			pb::ProtocolErrorCode::Unsupported,
			"walk exclude globs are not implemented".to_owned(),
		));
	}
	let options = request.options.as_ref();
	let follow_links = match options
		.map(|options| pb::WalkFollowLinks::try_from(options.follow_links))
		.transpose()
		.map_err(|_| {
			(pb::ProtocolErrorCode::InvalidArgument, "walk follow_links value is invalid".to_owned())
		})?
		.unwrap_or(pb::WalkFollowLinks::Never)
	{
		pb::WalkFollowLinks::Unspecified | pb::WalkFollowLinks::Never => FollowLinks::Never,
		pb::WalkFollowLinks::Roots => FollowLinks::Roots,
		pb::WalkFollowLinks::Always => FollowLinks::Always,
	};
	let detail = match options
		.map(|options| pb::WalkDetail::try_from(options.detail))
		.transpose()
		.map_err(|_| {
			(pb::ProtocolErrorCode::InvalidArgument, "walk detail value is invalid".to_owned())
		})?
		.unwrap_or(pb::WalkDetail::Minimal)
	{
		pb::WalkDetail::Unspecified | pb::WalkDetail::Minimal => WalkDetail::Minimal,
		pb::WalkDetail::Full => WalkDetail::Full,
	};
	let order = match options
		.map(|options| pb::WalkOrder::try_from(options.order))
		.transpose()
		.map_err(|_| {
			(pb::ProtocolErrorCode::InvalidArgument, "walk order value is invalid".to_owned())
		})?
		.unwrap_or(pb::WalkOrder::Path)
	{
		pb::WalkOrder::Unspecified | pb::WalkOrder::Path => WalkOrder::Path,
		pb::WalkOrder::Native => WalkOrder::Unordered,
	};
	let directory_errors = match options
		.map(|options| pb::DirectoryErrorMode::try_from(options.directory_errors))
		.transpose()
		.map_err(|_| {
			(
				pb::ProtocolErrorCode::InvalidArgument,
				"walk directory_errors value is invalid".to_owned(),
			)
		})?
		.unwrap_or(pb::DirectoryErrorMode::SkipSkippable)
	{
		pb::DirectoryErrorMode::Unspecified | pb::DirectoryErrorMode::SkipSkippable => {
			DirectoryErrorMode::SkipSkippable
		},
		pb::DirectoryErrorMode::Visit => DirectoryErrorMode::Visit,
	};
	let options = options.cloned().unwrap_or_default();
	let mut walk = WalkRequest::from_options(root, WalkOptions {
		include_hidden: options.include_hidden,
		use_gitignore: options.use_gitignore,
		skip_git: options.skip_git,
		skip_node_modules: options.skip_node_modules,
		follow_links,
		detail,
		order,
		emit_root: options.emit_root,
		min_depth: usize::try_from(options.min_depth).unwrap_or(usize::MAX),
		max_depth: if options.max_depth == 0 {
			usize::MAX
		} else {
			usize::try_from(options.max_depth).unwrap_or(usize::MAX)
		},
		contents_first: options.contents_first,
		directory_errors,
		same_file_system: options.same_file_system,
		cache: options.cache,
	});
	if !request.include.is_empty() {
		let glob = CompiledWalkGlob::new(request.include.iter().cloned()).map_err(|error| {
			(
				pb::ProtocolErrorCode::InvalidArgument,
				fault_in("walk include glob is invalid", error).to_string(),
			)
		})?;
		walk = walk.filter(WalkFilter::all().glob(glob));
	}
	if let Some(limit) = request.limit {
		walk = walk.limit(usize::try_from(limit).unwrap_or(usize::MAX));
	}
	Ok(walk)
}

pub(in crate::server) async fn send_materialization_error(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	error: &MaterializationError,
) {
	let code = match error {
		MaterializationError::InvalidUri => pb::ProtocolErrorCode::InvalidArgument,
		MaterializationError::NotFound => pb::ProtocolErrorCode::NotFound,
		MaterializationError::UnsupportedScheme => pb::ProtocolErrorCode::Unsupported,
		MaterializationError::OutsideGrant | MaterializationError::SymbolicLink => {
			pb::ProtocolErrorCode::PermissionDenied
		},
		MaterializationError::TooLarge { .. } => pb::ProtocolErrorCode::ResourceExhausted,
		MaterializationError::Io(_) => pb::ProtocolErrorCode::Internal,
	};
	send_error(responses, request_id, code, &error.to_string()).await;
}

pub(in crate::server) async fn send_exec_error(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	error: &ExecError,
) {
	let code = match error {
		ExecError::SessionNotFound
		| ExecError::RunNotFound
		| ExecError::FinalCwdNotFound
		| ExecError::ProcessNotFound { name: _ } => pb::ProtocolErrorCode::NotFound,
		ExecError::ProcessExists { name: _ } => pb::ProtocolErrorCode::AlreadyExists,
		ExecError::StaleProcessGeneration { .. } | ExecError::StaleFinalCwdRevision => {
			pb::ProtocolErrorCode::PreconditionFailed
		},
		ExecError::UnsupportedSignal { signal: _ } | ExecError::UnsupportedShellProfile { .. } => {
			pb::ProtocolErrorCode::Unsupported
		},
		ExecError::WireRevision
		| ExecError::InvalidControl
		| ExecError::InvalidProcessName
		| ExecError::DetachedPty => pb::ProtocolErrorCode::InvalidArgument,
		_ => pb::ProtocolErrorCode::Internal,
	};
	send_error(responses, request_id, code, &error.to_string()).await;
}

pub(in crate::server) fn worker_operation_allowed(operation: &str) -> bool {
	operation.starts_with("omp.env.docs.")
		|| operation.starts_with("omp.env.fs.")
		|| operation.starts_with("omp.env.find.")
		|| operation.starts_with("omp.env.http.")
		|| matches!(
			operation,
			"omp.env.blobs.stat"
				| "omp.env.blobs.get"
				| "omp.env.blobs.put"
				| "omp.env.blobs.commit_put"
		)
}

pub(in crate::server) async fn send_blob_error(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	error: &BlobError,
) {
	let code = match error {
		BlobError::InvalidHash
		| BlobError::HashMismatch
		| BlobError::SizeMismatch { .. }
		| BlobError::InvalidRange { .. }
		| BlobError::LengthOverflow => pb::ProtocolErrorCode::InvalidArgument,
		BlobError::Store(blob::Error::NotFound) => pb::ProtocolErrorCode::NotFound,
		BlobError::VerdictPinned => pb::ProtocolErrorCode::PreconditionFailed,
		BlobError::Store(_)
		| BlobError::Remove(_)
		| BlobError::ReadTruncated { .. }
		| BlobError::FinalizeTask(_)
		| BlobError::ArtifactMetadata(_)
		| BlobError::JournalScan { .. }
		| BlobError::UnsupportedJournalResult { .. }
		| BlobError::JournalResult { .. }
		| BlobError::ArtifactClock => pb::ProtocolErrorCode::Internal,
	};
	send_error(responses, request_id, code, &error.to_string()).await;
}

pub(in crate::server) fn connection_lease_id(
	target: Option<&document_pb::DocumentTarget>,
) -> Option<&Bytes> {
	let document_target::Target::LeaseId(lease_id) = target?.target.as_ref()? else {
		return None;
	};
	Some(lease_id)
}

pub(in crate::server) fn connection_lease<'a>(
	connection: &'a ConnectionState,
	target: Option<&document_pb::DocumentTarget>,
) -> Option<&'a DocumentLease> {
	let document_target::Target::LeaseId(lease_id) = target?.target.as_ref()? else {
		return None;
	};
	connection.document_leases.get(lease_id)
}

pub(in crate::server) async fn send_document_error(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	error: &DocumentError,
) {
	let code = match error {
		DocumentError::Protocol { code, .. } => {
			match document_pb::ProtocolErrorCode::try_from(*code) {
				Ok(document_pb::ProtocolErrorCode::InvalidArgument) => {
					pb::ProtocolErrorCode::InvalidArgument
				},
				Ok(document_pb::ProtocolErrorCode::NotFound) => pb::ProtocolErrorCode::NotFound,
				Ok(document_pb::ProtocolErrorCode::PermissionDenied) => {
					pb::ProtocolErrorCode::PermissionDenied
				},
				Ok(document_pb::ProtocolErrorCode::Unsupported) => pb::ProtocolErrorCode::Unsupported,
				Ok(document_pb::ProtocolErrorCode::AlreadyExists) => {
					pb::ProtocolErrorCode::AlreadyExists
				},

				Ok(document_pb::ProtocolErrorCode::Cancelled) => pb::ProtocolErrorCode::Cancelled,
				Ok(
					document_pb::ProtocolErrorCode::RevisionExpired
					| document_pb::ProtocolErrorCode::PreconditionFailed
					| document_pb::ProtocolErrorCode::ContentModified,
				) => pb::ProtocolErrorCode::PreconditionFailed,
				_ => pb::ProtocolErrorCode::Internal,
			}
		},
		DocumentError::Cancelled => pb::ProtocolErrorCode::Cancelled,
		DocumentError::Disconnected => pb::ProtocolErrorCode::Internal,
		DocumentError::MalformedResponse(_) => pb::ProtocolErrorCode::InvalidArgument,
		DocumentError::Wire(_) => pb::ProtocolErrorCode::Internal,
		DocumentError::WriteScope(_) => pb::ProtocolErrorCode::PermissionDenied,
	};
	send_error(responses, request_id, code, &error.to_string()).await;
}

pub(in crate::server) async fn send_http_error(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	error: &HttpEgressError,
) {
	let code = match error {
		HttpEgressError::InvalidArgument(_) => pb::ProtocolErrorCode::InvalidArgument,
		HttpEgressError::TimedOut => pb::ProtocolErrorCode::DeadlineExceeded,
		HttpEgressError::ResponseTooLarge => pb::ProtocolErrorCode::ResourceExhausted,
		HttpEgressError::UnsupportedSocksProxy { .. } => pb::ProtocolErrorCode::Internal,
		HttpEgressError::Transport(_) => pb::ProtocolErrorCode::Internal,
	};
	send_error(responses, request_id, code, &error.to_string()).await;
}

pub(in crate::server) fn frame_data_operation(
	body: &client_frame::Body,
) -> Option<(&'static str, &'static str)> {
	match body {
		client_frame::Body::OpenSession(_) => Some(("omp.env.sh.open_session", "env.exec")),
		client_frame::Body::CloseSession(_) => Some(("omp.env.sh.close_session", "env.exec")),
		client_frame::Body::Exec(_) => Some(("omp.env.sh.exec", "env.exec")),
		client_frame::Body::Stdin(_) => Some(("omp.env.sh.stdin", "env.exec")),
		client_frame::Body::Signal(_) => Some(("omp.env.sh.signal", "env.exec")),
		client_frame::Body::Resize(_) => Some(("omp.env.sh.resize", "env.exec")),
		client_frame::Body::StartProcess(_) => Some(("omp.env.proc.start", "env.process")),
		client_frame::Body::GetProcess(_) => Some(("omp.env.Process.info", "env.process")),
		client_frame::Body::RestartProcess(_) => Some(("omp.env.Process.restart", "env.process")),
		client_frame::Body::HttpRequest(request) => Some((
			match request.method.as_str() {
				"POST" => "omp.env.http.post",
				"PUT" => "omp.env.http.put",
				_ => "omp.env.http.get",
			},
			"env.net",
		)),
		client_frame::Body::ListProcesses(_) => Some(("omp.env.proc.list", "env.process")),
		client_frame::Body::AttachOutput(_) => Some(("omp.env.proc.attach", "env.process")),
		client_frame::Body::SendInput(_) => Some(("omp.env.proc.send_input", "env.process")),
		client_frame::Body::SignalProcess(_) => Some(("omp.env.proc.signal", "env.process")),
		client_frame::Body::StopProcess(_) => Some(("omp.env.proc.stop", "env.process")),
		client_frame::Body::BlobStat(_) => Some(("omp.env.blobs.stat", "env.blob")),
		client_frame::Body::BlobGet(_) => Some(("omp.env.blobs.get", "env.blob")),
		client_frame::Body::BlobPutChunk(_) => Some(("omp.env.blobs.put", "env.blob")),
		client_frame::Body::BlobPutCommit(_) => Some(("omp.env.blobs.commit_put", "env.blob")),
		client_frame::Body::BlobDelete(_) => Some(("omp.env.blobs.delete", "env.blob")),
		_ => None,
	}
}
pub(in crate::server) fn requests_pty(body: &client_frame::Body) -> bool {
	match body {
		client_frame::Body::OpenSession(request) => request.pty.is_some(),
		client_frame::Body::StartProcess(request) => {
			request.spec.as_ref().is_some_and(|spec| spec.pty.is_some())
		},
		_ => false,
	}
}

pub(in crate::server) async fn authorize_data_operation(
	connection: &ConnectionState,
	scope: Option<&pb::InvocationScope>,
	operation: &'static str,
	capability: &'static str,
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
) -> bool {
	let Some(spec) = omp_tool::operation_spec(operation) else {
		send_error(
			responses,
			request_id,
			pb::ProtocolErrorCode::Unsupported,
			"DATA operation has no canonical OperationSpec",
		)
		.await;
		return false;
	};
	if spec.authority != omp_tool::Authority::Environment {
		send_error(
			responses,
			request_id,
			pb::ProtocolErrorCode::PermissionDenied,
			"DATA operation is not Environment-authoritative",
		)
		.await;
		return false;
	}
	if !connection.grants(capability) {
		send_policy_error(responses, request_id, PolicyError::Denied { capability }).await;
		return false;
	}
	if scope.is_none() {
		if connection.host.is_some() {
			send_policy_error(responses, request_id, PolicyError::EffectsNotAuthorized).await;
			return false;
		}
		return true;
	}
	if !spec
		.minimum_phase
		.has_reached(omp_core::InvocationPhase::EffectsAuthorized)
	{
		send_error(
			responses,
			request_id,
			pb::ProtocolErrorCode::Internal,
			"DATA OperationSpec does not enforce EFFECTS_AUTHORIZED",
		)
		.await;
		return false;
	}
	let Some(scope) = scope else {
		send_policy_error(responses, request_id, PolicyError::EffectsNotAuthorized).await;
		return false;
	};
	let Some(host) = &connection.host else {
		send_policy_error(responses, request_id, PolicyError::EffectsNotAuthorized).await;
		return false;
	};
	let worker_scope = connection
		.authority
		.is_worker_invocation(host, &scope.invocation_id);
	let credentials = DataAuthority {
		invocation_id:      &scope.invocation_id,
		effect_token:       &scope.effect_token,
		host_generation:    scope.host_generation,
		session_generation: scope.session_generation,
	};
	let result = if capability == "env.search" {
		connection
			.authority
			.validate_read(host, connection.connection_owner, credentials)
	} else {
		connection
			.authority
			.validate(host, connection.connection_owner, credentials, capability)
	};
	if let Err(error) = result {
		send_policy_error(responses, request_id, error).await;
		return false;
	}
	if worker_scope && !worker_operation_allowed(operation) {
		send_policy_error(responses, request_id, PolicyError::Denied { capability }).await;
		return false;
	}
	true
}

pub(in crate::server) async fn send_policy_error(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	error: PolicyError,
) {
	if let PolicyError::QuotaExceeded { quota, limit, used } = &error {
		let props = ValueMap {
			fields: BTreeMap::from([
				("quota".to_owned(), Value { kind: Some(value::Kind::String((*quota).to_owned())) }),
				("limit".to_owned(), Value { kind: Some(value::Kind::Int(*limit as i64)) }),
				("used".to_owned(), Value { kind: Some(value::Kind::Int(*used as i64)) }),
			]),
		};
		send_body(
			responses,
			request_id,
			server_frame::Body::Error(pb::ProtocolError {
				code:    pb::ProtocolErrorCode::ResourceExhausted.into(),
				message: format!("QuotaExceeded: quota={quota}"),
				props:   Some(props),
			}),
		)
		.await;
		return;
	}
	let (code, message) = match error {
		PolicyError::EffectsNotAuthorized => (
			pb::ProtocolErrorCode::Uncommitted,
			sf!("omp.EffectsNotAuthorized: invocation has not reached EFFECTS_AUTHORIZED"),
		),
		PolicyError::Denied { capability } => (
			pb::ProtocolErrorCode::PermissionDenied,
			Str::from(format!(
				"Denied: effect envelope does not grant {capability}; escalation is not re-prompted"
			)),
		),
		PolicyError::InvalidEffectToken => (
			pb::ProtocolErrorCode::PermissionDenied,
			sf!("Denied: effect token is absent, mismatched, revoked, or connection-bound",),
		),
		PolicyError::StaleGeneration => (
			pb::ProtocolErrorCode::PreconditionFailed,
			sf!("StaleGeneration: host or session generation is stale"),
		),
		PolicyError::LeaseNotOwned => (
			pb::ProtocolErrorCode::PermissionDenied,
			sf!("Denied: document lease belongs to another connection"),
		),
		PolicyError::EnforcementUnavailable => (
			pb::ProtocolErrorCode::Unsupported,
			sf!("EnforcementUnavailable: sandbox ENFORCE is deferred; refusing instead of degrading",),
		),
		PolicyError::QuotaExceeded { .. } => unreachable!("quota errors returned above"),
	};
	send_error(responses, request_id, code, message.as_str()).await;
}

pub(in crate::server) async fn send_data_response(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	body: data_response::Body,
) {
	send_body(
		responses,
		request_id,
		server_frame::Body::Data(pb::DataResponse { body: Some(body), props: Default::default() }),
	)
	.await;
}

pub(in crate::server) async fn send_stream_error(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	kind: pb::EventStreamKind,
	invocation_id: &str,
	exec: &[u8],
	process_name: &str,
	message: &str,
) {
	send_body(
		responses,
		request_id,
		server_frame::Body::EventStreamError(pb::EventStreamError {
			stream:         kind as i32,
			failure:        pb::EventStreamFailure::Closed as i32,
			invocation_id:  invocation_id.to_owned(),
			exec:           Bytes::copy_from_slice(exec),
			process_name:   process_name.to_owned(),
			skipped_events: 0,
			message:        message.to_owned(),
			props:          Default::default(),
		}),
	)
	.await;
}

/// Sends a protocol error frame, rendering the message once at this
/// model-facing fault boundary.
pub(in crate::server) async fn send_error(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	code: pb::ProtocolErrorCode,
	message: impl std::fmt::Display,
) {
	send_body(
		responses,
		request_id,
		server_frame::Body::Error(pb::ProtocolError {
			code:    code as i32,
			message: message.to_string(),
			props:   Default::default(),
		}),
	)
	.await;
}

pub(in crate::server) async fn send_body(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	body: server_frame::Body,
) {
	let _ = responses
		.send_async(checked_server_frame(request_id, body))
		.await;
}

pub(in crate::server) async fn send_invocation_body(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	body: server_frame::Body,
) -> bool {
	matches!(
		time::timeout(
			INVOCATION_RESPONSE_SEND_GRACE,
			responses.send_async(checked_server_frame(request_id, body)),
		)
		.await,
		Ok(Ok(()))
	)
}

pub(in crate::server) async fn send_invocation_terminal_body(
	responses: &flume::Sender<pb::ServerFrame>,
	request_id: u64,
	body: server_frame::Body,
) {
	let frame = checked_server_frame(request_id, body);
	let retry = frame.clone();
	if time::timeout(INVOCATION_RESPONSE_SEND_GRACE, responses.send_async(frame))
		.await
		.is_err()
	{
		let responses = responses.clone();
		tokio::spawn(async move {
			let _ = responses.send_async(retry).await;
		});
	}
}

pub(in crate::server) fn checked_server_frame(
	request_id: u64,
	body: server_frame::Body,
) -> pb::ServerFrame {
	let mut frame = server_frame(request_id, body);
	if frame.encoded_len() > FRAME_LIMIT {
		frame = server_frame(
			request_id,
			server_frame::Body::Error(pb::ProtocolError {
				code:    pb::ProtocolErrorCode::Internal as i32,
				message: "environment response exceeds the configured frame limit".to_owned(),
				props:   Default::default(),
			}),
		);
	}
	frame
}

pub(in crate::server) fn server_frame(
	request_id: u64,
	body: server_frame::Body,
) -> pb::ServerFrame {
	pb::ServerFrame { request_id, body: Some(body), props: Default::default() }
}

pub(in crate::server) async fn read_server_frame<R>(
	reader: &mut R,
	scratch: &mut BytesMut,
) -> io::Result<Option<pb::ServerFrame>>
where
	R: AsyncRead + Unpin,
{
	let Some(length) = read_length(reader).await? else {
		return Ok(None);
	};
	if length > FRAME_LIMIT {
		return Err(io::Error::new(
			io::ErrorKind::InvalidData,
			"environment frame exceeds the configured limit",
		));
	}
	scratch.clear();
	scratch.resize(length, 0);
	reader.read_exact(scratch).await?;
	pb::ServerFrame::decode(&scratch[..])
		.map(Some)
		.map_err(io::Error::other)
}

pub(in crate::server) async fn write_client_frame<W>(
	writer: &mut W,
	frame: &pb::ClientFrame,
	scratch: &mut BytesMut,
) -> io::Result<()>
where
	W: AsyncWrite + Unpin,
{
	if frame.encoded_len() > FRAME_LIMIT {
		return Err(io::Error::new(
			io::ErrorKind::InvalidData,
			"environment frame exceeds the configured limit",
		));
	}
	scratch.clear();
	frame
		.encode_length_delimited(&mut *scratch)
		.map_err(io::Error::other)?;
	writer.write_all(scratch).await?;
	writer.flush().await
}
pub(in crate::server) async fn read_client_frame<R>(
	reader: &mut R,
	scratch: &mut BytesMut,
) -> io::Result<Option<pb::ClientFrame>>
where
	R: AsyncRead + Unpin,
{
	let Some(length) = read_length(reader).await? else {
		return Ok(None);
	};
	if length > FRAME_LIMIT {
		return Err(io::Error::new(
			io::ErrorKind::InvalidData,
			"environment frame exceeds the configured limit",
		));
	}
	scratch.clear();
	scratch.resize(length, 0);
	reader.read_exact(scratch).await?;
	pb::ClientFrame::decode(&scratch[..])
		.map(Some)
		.map_err(io::Error::other)
}

pub(in crate::server) async fn write_server_frame<W>(
	writer: &mut W,
	frame: &pb::ServerFrame,
	scratch: &mut BytesMut,
) -> io::Result<()>
where
	W: AsyncWrite + Unpin,
{
	if frame.encoded_len() > FRAME_LIMIT {
		return Err(io::Error::new(
			io::ErrorKind::InvalidData,
			"environment frame exceeds the configured limit",
		));
	}
	scratch.clear();
	frame
		.encode_length_delimited(&mut *scratch)
		.map_err(io::Error::other)?;
	writer.write_all(scratch).await?;
	writer.flush().await
}

pub(in crate::server) async fn read_length<R>(reader: &mut R) -> io::Result<Option<usize>>
where
	R: AsyncRead + Unpin,
{
	let mut value = 0_u64;
	for shift in (0..70).step_by(7) {
		let mut byte = [0_u8; 1];
		match reader.read_exact(&mut byte).await {
			Ok(_) => {},
			Err(error) if error.kind() == io::ErrorKind::UnexpectedEof && shift == 0 => {
				return Ok(None);
			},
			Err(error) => return Err(error),
		}
		let part = u64::from(byte[0] & 0x7f);
		if shift == 63 && part > 1 {
			return Err(io::Error::new(
				io::ErrorKind::InvalidData,
				"invalid environment frame length",
			));
		}
		value |= part << shift;
		if byte[0] & 0x80 == 0 {
			return usize::try_from(value).map(Some).map_err(io::Error::other);
		}
	}
	Err(io::Error::new(io::ErrorKind::InvalidData, "invalid environment frame length"))
}
