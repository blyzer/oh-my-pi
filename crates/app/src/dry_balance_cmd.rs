//! Deterministic account-pool selection simulation and opt-in live benchmark.

use std::{collections::BTreeMap, sync, time::SystemTime};

use miette::{IntoDiagnostic as _, miette};
use omp_ai::account::{AccountPool, AccountSelectionRequest, AccountStateStore, RotationPolicy};
use omp_catalog::{ModelKey, snapshot::Catalog};
use omp_core::{Str, fast_hash64};
use serde_json::json;

use crate::{
	bench_cmd, cli,
	cli::{BenchArgs, DryBalanceArgs},
};

/// Simulates the canonical account pool and optionally benchmarks through its
/// normal credential/receipt path.
pub async fn run(args: DryBalanceArgs) -> miette::Result<()> {
	if args.count == 0 || args.concurrency == 0 {
		return Err(miette!("--count and --concurrency must be greater than zero"));
	}
	let data_dir = omp_core::dirs::data_dir(args.data_dir.clone()).into_diagnostic()?;
	let catalog = Catalog::try_embedded().map_err(|error| miette!(error.to_string()))?;
	let model = args
		.model
		.as_ref()
		.map_or_else(|| catalog.models().first(), |key| catalog.model(&ModelKey::from(key.clone())))
		.ok_or_else(|| miette!("model catalog is empty or the selected model is unknown"))?;
	let route = model
		.routes
		.first()
		.cloned()
		.ok_or_else(|| miette!("selected model has no eligible route"))?;
	let provider = catalog
		.route(&route)
		.map(|route| route.provider.clone())
		.ok_or_else(|| miette!("selected model route is absent from the catalog"))?;
	let pool = AccountPool::with_store(sync::Arc::new(
		AccountStateStore::open(data_dir.join("credentials.db")).into_diagnostic()?,
	))
	.into_diagnostic()?;
	let accounts = pool
		.accounts()
		.into_iter()
		.filter(|account| account.provider == provider && account.routes.contains(&route))
		.collect::<Vec<_>>();
	if accounts.is_empty() {
		return Err(miette!("provider `{}` has no eligible stored accounts", provider.as_str()));
	}
	let labels = account_labels(accounts.iter().map(|account| account.account.as_str()));
	let mut counts = BTreeMap::<String, u32>::new();
	let mut receipts = Vec::with_capacity(args.count as usize);
	for sample in 0..args.count {
		// Sample a fresh randomized session id for every attempt. Feed the same
		// distribution into the canonical pool by making the hashed
		// session bucket the preferred preceding account.
		let session_id = cli::turn_id();
		let bucket = fast_hash64(session_id.as_bytes()) as usize % accounts.len();
		let preferred = accounts[bucket].account.clone();
		let selection = pool
			.select(&AccountSelectionRequest {
				provider:           provider.clone(),
				route:              route.clone(),
				affinity:           None,
				previous_account:   Some(preferred),
				previous_principal: None,
				rotate:             false,
				rotation:           RotationPolicy::default(),
				now:                SystemTime::now(),
				quota_scope:        None,
				pin:                None,
			})
			.map_err(|error| miette!(error.to_string()))?;
		*counts
			.entry(labels[selection.record.account.as_str()].clone())
			.or_default() += 1;
		receipts.push(json!({
			"sample": sample,
			"sessionId": session_id,
			"account": labels[selection.record.account.as_str()],
			"candidateCount": selection.receipt.candidates.len(),
		}));
	}
	if args.json {
		println!(
			"{}",
			serde_json::to_string_pretty(&json!({
				"model": model.key,
				"provider": provider,
				"route": route,
				"counts": counts,
				"receipts": receipts,
			}))
			.into_diagnostic()?
		);
	} else {
		for (account, count) in counts {
			println!("{account} {count}");
		}
	}
	if args.bench {
		bench_cmd::run(BenchArgs {
			model:         model.key.as_str().into(),
			data_dir:      args.data_dir,
			runs:          Some(args.count),
			max_tokens:    Some(512),
			prompt:        Some(Str::new_static("Reply with the word ready.")),
			profile:       crate::cli::BenchProfile::Chat,
			prefill_bytes: None,
			par:           args.concurrency,
			json:          args.json,
		})
		.await?;
	}
	Ok(())
}

fn account_labels<'a>(accounts: impl Iterator<Item = &'a str>) -> BTreeMap<&'a str, String> {
	accounts
		.collect::<std::collections::BTreeSet<_>>()
		.into_iter()
		.enumerate()
		.map(|(index, account)| (account, format!("account-{}", index + 1)))
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn balance_labels_are_stable_private_and_collision_free() {
		let accounts = ["short-a", "short-b", "same:first:last", "same:other:last"];
		let labels = account_labels(accounts.iter().copied());
		assert_eq!(labels, account_labels(accounts.iter().rev().copied()));
		assert_eq!(
			labels
				.values()
				.collect::<std::collections::BTreeSet<_>>()
				.len(),
			accounts.len()
		);
		let json = serde_json::to_string(&labels.values().collect::<Vec<_>>()).unwrap();
		for account in accounts {
			assert!(!json.contains(account));
		}
	}
}
