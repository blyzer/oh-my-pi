use omp_catalog::{WireTarget, provider::RouteDef};
use omp_core::Str;
use url::Url;

use crate::{
	error::{Error, ErrorDetail, ErrorKind, ErrorPhase, RetryAction},
	layer::ExecutionContext,
	receipt::ReasonId,
};

const CLOUDFLARE_PROVIDER: &str = "cloudflare-ai-gateway";
const ACCOUNT_ENV: &str = "OMP_CLOUDFLARE_ACCOUNT_ID";
const GATEWAY_ENV: &str = "OMP_CLOUDFLARE_AI_GATEWAY_NAME";

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum CloudflareEndpointError {
	#[error("missing value for {0}")]
	Missing(&'static str),
	#[error("invalid value for {0}")]
	Invalid(&'static str),
	#[error("non-Unicode value for {0}")]
	NotUnicode(&'static str),
}

impl CloudflareEndpointError {
	fn reason(self) -> &'static str {
		match self {
			Self::Missing(ACCOUNT_ENV) => "cloudflare-account-id-missing",
			Self::Missing(GATEWAY_ENV) => "cloudflare-gateway-name-missing",
			Self::Invalid(ACCOUNT_ENV) | Self::NotUnicode(ACCOUNT_ENV) => {
				"cloudflare-account-id-invalid"
			},
			Self::Invalid(GATEWAY_ENV) | Self::NotUnicode(GATEWAY_ENV) => {
				"cloudflare-gateway-name-invalid"
			},
			Self::Missing(_) => "cloudflare-endpoint-value-missing",
			Self::Invalid(_) | Self::NotUnicode(_) => "cloudflare-endpoint-value-invalid",
		}
	}
}

pub(crate) fn resolve_cloudflare_endpoint_template(
	template: &str,
	account: Option<&str>,
	gateway: Option<&str>,
) -> Result<Option<String>, CloudflareEndpointError> {
	let needs_account = template.contains("{account}");
	let needs_gateway = template.contains("{gateway}");
	if !needs_account && !needs_gateway {
		return Ok(None);
	}

	let mut resolved = template.to_owned();
	if needs_account {
		let account = account.ok_or(CloudflareEndpointError::Missing(ACCOUNT_ENV))?;
		validate_value(ACCOUNT_ENV, account)?;
		resolved = resolved.replace("{account}", account);
	}
	if needs_gateway {
		let gateway = gateway.ok_or(CloudflareEndpointError::Missing(GATEWAY_ENV))?;
		validate_value(GATEWAY_ENV, gateway)?;
		resolved = resolved.replace("{gateway}", gateway);
	}
	Ok(Some(resolved))
}

fn validate_value(name: &'static str, value: &str) -> Result<(), CloudflareEndpointError> {
	if value.is_empty()
		|| !value
			.bytes()
			.all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
	{
		return Err(CloudflareEndpointError::Invalid(name));
	}
	Ok(())
}

fn optional_env(name: &'static str) -> Result<Option<String>, CloudflareEndpointError> {
	match std::env::var(name) {
		Ok(value) => Ok(Some(value)),
		Err(std::env::VarError::NotPresent) => Ok(None),
		Err(std::env::VarError::NotUnicode(_)) => Err(CloudflareEndpointError::NotUnicode(name)),
	}
}

pub(crate) fn cloudflare_effective_route(
	route: &RouteDef,
	target: Option<&WireTarget>,
	context: &ExecutionContext,
) -> Result<Option<(RouteDef, WireTarget)>, Error> {
	if route.provider.as_str() != CLOUDFLARE_PROVIDER {
		return Ok(None);
	}
	let account =
		optional_env(ACCOUNT_ENV).map_err(|error| configuration_error(context, error.reason()))?;
	let gateway =
		optional_env(GATEWAY_ENV).map_err(|error| configuration_error(context, error.reason()))?;
	let endpoint = resolve_cloudflare_endpoint_template(
		route.endpoint.base_url.as_str(),
		account.as_deref(),
		gateway.as_deref(),
	)
	.map_err(|error| configuration_error(context, error.reason()))?
	.ok_or_else(|| configuration_error(context, "cloudflare-endpoint-placeholder-missing"))?;
	let parsed = Url::parse(&endpoint)
		.map_err(|_| configuration_error(context, "cloudflare-endpoint-invalid"))?;
	let mut effective_route = route.clone();
	effective_route.endpoint.base_url = Str::new(endpoint);
	effective_route.trust_domain.origin = Str::new(parsed.origin().ascii_serialization());
	let mut effective_target = target
		.cloned()
		.ok_or_else(|| configuration_error(context, "cloudflare-wire-target-missing"))?;
	effective_target.endpoint = effective_route.endpoint.clone();
	Ok(Some((effective_route, effective_target)))
}

fn configuration_error(context: &ExecutionContext, reason: &'static str) -> Error {
	Error::new(
		ErrorKind::InvalidRequest,
		ErrorPhase::Encoding,
		RetryAction::Never,
		context.receipt(),
	)
	.detail(ErrorDetail::protocol(ReasonId(Str::new(reason))))
}

#[cfg(test)]
mod tests {
	use super::{CloudflareEndpointError, resolve_cloudflare_endpoint_template};

	const TEMPLATE: &str = "https://gateway.ai.cloudflare.com/v1/{account}/{gateway}/anthropic";

	#[test]
	fn missing_account_is_a_typed_error() {
		assert_eq!(
			resolve_cloudflare_endpoint_template(TEMPLATE, None, Some("gateway-prod")),
			Err(CloudflareEndpointError::Missing("OMP_CLOUDFLARE_ACCOUNT_ID"))
		);
	}

	#[test]
	fn invalid_account_is_a_typed_error() {
		assert_eq!(
			resolve_cloudflare_endpoint_template(TEMPLATE, Some("account/123"), Some("gateway-prod"),),
			Err(CloudflareEndpointError::Invalid("OMP_CLOUDFLARE_ACCOUNT_ID"))
		);
	}

	#[test]
	fn missing_gateway_is_a_typed_error() {
		assert_eq!(
			resolve_cloudflare_endpoint_template(TEMPLATE, Some("account-123"), None),
			Err(CloudflareEndpointError::Missing("OMP_CLOUDFLARE_AI_GATEWAY_NAME"))
		);
	}

	#[test]
	fn invalid_gateway_is_a_typed_error() {
		assert_eq!(
			resolve_cloudflare_endpoint_template(TEMPLATE, Some("account-123"), Some("gateway/name"),),
			Err(CloudflareEndpointError::Invalid("OMP_CLOUDFLARE_AI_GATEWAY_NAME"))
		);
	}
}
