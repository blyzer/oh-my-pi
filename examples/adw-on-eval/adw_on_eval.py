"""plan -> workpool fan-out -> gates -> retry, as one omp2 eval cell.

Run it inside the omp2 `eval` tool (Python kernel): `%run examples/adw-on-eval/adw_on_eval.py`.
It uses only prelude helpers that a production session binds: `phase`, `log`, `workpool`, `read`, `output`.
`agent()`, `completion()` and `judge` are deliberately NOT used: the production parent host binds
only `__workpool__` (`WorkpoolSessionHost` reports the others unavailable), so even the plan step
goes through a pool. README.md says what this needs and what it does not guarantee.

The task is deliberately tiny: write small Python modules, each starting with a header comment,
and gate every module on a deterministic check. The same task is declared for `omp adw` in
`greet-files.toml`.
"""

import json
import re
import time

MODULES = {  # file -> function the module must define
	"greet.py": "greet(name) returning 'Hello, ' + name",
	"farewell.py": "farewell(name) returning 'Goodbye, ' + name",
}
MAX_ATTEMPTS = 3  # per file, matching `omp adw run --max-attempts` (default 3)
AGENT = "task"
POLL_SECONDS = 2
MERGE_GRACE_SECONDS = 90  # a worker's isolated worktree is merged when the worker retires
HEADER = re.compile(r"^# (?P<name>[A-Za-z0-9_.]+): \S")


def run_items(label, texts):
	"""Run each text as one pool item and return {item id: yielded data}, in push order.

	A pool closes itself when its items settle, so each call makes its own pool. The pool has no
	blocking wait: `status()` is polled. Each item yields JSON through the batch-local `yield`
	tool; the settled batch output is that JSON, retained as an `artifact://` address that
	`peek()` reports and `output()` resolves. An item that failed has no data and is simply absent
	from the result.
	"""
	pool = workpool(AGENT, name=label, context="Edit only the file named in your item.")
	try:
		ids = pool.push(*texts)
		while True:
			status = pool.status()
			counts = status["items"]
			if status["closed"] and counts["queued"] + counts["running"] == 0:
				break
			time.sleep(POLL_SECONDS)
		results = {}
		for batch in pool.peek()["batches"]:
			if batch["status"] != "completed" or not batch.get("output"):
				log(f"{label}: batch {batch['id']} {batch['status']}")
				continue
			results.update(json.loads(output(batch["output"])))
		return {item_id: results[item_id] for item_id in ids if item_id in results}
	finally:
		pool.close()  # drops anything still queued if the cell is interrupted mid-round


def gate(path):
	"""Deterministic check for one file: None when it passes, else the feedback text.

	It runs in this kernel with the kernel's own authority. An `omp adw` code phase would run
	the same check as a declared argv under the environment's exec sandbox instead.
	"""
	try:
		text = read(path)
	except Exception as error:  # a missing file is a gate failure, not a script failure
		return f"{path} does not exist yet or could not be read: {error}"
	first = text.splitlines()[0] if text.strip() else ""
	match = HEADER.match(first)
	if not match or match.group("name") != path:
		return f"first line of {path} must look like '# {path}: <purpose>' but is {first!r}"
	return None


def gate_with_merge_grace(path):
	"""`gate`, but a missing file is given time to arrive.

	The pool reports `closed` before the retiring worker's patch is applied to the project
	(`finish` sets it first; each worker merges its worktree when it leaves its loop), so a
	missing file right after the pool settles is not yet a verdict.
	"""
	deadline = time.monotonic() + MERGE_GRACE_SECONDS
	while True:
		feedback = gate(path)
		if feedback is None or "does not exist" not in feedback or time.monotonic() >= deadline:
			return feedback
		time.sleep(POLL_SECONDS)


def item_text(path, spec, feedback):
	"""The pool item for one file; the failed gate's feedback rides in the item text."""
	text = (
		f"Create `{path}` in the project root. Its first line is a one-line header comment of the "
		f"form '# {path}: <purpose>'. It then defines {spec}. Touch no other file. "
		f'Submit data {{"file": "{path}"}} when it exists.'
	)
	if feedback:
		text += f"\nYour previous attempt was rejected by a gate. Fix exactly this:\n{feedback}"
	return text


phase("plan")
planned = None
for data in run_items(
	"adw-plan",
	[
		"Do not edit any file. List the module files to write for this task. Submit data "
		'{"files": ["<file>", ...]} naming only files from this list. The task: '
		+ json.dumps(MODULES)
	],
).values():
	planned = data.get("files") if isinstance(data, dict) else None
unknown = sorted(set(planned or []) - set(MODULES))
if not planned or unknown:
	# The plan is advice, not authority: the file set is fixed above, so a plan naming anything
	# else is rejected instead of being fanned out.
	raise SystemExit(f"plan rejected: {planned!r} (unknown files: {unknown})")
log(f"plan: {planned}")

pending = {path: "" for path in planned}  # file -> feedback from its last failed gate
attempts = {path: 0 for path in planned}
settled = {}  # file -> "accepted" | the gate's last feedback
round_number = 0

while pending:
	round_number += 1
	round_files = sorted(pending)
	for path in round_files:
		attempts[path] += 1
	phase(f"implement round {round_number}: {round_files}")
	run_items(
		f"adw-write-r{round_number}",
		[item_text(path, MODULES[path], pending[path]) for path in round_files],
	)

	phase(f"gate round {round_number}")
	pending = {}
	for path in round_files:
		feedback = gate_with_merge_grace(path)
		if feedback is None:
			settled[path] = "accepted"
			log(f"{path}: accepted on attempt {attempts[path]}")
		elif attempts[path] >= MAX_ATTEMPTS:
			settled[path] = feedback
			log(f"{path}: halted after {attempts[path]} attempts: {feedback}")
		else:
			pending[path] = feedback
			log(f"{path}: rejected on attempt {attempts[path]}: {feedback}")

accepted = all(value == "accepted" for value in settled.values())
print(json.dumps({"accepted": accepted, "attempts": attempts, "settled": settled}, indent=2))
if not accepted:
	raise SystemExit("workflow halted: at least one file never passed its gate")
