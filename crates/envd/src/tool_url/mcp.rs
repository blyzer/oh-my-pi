//! Opaque advertised MCP resource URI reads.
//!
//! Which mounted server answers a resource is live state: a server can start
//! or stop advertising it, and mounts come and go, between the moment a read
//! is judged and the moment it runs. A judged read therefore resolves its
//! server once, when it is judged, and pins it ([`InvocationPins`]): the
//! envelope judged, the host its approval names and the server its executor
//! asks are one resolution, and the executor refuses rather than ask another
//! server, or the same one once reading it reaches the network beyond what
//! was judged.

use std::sync::Arc;

use omp_core::{CowBytes, Str};
use omp_proto::env::v1::McpResourceRequest;
use omp_tool::{FetchEffects, InvocationPins, ResolutionPin};
use omp_tools::read::{
	Fault,
	resolver::{Resolve, ResourceCompletion, fuzzy_score},
	selector::ParsedSelector,
};
use tokio_util::sync::CancellationToken;

use crate::mcp::McpService;

/// The resolver name a read's judgment pins its MCP servers under.
const PIN_RESOLVER: &str = "mcp";

/// Environment-scoped MCP resource resolver.
pub(crate) struct McpUrlResolver {
	service: Arc<McpService>,
}

impl McpUrlResolver {
	pub(crate) const fn new(service: Arc<McpService>) -> Self {
		Self { service }
	}

	/// Names the mounted server a read of `resource` asks, as its judgment
	/// pinned it ([`Self::judged`]); `None` when no mounted server advertised
	/// it then.
	pub(crate) fn fetch_server(&self, resource: &str) -> Option<Str> {
		let uri = self.parse(resource).ok()?;
		self.judged(uri).target
	}

	/// The server a read of `uri` asks and the fetch that is, resolved once
	/// per judged call from the mount advertising it when the call is judged
	/// ([`McpService::resource_read_pin`]) and pinned for the rest of the call;
	/// a call nobody judged resolves it live.
	fn judged(&self, uri: &str) -> ResolutionPin {
		InvocationPins::pin(PIN_RESOLVER, uri, || self.service.resource_read_pin(uri))
	}

	fn parse<'a>(&self, resource: &'a str) -> Result<&'a str, Fault> {
		if resource.is_empty() {
			return Err(Fault::Invalid {
				message: Str::new_static("mcp:// reads require a nonempty advertised resource URI."),
			});
		}
		Ok(resource)
	}
}

impl Resolve for McpUrlResolver {
	/// The fetch a read of `resource` performs, judged from the server
	/// advertising it when the call is judged and pinned for its execution
	/// ([`Self::judged`]): a remote server is reached with its configured
	/// credentials, a local one only when its declared tier says it reaches
	/// the network. A resource no mounted server advertises yet may be
	/// advertised by a remote one by the time the read runs, so it fetches;
	/// an empty one is refused before.
	fn read_fetch(&self, resource: &str, _query: Option<&str>) -> Option<FetchEffects> {
		let uri = self.parse(resource).ok()?;
		self.judged(uri).fetch
	}

	/// Reads from the server the call's judgment pinned, refused when that
	/// server is no longer mounted or now reaches the network beyond the
	/// judged fetch, never sent to another one. A read judged before any
	/// server advertised the resource (judged as a credentialed fetch, its
	/// approval naming no server) and a read no dispatcher judged ask the
	/// server advertising it now.
	async fn read<'a>(
		&'a self,
		resource: &'a str,
		_selector: &'a ParsedSelector,
	) -> Result<CowBytes<'static>, Fault> {
		let uri = self.parse(resource)?;
		let pin = InvocationPins::pinned(PIN_RESOLVER, uri);
		let server = if let Some(pin) = &pin
			&& let Some(server) = pin.target.as_deref()
		{
			self
				.service
				.pinned_resource_server(server, pin)
				.map_err(|error| Fault::Source { message: Str::new(error.to_string()) })?
		} else {
			self
				.service
				.resolve_resource_server(uri)
				.ok_or_else(|| Fault::Source {
					message: Str::new(format!("MCP resource '{uri}' is not advertised.")),
				})?
		};
		let result = self
			.service
			.resource(
				McpResourceRequest {
					server:        Some(server),
					uri:           uri.to_owned(),
					max_bytes:     8 * 1024 * 1024,
					wire_revision: 1,
				},
				CancellationToken::new(),
			)
			.await
			.map_err(|error| Fault::Source { message: Str::new(error.to_string()) })?;
		if result.truncated {
			return Err(Fault::Source {
				message: Str::new_static("MCP resource exceeded the read size limit."),
			});
		}
		Ok(CowBytes::from(result.content))
	}

	async fn complete(
		&self,
		query: &str,
		max_results: usize,
	) -> Result<Vec<ResourceCompletion>, Fault> {
		let mut matches = self
			.service
			.resource_uris()
			.into_iter()
			.filter_map(|uri| {
				let value = Str::new(format!("mcp://{uri}"));
				let score = fuzzy_score(query, &value)?;
				Some(ResourceCompletion {
					value,
					description: Str::new_static("advertised MCP resource"),
					score,
				})
			})
			.collect::<Vec<_>>();
		matches.sort_unstable_by(|left, right| {
			right
				.score
				.cmp(&left.score)
				.then_with(|| left.value.cmp(&right.value))
		});
		matches.truncate(max_results);
		Ok(matches)
	}
}
