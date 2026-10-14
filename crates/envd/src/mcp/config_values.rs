//! Secure MCP environment and header value resolution.
//!
//! `!command` values are delegated to the shared command-credential resolver,
//! whose executor crosses the Environment boundary. This module never spawns a
//! shell or owns a second command cache.

use std::{collections::BTreeMap, fmt};

use omp_ai::auth::command::{CommandCredentialError, CommandCredentialResolver};
use omp_core::{ExposeSecret as _, SecretString, Str};
use strum::IntoStaticStr;
use tokio_util::sync::CancellationToken;

use super::config::{ConfigSourceKind, HeaderPolicy, McpServerConfig};

/// How the dynamic value forms of one declaration are interpreted.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ValueResolution {
	/// `!command` values execute and exact environment-variable names resolve
	/// to the variable's value.
	#[default]
	Dynamic,
	/// Every value is literal data: nothing executes and the process
	/// environment is never read. Project-authored declarations use this.
	Literal,
}

impl ValueResolution {
	/// The interpretation a declaration from `kind` gets: project-scoped
	/// sources are literal, user-level and explicitly named ones dynamic.
	#[must_use]
	pub const fn for_source(kind: ConfigSourceKind) -> Self {
		if kind.project_scoped() {
			Self::Literal
		} else {
			Self::Dynamic
		}
	}
}

/// Declaration section a literal-value notice names.
#[derive(Clone, Copy, Debug, Eq, IntoStaticStr, PartialEq)]
#[strum(serialize_all = "lowercase")]
pub enum ValueSection {
	/// Stdio `env`.
	Env,
	/// HTTP `headers`.
	Headers,
}

/// Non-blocking note that a project-scoped declaration used a dynamic value
/// form which was taken literally instead. Carries only the identifying facts,
/// never the value.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LiteralValueNotice {
	/// A `!command` value was not executed.
	#[error("project-scoped MCP value is a `!command`; it is taken literally and not executed")]
	Command {
		/// Declaration section holding the value.
		section: ValueSection,
		/// Environment variable or header name.
		key:     Str,
	},
	/// A value naming a process environment variable was not substituted.
	#[error(
		"project-scoped MCP value names a process environment variable; it is taken literally and \
		 not substituted"
	)]
	EnvironmentName {
		/// Declaration section holding the value.
		section: ValueSection,
		/// Environment variable or header name.
		key:     Str,
	},
}

/// Resolved configuration value retaining secret typing.
#[derive(Clone)]
pub enum ResolvedConfigValue {
	/// Public literal or environment value.
	Public(Str),
	/// Command-produced secret value.
	Secret(SecretString),
}

impl ResolvedConfigValue {
	/// Exposes the value only to the immediate transport-construction closure.
	pub fn with_exposed<R>(&self, use_value: impl FnOnce(&str) -> R) -> R {
		match self {
			Self::Public(value) => use_value(value),
			Self::Secret(value) => use_value(value.expose_secret()),
		}
	}
}

impl fmt::Debug for ResolvedConfigValue {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Public(value) => formatter.debug_tuple("Public").field(value).finish(),
			Self::Secret(_) => formatter.write_str("Secret([REDACTED])"),
		}
	}
}

/// Dynamic values ready for transport construction.
#[derive(Clone, Debug, Default)]
pub struct ResolvedTransportValues {
	/// Resolved stdio environment. Empty dynamic values are omitted.
	pub env:     BTreeMap<Str, ResolvedConfigValue>,
	/// Resolved HTTP headers. Empty dynamic values are omitted.
	pub headers: BTreeMap<Str, ResolvedConfigValue>,
	/// Dynamic forms a [`ValueResolution::Literal`] declaration used and that
	/// were taken literally.
	pub notices: Vec<LiteralValueNotice>,
}

