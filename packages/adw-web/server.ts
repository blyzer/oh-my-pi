/**
 * Trace viewer server.
 *
 * One process: Bun bundles and serves `index.html` on `/`, and the API decodes
 * runs through the native reader. Decoding cannot move into the page — the
 * N-API addon does not load in a browser — so this is the boundary where the
 * binary log becomes JSON.
 *
 *   bun server.ts                 # http://localhost:4600
 *   ADW_RUNS_DIR=… bun server.ts  # point at another machine's runs
 */

import index from "./index.html";
import { listRuns, readRun, runsRoot } from "./src/trace";

const port = Number(process.env.PORT ?? 4600);

const server = Bun.serve({
	port,
	development: process.env.NODE_ENV !== "production",
	routes: {
		"/": index,
		"/api/runs": () => Response.json(listRuns()),
		"/api/runs/:id": request => {
			const run = readRun(request.params.id);
			return run ? Response.json(run) : new Response("no such run", { status: 404 });
		},
	},
	fetch: () => new Response("not found", { status: 404 }),
});

console.log(`adw traces: ${server.url}\nreading:    ${runsRoot()}`);
