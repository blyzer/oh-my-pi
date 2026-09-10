/**
 * The registered FactoryBench scenarios.
 *
 * Each one drives the real engine — `runGraph`, the real gates, the real
 * journal — and asserts a property that must survive any future
 * optimization. They are written against observable outcomes, never against
 * internal call sequences, so a rewrite that preserves the safety property
 * keeps passing and one that quietly drops it does not.
 */
import * as fs from "node:fs/promises";
import path from "node:path";
import { classifyFailure } from "@oh-my-pi/pi-coding-agent/task/admission";
import { defineScenario, type ScenarioOutcome } from "./bench";
import { type GraphPhase, runGraph } from "./graph";
import { copyIsolation } from "./isolation";
import { replay } from "./ledger";

function ok(evidence: string): ScenarioOutcome {
	return { passed: true, evidence };
}

function no(evidence: string): ScenarioOutcome {
	return { passed: false, evidence };
}

/** A phase that writes what it is told and declares it honestly. */
function writer(name: string, file: string, body: string, extra: Partial<GraphPhase> = {}): GraphPhase {
	return {
		name,
		scope: ["**"],
		assertions: [],
		maxAttempts: 1,
		writes: true,
		produce: async ({ workspace }) => {
			await Bun.write(path.join(workspace, file), body);
			return { changedFiles: [file], declaredArtifacts: [file], label: name, exitCode: 0 };
		},
		...extra,
	};
}

export const FAIL_CLOSED_001 = defineScenario({
	id: "FB-FAILCLOSED-001",
	family: "FB-FAILCLOSED",
	canonical: true,
	asserts:
		"A phase whose acceptance requires a file to both contain and not contain the same marker " +
		"can never be accepted, however the producer behaves.",
	run: async workdir => {
		const root = path.join(workdir, "tree");
		const runDir = path.join(workdir, "run");
		await fs.mkdir(root, { recursive: true });
		await fs.mkdir(runDir, { recursive: true });

		const marker = "__FACTORY_FAIL_CLOSED_CONTRADICTION_0024__";
		// The producer is maximally cooperative: it writes the marker, claims
		// success, and declares exactly what it touched. Nothing about its
		// behaviour is the reason this must fail.
		const result = await runGraph({
			workflowId: "fb-failclosed-001",
			runDir,
			workspace: root,
			allowUnguardedWrites: true,
			phases: [
				writer("build", "marker.txt", `${marker}\n`, {
					assertions: [
						{ type: "file_contains", file: "marker.txt", marker },
						{ type: "file_not_contains", file: "marker.txt", marker },
					],
				}),
			],
		});

		if (result.status !== "failed") return no(`run status was ${result.status}, expected failed`);
		const build = result.phases.find(phase => phase.phase === "build");
		if (build?.status !== "rejected") return no(`build status was ${build?.status}, expected rejected`);

		// Durable truth must agree: an accepted projection with a rejected
		// phase would mean the ledger and the run disagree about the same run.
		const { projection } = await replay(runDir);
		if (projection.status === "accepted") return no("ledger projection reported accepted");
		return ok(`rejected with: ${build.evidence.at(-1) ?? "no evidence"}`);
	},
});