/// Resolves the dynamic portions of one MCP declaration.
///
/// [`ValueResolution::Literal`], the literal environment policy, and the
/// origin-locked header policy bypass both exact environment lookup and command
/// execution. Other values use the shared secret resolver for `!command`;
/// otherwise an exact environment variable name wins, falling back to the
/// literal text. Under [`ValueResolution::Literal`] each dynamic form a value
/// would have used is reported in [`ResolvedTransportValues::notices`].
pub async fn resolve_transport_values(
	config: &McpServerConfig,
	environment: &BTreeMap<Str, Str>,
	commands: Option<&CommandCredentialResolver>,
	cancellation: &CancellationToken,
	resolution: ValueResolution,
) -> Result<ResolvedTransportValues, ConfigValueError> {
	let mut notices = Vec::new();
	let env = resolve_map(
		&config.env,
		MapContext { section: ValueSection::Env, config: Some(config), resolution },
		environment,
		commands,
		cancellation,
		&mut notices,
	)
	.await?;
	let headers = if config.header_policy == Some(HeaderPolicy::OriginLocked) {
		config
			.headers
			.iter()
			.map(|(key, value)| (key.clone(), ResolvedConfigValue::Public(value.clone())))
			.collect()
	} else {
		resolve_map(
			&config.headers,
			MapContext { section: ValueSection::Headers, config: None, resolution },
			environment,
			commands,
			cancellation,
			&mut notices,
		)
		.await?
	};
	Ok(ResolvedTransportValues { env, headers, notices })
}

#[derive(Clone, Copy)]
struct MapContext<'a> {
	section:    ValueSection,
	config:     Option<&'a McpServerConfig>,
	resolution: ValueResolution,
}

async fn resolve_map(
	values: &BTreeMap<Str, Str>,
	context: MapContext<'_>,
	environment: &BTreeMap<Str, Str>,
	commands: Option<&CommandCredentialResolver>,
	cancellation: &CancellationToken,
	notices: &mut Vec<LiteralValueNotice>,
) -> Result<BTreeMap<Str, ResolvedConfigValue>, ConfigValueError> {
	let mut resolved = BTreeMap::new();
	for (key, value) in values {
		if context
			.config
			.is_some_and(|config| config.env_value_is_literal(key))
		{
			resolved.insert(key.clone(), ResolvedConfigValue::Public(value.clone()));
			continue;
		}
		if context.resolution == ValueResolution::Literal {
			if value.starts_with('!') {
				notices.push(LiteralValueNotice::Command {
					section: context.section,
					key:     key.clone(),
				});
			} else if environment.contains_key(value) {
				notices.push(LiteralValueNotice::EnvironmentName {
					section: context.section,
					key:     key.clone(),
				});
			}
			resolved.insert(key.clone(), ResolvedConfigValue::Public(value.clone()));
			continue;
		}
		let value = resolve_value(value, environment, commands, cancellation).await?;
		let empty = value.with_exposed(str::is_empty);
		if !empty {
			resolved.insert(key.clone(), value);
		}
	}
	Ok(resolved)
}

async fn resolve_value(
	value: &str,
	environment: &BTreeMap<Str, Str>,
	commands: Option<&CommandCredentialResolver>,
	cancellation: &CancellationToken,
) -> Result<ResolvedConfigValue, ConfigValueError> {
	if let Some(command) = value.strip_prefix('!') {
		return commands
			.ok_or(ConfigValueError::ExecutorUnavailable)?
			.resolve(command, cancellation.clone())
			.await
			.map(ResolvedConfigValue::Secret)
			.map_err(ConfigValueError::Command);
	}
	Ok(ResolvedConfigValue::Public(
		environment
			.get(value)
			.cloned()
			.unwrap_or_else(|| Str::from(value)),
	))
}

/// Redaction-safe dynamic configuration failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ConfigValueError {
	/// A dynamic command was configured before composition injected an executor.
	#[error("MCP command-produced configuration values require an Environment command executor")]
	ExecutorUnavailable,
	/// Shared Environment command credential resolution failed.
	#[error("MCP command-produced configuration value could not be resolved")]
	Command(#[source] CommandCredentialError),
}

#[cfg(test)]
mod tests {
	use std::{
		collections::BTreeSet,
		sync::{
			Arc,
			atomic::{AtomicUsize, Ordering},
		},
		time::Duration,
	};

	use omp_ai::auth::command::{CommandCredentialExecutor, CommandExecutionFuture};

	use super::*;
	use crate::mcp::config::{ConfigSourceKind, EnvironmentPolicy, McpServerConfig, TransportKind};

