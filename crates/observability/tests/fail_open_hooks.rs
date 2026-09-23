//! Fail-open telemetry hooks, called from outside `omp-observability`.
//!
//! [`TelemetryConfig::estimate_cost`] is generic over its catalog fallback, so
//! the method, and the `catch_unwind` inside it, is instantiated in the calling
//! crate rather than in `omp-observability`. A caller compiled without landing
//! pads (a Cranelift dev build) therefore does not contain an estimator panic;
//! see `docs/audits/cranelift-panic-cleanup.md` §4. This package builds with
//! LLVM in every profile, so these tests pin the contained behavior, not the
//! backend choice.

use std::sync::Arc;

use omp_observability::config::{
	ChatUsageSnapshot, CostEstimate, CostEstimatorContext, TelemetryConfig, TelemetryWarningCode,
};
use parking_lot::Mutex;

#[test]
fn panicking_estimator_falls_back_to_the_catalog_price_and_warns() {
	let warnings = Arc::new(Mutex::new(Vec::new()));
	let capture = Arc::clone(&warnings);
	let config = TelemetryConfig {
		cost_estimator: Some(Arc::new(|_| panic!("pricing service exploded"))),
		on_telemetry_warning: Some(Arc::new(move |warning| {
			capture
				.lock()
				.push((warning.code, warning.error.as_deref().map(str::to_owned)));
			Ok(())
		})),
		..TelemetryConfig::default()
	};
	let input = CostEstimatorContext {
		provider:     "anthropic",
		model:        "model",
		service_tier: None,
		usage:        ChatUsageSnapshot::default(),
	};
	let catalog = CostEstimate::Available { usd: 1.0, input_usd: None, output_usd: None };

	let estimate = config.estimate_cost(&input, |_| Some(catalog.clone()));

	assert_eq!(estimate, Some(catalog));
	assert_eq!(*warnings.lock(), vec![(
		TelemetryWarningCode::CostEstimatorFailed,
		Some("pricing service exploded".to_owned())
	)]);
}
