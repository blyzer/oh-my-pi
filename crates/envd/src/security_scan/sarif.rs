use std::{
	collections::{BTreeMap, BTreeSet},
	path::{Path, PathBuf},
};

use omp_core::{Hash32, Str, Ulid};
use omp_tools::security_scan::{Fault, TargetKind, ValidationStatus};
use serde_json::{Value, json};
use url::Url;

use super::{
	model::{Evidence, Finding, Location, Producer, Provenance, Scan, Validation},
	now_stamp,
};

pub(super) fn import(input: Value, root: &Path, source: &Path) -> Result<Scan, Fault> {
	if input.get("version").and_then(Value::as_str) != Some("2.1.0") {
		return Err(Fault::InvalidSarif);
	}
	let runs = input
		.get("runs")
		.and_then(Value::as_array)
		.ok_or(Fault::InvalidSarif)?;
	let canonical_root = root.canonicalize().map_err(|_| Fault::Storage)?;
	let root_url = Url::from_directory_path(&canonical_root).map_err(|()| Fault::InvalidSarif)?;
	let scan_id = Str::new(format!("secscan_{}", Ulid::generate()));
	let created = now_stamp();
	let mut findings = Vec::new();
	let mut seen = BTreeSet::new();
	let mut producer_name = Str::new_static("SARIF importer");
	let mut producer_version = None;
	for run in runs {
		let driver = run.pointer("/tool/driver").and_then(Value::as_object);
		if let Some(name) = driver
			.and_then(|driver| driver.get("name"))
			.and_then(Value::as_str)
		{
			producer_name = Str::new(name);
		}
		producer_version = driver
			.and_then(|driver| driver.get("version"))
			.and_then(Value::as_str)
			.map(Str::new);
		let mut rules = BTreeMap::new();
		if let Some(entries) = driver
			.and_then(|driver| driver.get("rules"))
			.and_then(Value::as_array)
		{
			for rule in entries {
				if let Some(id) = rule.get("id").and_then(Value::as_str) {
					rules.insert(id, rule);
				}
			}
		}
		for (ordinal, result) in run
			.get("results")
			.and_then(Value::as_array)
			.into_iter()
			.flatten()
			.enumerate()
		{
			let rule_id = result
				.get("ruleId")
				.and_then(Value::as_str)
				.unwrap_or("sarif.unknown");
			let rule = rules.get(rule_id).copied();
			let summary = result
				.pointer("/message/text")
				.or_else(|| result.pointer("/message/markdown"))
				.and_then(Value::as_str)
				.unwrap_or(rule_id);
			let title = rule
				.and_then(|rule| rule.pointer("/shortDescription/text"))
				.or_else(|| rule.and_then(|rule| rule.get("name")))
				.and_then(Value::as_str)
				.unwrap_or(rule_id);
			let locations = import_locations(result, run, &root_url, &canonical_root);
			let vendor_fingerprints =
				string_map(result.get("fingerprints"), result.get("partialFingerprints"));
			let category = result
				.pointer("/properties/category")
				.and_then(Value::as_str)
				.unwrap_or_else(|| rule_id.split(['.', '/', '-']).next().unwrap_or("security"));
			let anchor = vendor_fingerprints
				.values()
				.next()
				.cloned()
				.unwrap_or_else(|| Str::new(format!("{rule_id}:{summary}:{ordinal}")));
			let fingerprint = fingerprint(rule_id, category, &anchor, &locations)?;
			if !seen.insert(fingerprint.clone()) {
				continue;
			}
			let digest = Hash32::sum(fingerprint.as_bytes()).to_hex();
			let tags = rule
				.and_then(|rule| rule.pointer("/properties/tags"))
				.and_then(Value::as_array)
				.map(|tags| {
					tags
						.iter()
						.filter_map(Value::as_str)
						.map(Str::new)
						.collect::<Vec<_>>()
				})
				.unwrap_or_default();
			let cwe = tags
				.iter()
				.filter(|tag| {
					tag.get(..4)
						.is_some_and(|prefix| prefix.eq_ignore_ascii_case("cwe-"))
				})
				.cloned()
				.collect();
			let producer = Producer {
				kind:    Str::new_static("sarif-import"),
				name:    producer_name.clone(),
				version: producer_version.clone(),
				vendor:  None,
			};
			let provenance = Provenance {
				producer: producer.clone(),
				created_at: created.clone(),
				imported_at: Some(now_stamp()),
				source_ids: BTreeMap::from([(
					Str::new_static("source_path"),
					Str::new(source.to_string_lossy()),
				)]),
				vendor_fingerprints,
				metadata: BTreeMap::new(),
			};
			let severity = severity(result);
			let validation = canonical_validation(result.pointer("/properties/validation"));
			let disposition = canonical_disposition(result.pointer("/properties/disposition"));
			findings.push(Finding {
				id: Str::new(format!("secf_{}", &digest[..24])),
				scan_id: scan_id.clone(),
				fingerprint,
				rule_id: Str::new(rule_id),
				title: Str::new(title),
				summary: Str::new(summary),
				severity,
				confidence: Str::new_static("medium"),
				category: Str::new(category),
				cwe,
				locations,
				evidence: Vec::<Evidence>::new(),
				remediation: None,
				validation: Validation {
					status:       serde_json::from_value(Value::String(validation.to_owned()))
						.unwrap_or(ValidationStatus::Unvalidated),
					summary:      None,
					evidence_ids: Vec::new(),
					validated_at: None,
				},
				disposition: Str::new_static(disposition),
				provenance,
			});
		}
	}
	let producer = Producer {
		kind:    Str::new_static("sarif-import"),
		name:    producer_name,
		version: producer_version,
		vendor:  None,
	};
	let provenance = Provenance {
		producer:            producer.clone(),
		created_at:          created.clone(),
		imported_at:         Some(now_stamp()),
		source_ids:          BTreeMap::from([(
			Str::new_static("source_path"),
			Str::new(source.to_string_lossy()),
		)]),
		vendor_fingerprints: BTreeMap::new(),
		metadata:            BTreeMap::new(),
	};
	Ok(Scan {
		id: scan_id,
		plan_id: None,
		status: Str::new_static("completed"),
		created_at: created.clone(),
		completed_at: Some(created),
		target: TargetKind::Repository,
		producer,
		provenance,
		report: Some(Str::new(format!(
			"# Imported SARIF security results\n\nFindings: {}\n",
			findings.len()
		))),
		findings,
		sarif: Some(input),
	})
}