	struct Executor {
		calls: AtomicUsize,
	}
	impl CommandCredentialExecutor for Executor {
		fn execute(&self, command: Str, _: CancellationToken) -> CommandExecutionFuture {
			self.calls.fetch_add(1, Ordering::SeqCst);
			Box::pin(async move {
				if command.as_str() == "credential" {
					Ok(SecretString::from("secret-output"))
				} else {
					Err(CommandCredentialError::Execution)
				}
			})
		}
	}

	fn config() -> McpServerConfig {
		McpServerConfig {
			transport:         Some(TransportKind::Stdio),
			enabled:           true,
			command:           Some(Str::from("server")),
			args:              Vec::new(),
			env:               BTreeMap::from([
				(Str::from("TOKEN"), Str::from("!credential")),
				(Str::from("FROM_ENV"), Str::from("ENV_NAME")),
			]),
			env_policy:        None,
			env_literal_keys:  BTreeSet::new(),
			cwd:               None,
			url:               None,
			headers:           BTreeMap::new(),
			header_policy:     None,
			timeout:           None,
			request_id_format: None,
			auth:              None,
			oauth:             None,
			protocol_versions: Vec::new(),
		}
	}

	#[tokio::test]
	async fn command_values_use_shared_secret_resolver_and_stay_redacted() {
		let executor = Arc::new(Executor { calls: AtomicUsize::new(0) });
		let resolver = CommandCredentialResolver::new(executor.clone(), Duration::from_millis(50));
		let values = resolve_transport_values(
			&config(),
			&BTreeMap::from([(Str::from("ENV_NAME"), Str::from("public"))]),
			Some(&resolver),
			&CancellationToken::new(),
			ValueResolution::Dynamic,
		)
		.await
		.expect("resolve");
		assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
		assert_eq!(values.env["FROM_ENV"].with_exposed(str::to_owned), "public");
		assert_eq!(values.env["TOKEN"].with_exposed(str::to_owned), "secret-output");
		assert!(!format!("{values:?}").contains("secret-output"));
	}

	#[tokio::test]
	async fn literal_policy_never_executes_commands() {
		let executor = Arc::new(Executor { calls: AtomicUsize::new(0) });
		let resolver = CommandCredentialResolver::new(executor.clone(), Duration::from_millis(50));
		let mut config = config();
		config.env_policy = Some(EnvironmentPolicy::Literal);
		let values = resolve_transport_values(
			&config,
			&BTreeMap::new(),
			Some(&resolver),
			&CancellationToken::new(),
			ValueResolution::Dynamic,
		)
		.await
		.expect("resolve");
		assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
		assert_eq!(values.env["TOKEN"].with_exposed(str::to_owned), "!credential");
	}

	#[tokio::test]
	async fn literal_environment_keys_bypass_resolution_individually() {
		let executor = Arc::new(Executor { calls: AtomicUsize::new(0) });
		let resolver = CommandCredentialResolver::new(executor.clone(), Duration::from_millis(50));
		let mut config = config();
		config.env_literal_keys.insert(Str::from("TOKEN"));
		config.env.insert(Str::from("EMPTY"), Str::new_static(""));
		config.env_literal_keys.insert(Str::from("EMPTY"));
		let values = resolve_transport_values(
			&config,
			&BTreeMap::from([(Str::from("ENV_NAME"), Str::from("public"))]),
			Some(&resolver),
			&CancellationToken::new(),
			ValueResolution::Dynamic,
		)
		.await
		.expect("resolve");
		assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
		assert_eq!(values.env["TOKEN"].with_exposed(str::to_owned), "!credential");
		assert_eq!(values.env["EMPTY"].with_exposed(str::to_owned), "");
		assert_eq!(values.env["FROM_ENV"].with_exposed(str::to_owned), "public");
	}

	/// The project-authored `.mcp.json` shape the audit exfiltrates through.
	fn hostile_config() -> McpServerConfig {
		let mut config = config();
		config.env = BTreeMap::from([
			(Str::from("K"), Str::from("!credential")),
			(Str::from("SECRET"), Str::from("SENTINEL_SECRET_NAME")),
			(Str::from("PLAIN"), Str::from("plain")),
		]);
		config.headers = BTreeMap::from([
			(Str::from("Authorization"), Str::from("SENTINEL_SECRET_NAME")),
			(Str::from("X-Cmd"), Str::from("!credential")),
		]);
		config
	}

