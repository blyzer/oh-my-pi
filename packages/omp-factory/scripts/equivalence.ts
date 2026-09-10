/**
 * WP-12 static equivalence probe: run the frozen ADW example workflows
 * (tag adw-prototype-reference) through the new DAG/readiness semantics and
 * report where observable scheduling behavior matches or diverges.
 */
import { buildWaves } from "../src/dag";

const TAG = "adw-prototype-reference";
const EXAMPLES = ["fix", "ship", "review", "sdlc"] as const;

interface AdwPhase {
	name: string;
	kind: string;
	dependsOn?: string[];
	gates?: string[];
}

async function show(ref: string, file: string): Promise<string> {
	const child = Bun.spawn(["git", "show", `${ref}:${file}`], { stdout: "pipe", stderr: "pipe" });
	const [text, stderr, code] = await Promise.all([
		new Response(child.stdout).text(),
		new Response(child.stderr).text(),
		child.exited,
	]);
	if (code !== 0) throw new Error(`git show failed: ${stderr}`);
	return text;
}

let mismatches = 0;

for (const example of EXAMPLES) {
	const raw = await show(TAG, `docs/adw/examples/${example}.yml`);
	const parsed = Bun.YAML.parse(raw) as { phases?: AdwPhase[] };
	const phases = parsed.phases ?? [];
	const names = phases.map(p => p.name);
	// ADW rule: no dependsOn => declaration predecessor; [] => independent.
	const deps: Record<string, string[]> = {};
	phases.forEach((phase, index) => {
		if (phase.dependsOn !== undefined) deps[phase.name] = [...phase.dependsOn];
		else deps[phase.name] = index === 0 ? [] : [names[index - 1] as string];
	});
	let waves: string[][];
	try {
		waves = buildWaves(names, deps);
	} catch (error) {
		console.log(`${example}: CYCLE/ERROR ${(error as Error).message}`);
		mismatches += 1;
		continue;
	}
	const scheduled = waves.flat();
	const complete = scheduled.length === names.length && names.every(name => scheduled.includes(name));
	// Every dependency must appear in a strictly earlier wave (acceptance order).
	const waveOf = new Map(waves.flatMap((wave, i) => wave.map(name => [name, i] as const)));
	const ordered = names.every(name =>
		(deps[name] ?? []).every(dep => (waveOf.get(dep) ?? 0) < (waveOf.get(name) ?? 0)),
	);
	const gates = [...new Set(phases.flatMap(p => p.gates ?? []))];
	console.log(
		`${example}: phases=${names.length} waves=${waves.map(w => `[${w.join(",")}]`).join("->")} scheduled-once=${complete} deps-earlier=${ordered} gates=${gates.join(",") || "none"}`,
	);
	if (!complete || !ordered) mismatches += 1;
}

console.log(mismatches === 0 ? "EQUIVALENCE-STATIC-OK" : `EQUIVALENCE-DIVERGENCES=${mismatches}`);
process.exit(mismatches === 0 ? 0 : 1);