pub(super) fn export(scan: &Scan, root: &Path) -> Value {
	let mut rules = BTreeMap::<&str, &Finding>::new();
	for finding in &scan.findings {
		rules.entry(&finding.rule_id).or_insert(finding);
	}
	let base = Url::from_directory_path(root)
		.ok()
		.map_or_else(|| Str::new_static("file:///"), |url| Str::new(url.as_str()));
	json!({
		"$schema": "https://json.schemastore.org/sarif-2.1.0.json",
		"version": "2.1.0",
		"runs": [{
			"tool": {"driver": {
				"name": scan.producer.name,
				"version": scan.producer.version,
				"informationUri": "https://omp.sh",
				"rules": rules.values().map(|finding| json!({
					"id": finding.rule_id,
					"name": finding.rule_id,
					"shortDescription": {"text": finding.title},
					"fullDescription": {"text": finding.summary},
					"properties": {"tags": finding.cwe},
				})).collect::<Vec<_>>()
			}},
			"results": scan.findings.iter().map(|finding| json!({
				"ruleId": finding.rule_id,
				"level": sarif_level(&finding.severity),
				"message": {"text": finding.summary},
				"locations": finding.locations.iter().map(|location| json!({
					"physicalLocation": {
						"artifactLocation": {"uri": location.path, "uriBaseId": "%SRCROOT%"},
						"region": {
							"startLine": location.start_line,
							"endLine": location.end_line,
							"startColumn": location.start_column,
							"endColumn": location.end_column,
						}
					}
				})).collect::<Vec<_>>(),
				"fingerprints": {"omp-security/v1": finding.fingerprint},
				"properties": {
					"findingId": finding.id,
					"confidence": finding.confidence,
					"validation": finding.validation.status,
					"disposition": finding.disposition,
					"category": finding.category,
				}
			})).collect::<Vec<_>>(),
			"originalUriBaseIds": {"%SRCROOT%": {"uri": base}},
		}]
	})
}