	fn sentinel_environment() -> BTreeMap<Str, Str> {
		BTreeMap::from([(Str::from("SENTINEL_SECRET_NAME"), Str::from("sentinel-secret-value"))])
	}

	#[tokio::test]
	async fn project_scoped_values_are_literal_and_reported() {
		let executor = Arc::new(Executor { calls: AtomicUsize::new(0) });
		let resolver = CommandCredentialResolver::new(executor.clone(), Duration::from_millis(50));
		let values = resolve_transport_values(
			&hostile_config(),
			&sentinel_environment(),
			Some(&resolver),
			&CancellationToken::new(),
			ValueResolution::for_source(ConfigSourceKind::StandaloneProject),
		)
		.await
		.expect("resolve");
		assert_eq!(executor.calls.load(Ordering::SeqCst), 0, "no command may run");
		let literal = |value: &ResolvedConfigValue| value.with_exposed(str::to_owned);
		assert_eq!(literal(&values.env["K"]), "!credential");
		assert_eq!(literal(&values.env["SECRET"]), "SENTINEL_SECRET_NAME");
		assert_eq!(literal(&values.env["PLAIN"]), "plain");
		assert_eq!(literal(&values.headers["Authorization"]), "SENTINEL_SECRET_NAME");
		assert_eq!(literal(&values.headers["X-Cmd"]), "!credential");
		assert!(!format!("{values:?}").contains("sentinel-secret-value"));
		let key = |name: &str| Str::from(name);
		assert_eq!(values.notices, [
			LiteralValueNotice::Command { section: ValueSection::Env, key: key("K") },
			LiteralValueNotice::EnvironmentName { section: ValueSection::Env, key: key("SECRET") },
			LiteralValueNotice::EnvironmentName {
				section: ValueSection::Headers,
				key:     key("Authorization"),
			},
			LiteralValueNotice::Command { section: ValueSection::Headers, key: key("X-Cmd") },
		]);
	}

	#[tokio::test]
	async fn user_level_values_still_resolve_dynamically() {
		let executor = Arc::new(Executor { calls: AtomicUsize::new(0) });
		let resolver = CommandCredentialResolver::new(executor.clone(), Duration::from_millis(50));
		let values = resolve_transport_values(
			&hostile_config(),
			&sentinel_environment(),
			Some(&resolver),
			&CancellationToken::new(),
			ValueResolution::for_source(ConfigSourceKind::User),
		)
		.await
		.expect("resolve");
		// The shared resolver caches the identical command, so it runs at least once.
		assert!(executor.calls.load(Ordering::SeqCst) >= 1);
		let exposed = |value: &ResolvedConfigValue| value.with_exposed(str::to_owned);
		assert_eq!(exposed(&values.env["K"]), "secret-output");
		assert_eq!(exposed(&values.env["SECRET"]), "sentinel-secret-value");
		assert_eq!(exposed(&values.headers["Authorization"]), "sentinel-secret-value");
		assert!(values.notices.is_empty());
	}

	#[test]
	fn exactly_the_project_scoped_kinds_resolve_literally() {
		use ConfigSourceKind::*;
		for kind in [
			Project,
			Root,
			ClaudeProject,
			AgentPluginProject,
			ClaudePluginProject,
			CodexProject,
			GeminiProject,
			OpenCodeProject,
			CursorProject,
			WindsurfProject,
			VsCodeProject,
			StandaloneProject,
		] {
			assert_eq!(ValueResolution::for_source(kind), ValueResolution::Literal, "{kind}");
		}
		for kind in [
			User,
			Manifest,
			ClaudeUser,
			AgentPluginUser,
			AgentPluginExplicit,
			ClaudePluginUser,
			CodexUser,
			GeminiUser,
			OpenCodeUser,
			CursorUser,
			WindsurfUser,
		] {
			assert_eq!(ValueResolution::for_source(kind), ValueResolution::Dynamic, "{kind}");
		}
	}
}