export const SCOPE_001 = defineScenario({
	id: "FB-SCOPE-001",
	family: "FB-SCOPE",
	asserts: "A write outside the declared scope is rejected on the actual change-set, not on the producer's claim.",
	run: async workdir => {
		const root = path.join(workdir, "tree");
		const runDir = path.join(workdir, "run");
		await fs.mkdir(path.join(root, "src"), { recursive: true });
		await fs.mkdir(runDir, { recursive: true });

		const result = await runGraph({
			workflowId: "fb-scope-001",
			runDir,
			workspace: root,
			allowUnguardedWrites: true,
			phases: [
				{
					name: "build",
					scope: ["src/**"],
					assertions: [],
					maxAttempts: 1,
					writes: true,
					// Declares only the in-scope file while actually touching
					// both: the claim is honest-looking, the change-set is not.
					produce: async ({ workspace }) => {
						await Bun.write(path.join(workspace, "src", "ok.ts"), "export const ok = 1;\n");
						await Bun.write(path.join(workspace, "outside.ts"), "export const sneaky = 1;\n");
						return {
							changedFiles: ["src/ok.ts", "outside.ts"],
							declaredArtifacts: ["src/ok.ts"],
							label: "build",
							exitCode: 0,
						};
					},
				},
			],
		});

		if (result.status !== "failed") return no(`run status was ${result.status}, expected failed`);
		const evidence = result.phases[0]?.evidence.join(" ") ?? "";
		if (!evidence.includes("outside.ts")) return no(`rejection did not name the out-of-scope path: ${evidence}`);
		return ok(evidence);
	},
});

export const DAG_001 = defineScenario({
	id: "FB-DAG-001",
	family: "FB-DAG",
	asserts:
		"A dependent phase does not run when its producer failed, while an unrelated branch still finishes — " +
		"readiness is an accepted version, not a completed run.",
	run: async workdir => {
		const root = path.join(workdir, "tree");
		const runDir = path.join(workdir, "run");
		await fs.mkdir(root, { recursive: true });
		await fs.mkdir(runDir, { recursive: true });
		const ran: string[] = [];

		const result = await runGraph({
			workflowId: "fb-dag-001",
			runDir,
			workspace: root,
			allowUnguardedWrites: true,
			phases: [
				{
					name: "producer",
					scope: ["**"],
					assertions: [],
					maxAttempts: 1,
					dependsOn: [],
					produce: async () => {
						ran.push("producer");
						return { changedFiles: [], label: "producer", exitCode: 1, output: "producer failed" };
					},
				},
				{
					name: "consumer",
					scope: ["**"],
					assertions: [],
					maxAttempts: 1,
					dependsOn: ["producer"],
					produce: async () => {
						ran.push("consumer");
						return { changedFiles: [], label: "consumer", exitCode: 0 };
					},
				},
				{
					name: "unrelated",
					scope: ["**"],
					assertions: [],
					maxAttempts: 1,
					dependsOn: [],
					produce: async () => {
						ran.push("unrelated");
						return { changedFiles: [], label: "unrelated", exitCode: 0 };
					},
				},
			],
		});

		if (ran.includes("consumer")) return no("consumer dispatched despite its producer failing");
		if (!ran.includes("unrelated")) return no("an unrelated branch was blocked by an unrelated failure");
		const consumer = result.phases.find(phase => phase.phase === "consumer");
		if (consumer?.status !== "blocked") return no(`consumer status was ${consumer?.status}, expected blocked`);
		return ok(`dispatched ${ran.join(", ")}; consumer blocked`);
	},
});

export const ISOLATION_001 = defineScenario({
	id: "FB-ISOLATION-001",
	family: "FB-ISOLATION",
	asserts:
		"Concurrent writers on the same file cannot both land; the loser is refused rather than silently clobbering.",
	run: async workdir => {
		const root = path.join(workdir, "tree");
		const runDir = path.join(workdir, "run");
		const journalDir = path.join(workdir, "journal");
		await fs.mkdir(root, { recursive: true });
		await fs.mkdir(runDir, { recursive: true });
		await fs.mkdir(journalDir, { recursive: true });
		await Bun.write(path.join(root, "shared.txt"), "original\n");

		const both = Promise.withResolvers<void>();
		let entered = 0;
		const racer = (name: string): GraphPhase => ({
			name,
			scope: ["**"],
			assertions: [],
			maxAttempts: 1,
			writes: true,
			dependsOn: [],
			produce: async ({ workspace }) => {
				entered += 1;
				if (entered === 2) both.resolve();
				await both.promise;
				await Bun.write(path.join(workspace, "shared.txt"), `${name}\n`);
				return {
					changedFiles: ["shared.txt"],
					declaredArtifacts: ["shared.txt"],
					label: name,
					exitCode: 0,
				};
			},
		});

		const result = await runGraph({
			workflowId: "fb-isolation-001",
			runDir,
			workspace: root,
			isolation: copyIsolation(),
			integrate: { journalDir, base: "fb-iso" },
			phases: [racer("alpha"), racer("beta")],
		});

		const landed = result.phases.filter(phase => phase.status === "accepted");
		if (landed.length !== 1) return no(`${landed.length} writers landed, expected exactly 1`);
		const delivered = await Bun.file(path.join(root, "shared.txt")).text();
		if (delivered !== `${landed[0]?.phase}\n`) {
			return no(`delivered tree holds ${JSON.stringify(delivered)}, not the accepted writer's content`);
		}
		return ok(`${landed[0]?.phase} landed; the other was refused`);
	},
});