/// Imports each location of `result` that resolves to a file under `root`. One
/// that does not is dropped, never the log: rejecting the log would hide every
/// other finding in it. An absent `region.startLine` stays absent, and a result
/// with no resolvable location keeps none.
fn import_locations(result: &Value, run: &Value, root_url: &Url, root: &Path) -> Vec<Location> {
	result
		.get("locations")
		.and_then(Value::as_array)
		.into_iter()
		.flatten()
		.filter_map(|entry| {
			let path = resolve_path(entry, run, root_url, root)?;
			Some(Location {
				path:         Str::new(path.to_string_lossy().replace('\\', "/")),
				start_line:   entry
					.pointer("/physicalLocation/region/startLine")
					.and_then(Value::as_u64)
					.filter(|line| *line > 0),
				end_line:     entry
					.pointer("/physicalLocation/region/endLine")
					.and_then(Value::as_u64),
				start_column: entry
					.pointer("/physicalLocation/region/startColumn")
					.and_then(Value::as_u64),
				end_column:   entry
					.pointer("/physicalLocation/region/endColumn")
					.and_then(Value::as_u64),
				role:         Some(Str::new_static("primary")),
			})
		})
		.collect()
}

fn resolve_path(entry: &Value, run: &Value, root_url: &Url, root: &Path) -> Option<PathBuf> {
	let uri = entry
		.pointer("/physicalLocation/artifactLocation/uri")
		.and_then(Value::as_str)?
		.replace('\\', "/");
	let base_id = entry
		.pointer("/physicalLocation/artifactLocation/uriBaseId")
		.and_then(Value::as_str);
	let resolved = match base_id {
		None | Some("%SRCROOT%") => root_url.join(&uri),
		Some(base_id) => {
			let declared = run
				.pointer(&format!("/originalUriBaseIds/{}/uri", pointer_escape(base_id)))
				.and_then(Value::as_str)?;
			root_url.join(declared).and_then(|base| base.join(&uri))
		},
	}
	.ok()?;
	if resolved.scheme() != "file" {
		return None;
	}
	normalize_candidate(&resolved.to_file_path().ok()?, root).ok()
}

fn normalize_candidate(candidate: &Path, root: &Path) -> Result<PathBuf, Fault> {
	let absolute = if candidate.is_absolute() {
		candidate.to_path_buf()
	} else {
		root.join(candidate)
	};
	let canonical = if absolute.exists() {
		absolute.canonicalize().map_err(|_| Fault::InvalidSarif)?
	} else {
		absolute
	};
	canonical
		.strip_prefix(root)
		.map(Path::to_path_buf)
		.map_err(|_| Fault::InvalidSarif)
}

fn string_map(first: Option<&Value>, second: Option<&Value>) -> BTreeMap<Str, Str> {
	first
		.into_iter()
		.chain(second)
		.filter_map(Value::as_object)
		.flat_map(|values| values.iter())
		.filter_map(|(key, value)| value.as_str().map(|value| (Str::new(key), Str::new(value))))
		.collect()
}

fn fingerprint(
	rule: &str,
	category: &str,
	anchor: &str,
	locations: &[Location],
) -> Result<Str, Fault> {
	let bytes = serde_json::to_vec(&json!({"rule_id": rule.to_lowercase(), "category": category.to_lowercase(), "anchor": anchor.to_lowercase(), "locations": locations})).map_err(|_| Fault::Storage)?;
	Ok(Str::new(format!("omp-security/v1:sha256:{}", Hash32::sum(&bytes).to_hex())))
}

fn severity(result: &Value) -> Str {
	if let Some(score) = result
		.pointer("/properties/security-severity")
		.and_then(Value::as_f64)
	{
		return Str::new_static(if score >= 9.0 {
			"critical"
		} else if score >= 7.0 {
			"high"
		} else if score >= 4.0 {
			"medium"
		} else if score > 0.0 {
			"low"
		} else {
			"informational"
		});
	}
	Str::new_static(match result.get("level").and_then(Value::as_str) {
		Some("error") => "high",
		Some("warning") => "medium",
		Some("note") => "low",
		_ => "informational",
	})
}

