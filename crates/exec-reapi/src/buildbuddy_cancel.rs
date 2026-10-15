//! BuildBuddy's invocation-scoped cancellation RPC.

use tonic::{Request, metadata::MetadataValue, transport::Channel};

use crate::{
	endpoint::{Endpoint, EndpointError, Scheme},
	profile::Credential,
	proto::buildbuddy::{
		invocation::CancelExecutionsRequest,
		service::buildbuddy_service_client::BuildBuddyServiceClient,
	},
};

/// Failure to configure or connect the vendor cancellation channel.
#[derive(Debug, thiserror::Error)]
pub enum CancelConnectError {
	/// The endpoint is invalid or would disclose the API key over plaintext.
	#[error("unsafe BuildBuddy endpoint")]
	Endpoint(#[from] EndpointError),
	/// The transport could not be configured or connected.
	#[error("BuildBuddy cancellation transport failed")]
	Transport(#[from] tonic::transport::Error),
}

/// Cloneable invocation canceller. The key is only written into gRPC metadata.
#[derive(Clone)]
pub struct BuildBuddyCanceller {
	client:     BuildBuddyServiceClient<Channel>,
	credential: Credential,
}

impl BuildBuddyCanceller {
	/// Reuses the already validated REAPI channel and credential.
	pub fn from_channel(channel: Channel, credential: Credential) -> Self {
		Self { client: BuildBuddyServiceClient::new(channel), credential }
	}

	/// Connects to a validated BuildBuddy endpoint.
	///
	/// # Errors
	/// Returns a typed endpoint or transport failure; plaintext is restricted to
	/// loopback.
	pub async fn connect(
		endpoint: &str,
		credential: Credential,
	) -> Result<Self, CancelConnectError> {
		let endpoint = Endpoint::parse(endpoint)?;
		let mut builder = tonic::transport::Endpoint::from_shared(endpoint.transport_url())?
			.connect_timeout(std::time::Duration::from_secs(30));
		if endpoint.scheme() == Scheme::Tls {
			builder =
				builder.tls_config(tonic::transport::ClientTlsConfig::new().with_webpki_roots())?;
		}
		Ok(Self::from_channel(builder.connect().await?, credential))
	}

	/// Requests cancellation of exactly `invocation_id`.
	///
	/// # Errors
	/// Returns the gRPC status if the server rejects or cannot receive the
	/// request.
	pub async fn cancel_invocation(&self, invocation_id: &str) -> Result<(), tonic::Status> {
		let mut request = Request::new(CancelExecutionsRequest {
			request_context: None,
			invocation_id:   invocation_id.to_owned(),
		});
		let mut key = MetadataValue::try_from(self.credential.expose())
			.map_err(|_| tonic::Status::invalid_argument("invalid API key metadata"))?;
		key.set_sensitive(true);
		request.metadata_mut().insert("x-buildbuddy-api-key", key);
		let mut client = self.client.clone();
		client.cancel_executions(request).await?;
		Ok(())
	}
}