export const PROVENANCE_001 = defineScenario({
	id: "FB-PROVENANCE-001",
	family: "FB-PROVENANCE",
	asserts: "An accepted consumer records the exact producer version it consumed.",
	run: async workdir => {
		const root = path.join(workdir, "tree");
		const runDir = path.join(workdir, "run");
		await fs.mkdir(root, { recursive: true });
		await fs.mkdir(runDir, { recursive: true });

		const result = await runGraph({
			workflowId: "fb-provenance-001",
			runDir,
			workspace: root,
			allowUnguardedWrites: true,
			phases: [
				writer("plan", "plan.md", "# plan\n", { dependsOn: [] }),
				{
					...writer("build", "build.txt", "built\n"),
					dependsOn: ["plan"],
					inputs: ["plan"],
				},
			],
		});

		if (result.status !== "accepted") return no(`run status was ${result.status}`);
		const build = result.phases.find(phase => phase.phase === "build");
		const consumed = build?.inputs.find(input => input.phase === "plan");
		if (!consumed) return no("build recorded no consumed input");
		if (consumed.version !== 1) return no(`consumed version was ${consumed.version}, expected 1`);
		if (!consumed.digest) return no("consumed input carried no digest");
		return ok(`build consumed plan v${consumed.version} (${consumed.digest})`);
	},
});

export const RESOURCE_001 = defineScenario({
	id: "FB-RESOURCE-001",
	family: "FB-RESOURCE",
	asserts:
		"A host failure ends the phase without spending the correction budget, while an ordinary " +
		"semantic failure still gets every attempt it was granted.",
	run: async workdir => {
		const root = path.join(workdir, "tree");
		await fs.mkdir(root, { recursive: true });

		// Same workflow, same budget, same producer shape. The only variable
		// is what the failure says, which is exactly the distinction under
		// test: who failed, the work or the machine.
		const attemptsFor = async (output: string, label: string): Promise<number> => {
			const runDir = path.join(workdir, `run-${label}`);
			await fs.mkdir(runDir, { recursive: true });
			let dispatched = 0;
			await runGraph({
				workflowId: `fb-resource-001-${label}`,
				runDir,
				workspace: root,
				allowUnguardedWrites: true,
				classifyFailure,
				phases: [
					{
						name: "build",
						scope: ["**"],
						assertions: [],
						maxAttempts: 3,
						produce: async () => {
							dispatched += 1;
							return { changedFiles: [], label: "build", exitCode: 1, output };
						},
					},
				],
			});
			return dispatched;
		};

		const semantic = await attemptsFor("expected 3 to equal 4", "semantic");
		if (semantic !== 3) return no(`a semantic failure used ${semantic} attempts, expected the full budget of 3`);

		const resource = await attemptsFor("fatal error: out of memory", "resource");
		if (resource !== 1) {
			return no(`an out-of-memory failure used ${resource} attempts; the budget was spent on an exhausted host`);
		}
		return ok(`semantic used ${semantic}/3 attempts, resource stopped after ${resource}`);
	},
});