fn sarif_level(severity: &str) -> &'static str {
	match severity {
		"critical" | "high" => "error",
		"medium" => "warning",
		"low" => "note",
		_ => "none",
	}
}

fn canonical_validation(value: Option<&Value>) -> &'static str {
	match value.and_then(Value::as_str) {
		Some("validated") => "validated",
		Some("rejected") => "rejected",
		Some("partial") => "partial",
		Some("error") => "error",
		_ => "unvalidated",
	}
}

fn canonical_disposition(value: Option<&Value>) -> &'static str {
	match value.and_then(Value::as_str) {
		Some("false_positive") => "false_positive",
		Some("accepted_risk") => "accepted_risk",
		Some("fixed") => "fixed",
		Some("wont_fix") => "wont_fix",
		_ => "open",
	}
}

fn pointer_escape(value: &str) -> String {
	value.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
	use std::{fs, path::PathBuf};

	use omp_core::Ulid;
	use serde_json::{Value, json};

	use super::import;

	const BIOME_UNREADABLE: &str = include_str!("../../../../fixtures/sarif/biome-unreadable.sarif");
	const RUFF_OUTSIDE: &str = include_str!("../../../../fixtures/sarif/ruff-outside.sarif");
	/// The scratch directory every fixture was recorded in
	/// (`fixtures/sarif/PROVENANCE.txt`).
	const RECORDED: &str = "/private/tmp/sarif-work";

	/// Points the recorded absolute paths at a fresh scratch directory, so the
	/// log resolves on any host, and returns the canonical project root inside
	/// it.
	fn relocated(log: &str, project: &str) -> (Value, PathBuf, PathBuf) {
		let scratch = std::env::temp_dir().join(format!("omp-sarif-{}", Ulid::generate()));
		fs::create_dir_all(scratch.join(project)).expect("project root");
		let scratch = scratch.canonicalize().expect("canonical scratch");
		let log = log.replace(RECORDED, &scratch.to_string_lossy());
		(serde_json::from_str(&log).expect("SARIF log"), scratch.join(project), scratch)
	}

	fn imported(log: &str, project: &str) -> Vec<Value> {
		let (log, root, scratch) = relocated(log, project);
		let scan = import(log, &root, &root.join("results.sarif"));
		fs::remove_dir_all(scratch).expect("remove scratch");
		scan
			.expect("import")
			.findings
			.iter()
			.map(|finding| {
				let locations = finding
					.locations
					.iter()
					.map(|location| json!({"path": location.path, "line": location.start_line}))
					.collect::<Vec<_>>();
				json!({"rule": finding.rule_id, "locations": locations})
			})
			.collect()
	}

	#[test]
	fn line_less_location_keeps_its_file_and_no_line() {
		assert_eq!(imported(BIOME_UNREADABLE, "biome"), vec![
			json!({"rule": "lint/correctness/noUnusedVariables", "locations": [{"path": "outcomes.js", "line": 2}]}),
			json!({"rule": "internalError/io", "locations": [{"path": "missing.js", "line": null}]}),
			json!({"rule": "lint/suspicious/noDoubleEquals", "locations": [{"path": "outcomes.js", "line": 3}]}),
			json!({"rule": "lint/suspicious/noDebugger", "locations": [{"path": "outcomes.js", "line": 4}]}),
		]);
	}

	#[test]
	fn location_outside_root_drops_only_that_location() {
		assert_eq!(imported(RUFF_OUTSIDE, "ruff"), vec![
			json!({"rule": "F401", "locations": [{"path": "outcomes.py", "line": 1}]}),
			json!({"rule": "F401", "locations": [{"path": "outcomes.py", "line": 2}]}),
			json!({"rule": "F821", "locations": [{"path": "outcomes.py", "line": 6}]}),
			json!({"rule": "F841", "locations": [{"path": "outcomes.py", "line": 10}]}),
			json!({"rule": "F401", "locations": []}),
		]);
	}
}
