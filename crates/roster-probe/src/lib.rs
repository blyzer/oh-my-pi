//! Probe capability for the built-in construction table.
//!
//! [`register_probe`] is a [`omp_tools::roster::RosterExtension`]. Driver
//! selection passes it on [`omp_envd::RegistryBridges::selected_roster`].
//! envd installs it while walking the table and does not name the probe.

use futures::Stream;
use omp_core::Str;
use omp_tool::{
	Claims, Confinement, Constraint, Effects, Ev, IncomingParams, Part, Precedence, Presentation,
	PromptCaps, Registry, RegistryError, Rev, Tool, ToolSpec, schema,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Model-facing name of the probe. It is not a production identity.
pub const NAME: &str = "roster_probe";

/// Arguments of the probe. The presence proof never invokes it.
#[derive(Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Params {}

/// Durable success payload. Uninhabited progress and fault types keep the
/// tool out of any production path.
#[derive(Debug, Deserialize, Serialize)]
pub struct Payload {}

/// The probe emits no progress.
#[derive(Debug, Deserialize, Serialize)]
pub enum Update {}

/// The probe emits no fault. Calls are not part of the proof.
#[derive(Debug, Deserialize, Serialize)]
pub struct Fault {}

struct Probe {
	spec: ToolSpec,
}

impl Tool for Probe {
	type Fault = Fault;
	type Params = Params;
	type Payload = Payload;
	type Update = Update;

	fn spec(&self) -> &ToolSpec {
		&self.spec
	}

	fn call<'c>(
		&'c self,
		_params: IncomingParams<'c>,
	) -> impl Stream<Item = Ev<Self::Update, Self::Payload, Self::Fault>> + Send + 'c {
		futures::stream::empty()
	}

	fn prompt(&self, _view: Result<&Self::Payload, &Self::Fault>, _caps: &PromptCaps) -> Vec<Part> {
		Vec::new()
	}
}

/// Registers the probe on a session registry.
///
/// Pass this function to [`omp_tools::roster::SelectedRoster::with_extension`].
pub fn register_probe(registry: &mut Registry) -> Result<(), RegistryError> {
	registry.register(
		Probe {
			spec: ToolSpec {
				name:            Str::new_static(NAME),
				rev:             Rev { family: Str::new_static(""), n: 1 },
				description:     Str::new_static(
					"Test capability registered through the built-in roster extension.",
				),
				schema:          schema::<Params>(),
				constraint:      Constraint::None,
				effects:         Effects::empty(),
				confinement:     Confinement::Host,
				projection_code: omp_tool::native_projection_code(
					env!("CARGO_PKG_NAME"),
					env!("CARGO_PKG_VERSION"),
					include_bytes!("lib.rs"),
				)
				.into(),
			},
		},
		Presentation::Hidden,
		Claims {
			precedence: Precedence::ENHANCEMENT,
			claimant:   Str::new_static("omp/roster-probe"),
			replaces:   None,
		},
	)
}
