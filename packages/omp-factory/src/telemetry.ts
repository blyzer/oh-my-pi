/**
 * Telemetry (WP-11): derived only from verified outcomes. A cheap model with
 * many failed attempts must read as more expensive per acceptance, never cheaper.
 */

export interface AttemptRecord {
	workflow: string;
	phase: string;
	attempt: number;
	role: string;
	model: string;
	provider: string;
	durationMs: number;
	tokensIn: number;
	tokensOut: number;
	costUsd: number;
	gateFailures: number;
	corrections: number;
	outcome: "accepted" | "rejected";
}

export interface TelemetrySummary {
	workflows: number;
	acceptances: number;
	verifiedSuccessRate: number;
	costPerVerifiedSuccess: number | null;
	correctionsPerAcceptance: number | null;
	gateFailureRate: number;
	totalCostUsd: number;
	byModel: Record<string, { attempts: number; acceptances: number; costUsd: number }>;
}

export function summarize(records: AttemptRecord[]): TelemetrySummary {
	const byModel: TelemetrySummary["byModel"] = {};
	let acceptances = 0;
	let corrections = 0;
	let gateFailures = 0;
	let totalCostUsd = 0;
	const workflows = new Set<string>();
	for (const record of records) {
		workflows.add(record.workflow);
		totalCostUsd += record.costUsd;
		gateFailures += record.gateFailures;
		const entry = byModel[record.model] ?? { attempts: 0, acceptances: 0, costUsd: 0 };
		entry.attempts += 1;
		entry.costUsd += record.costUsd;
		if (record.outcome === "accepted") {
			acceptances += 1;
			corrections += record.corrections;
			entry.acceptances += 1;
		}
		byModel[record.model] = entry;
	}
	return {
		workflows: workflows.size,
		acceptances,
		verifiedSuccessRate: records.length === 0 ? 0 : acceptances / records.length,
		costPerVerifiedSuccess: acceptances === 0 ? null : totalCostUsd / acceptances,
		correctionsPerAcceptance: acceptances === 0 ? null : corrections / acceptances,
		gateFailureRate: records.length === 0 ? 0 : gateFailures / records.length,
		totalCostUsd,
		byModel,
	};
}
