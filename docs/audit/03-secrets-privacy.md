# 03 — Secrets, Credentials & PII on the Outbound Path

Scope: `/Users/enverfrancisco/repositories/oh-my-pi`. Read-only audit. Every claim is marked
OBSERVED (code read) or MISSING (searched, does not exist). Documentation and comments are
not treated as evidence; where a doc claims something the code does not do, it is reported
as a discrepancy (Finding 11).

---

## Bottom line

**Secrets are NOT reliably prevented from leaving the machine.**

The repository contains a genuine, carefully built outbound redaction subsystem
(`packages/coding-agent/src/secrets/` plus a pattern redactor in `packages/ai`). It is not a
fiction and it is not a stub. But three properties, all OBSERVED, mean it cannot be relied
on as a privacy boundary:

1. **It is OFF by default.** `secrets.enabled` defaults to `false`
   (`settings-schema.ts:5478-5481`). With stock config `obfuscator` is `undefined`
   (`sdk.ts:1481-1483`) and pi-ai's `credentialRedactionEnabled` stays `false`
   (`transform-messages.ts:444`), so **both** filters are literal pass-throughs. Out of the
   box, a `.env`, an SSH private key, a PEM file or an AWS credential read by the model is
   transmitted byte-for-byte to the provider.
2. **Even when ON, content-derived coverage is an allowlist of six vendor token prefixes.**
   The sole content detector is `SENSITIVE_TOKEN_RE` (`transform-messages.ts:418-419`):
   GitHub, GitLab and OpenAI/Anthropic key shapes. There is **no** detector for PEM blocks,
   SSH private keys, AWS access keys, JWTs, database URLs, or generic high-entropy strings.
3. **Every uncertainty resolves toward transmitting.** Invalid regex, unparseable
   `secrets.yml`, low-entropy match, short value — each returns the original text. There is
   no fail-closed branch anywhere on the outbound path.

A permission prompt is not a substitute, and by default there is not even a prompt:
`tools.approvalMode` defaults to `"yolo"` (`settings-schema.ts:4223-4226`).

---

## Part 1 — Inventory of mechanisms

| # | Mechanism | Stage | DETECTS | WARNS | REDACTS | BLOCKS | Default |
|---|-----------|-------|:-------:|:-----:|:-------:|:------:|---------|
| M1 | `SecretObfuscator` (reversible keyed placeholders) | host -> provider Context | yes | no | yes | **no** | **off** |
| M2 | `redactSensitiveCredentials` (pi-ai pattern redactor) | provider wire (`transformMessages`) | yes | no | yes | **no** | **off** |
| M3 | Advisor transcript obfuscation | primary -> advisor model | yes | no | yes | no | follows M1 |
| M4 | `/share` snapshot redaction | session -> share server | yes | no | yes | no | on, gated on M1 |
| M5 | MCP error sanitizer | MCP error -> log / diagnostic | yes | no | yes | no | on |
| M6 | Blob-broker header/query stripping | outbound file-delete callbacks | yes | no | yes | no | on |
| M7 | `redactUrlCredentials` | browser tool transcript | yes | no | yes | no | on |
| M8 | `usage-cli` account masking | local terminal render | n/a | n/a | yes | no | on |

Only **M1 and M2 sit on the LLM outbound path**. M5-M8 protect logs, diagnostics and local
rendering; they are not privacy boundaries for model context.

---

### Finding 1 — The outbound secret filter exists and is correctly wired, but defaults to OFF

**Evidence:**

- `packages/coding-agent/src/config/settings-schema.ts:5478-5481` — `"secrets.enabled"`,
  `type: "boolean"`, `default: false`.
- `packages/coding-agent/src/sdk.ts:1481-1484` — in `createAgentSession`:
  `const obfuscator: SecretObfuscator | undefined = settings.get("secrets.enabled") ? await buildSecretObfuscator(cwd, agentDir, options.agentDir) : undefined;`
- `packages/coding-agent/src/config/settings.ts:3112-3114` — the same key drives
  `configureCredentialRedaction(value === true)`, the pi-ai module toggle.
- `packages/ai/src/providers/transform-messages.ts:444` —
  `let credentialRedactionEnabled = false;` (module-level default).
- `packages/ai/src/providers/transform-messages.ts:457` —
  `if (!credentialRedactionEnabled) return text;`
- `packages/ai/src/providers/transform-messages.ts:506` —
  `if (!credentialRedactionEnabled) return messages;`
- MISSING: no config preset, profile, or bootstrap path sets `secrets.enabled: true`.
  Searched `packages/coding-agent/src/config` — the only hits are the schema default and
  the settings hook. Remaining repo-wide hits are tests using `Settings.isolated(...)`.

**Runtime path:** `createAgentSession` -> `obfuscator === undefined` ->
`transformProviderContext` (`sdk.ts:3447-3448`) evaluates
`obfuscator ? obfuscateProviderContext(obfuscator, context) : context` and returns `context`
unchanged -> pi-ai `transformMessages` (`transform-messages.ts:599`) calls
`redactSensitiveCredentialsInMessages`, which returns the array identity-unchanged.

**Observed behavior:** With stock configuration, zero bytes of outbound conversation content
are inspected for secrets. Both filters are compiled-in no-ops.

**Conclusion:** The mechanism exists but is not a default protection. Any claim that OMP
protects secrets on the wire is conditional on the user having found and enabled a
non-default setting.

**Confidence: HIGH**

---

### Finding 2 — When enabled, M1 is a known-value redactor, not a secret scanner

**Evidence:** `packages/coding-agent/src/secrets/index.ts:247-268`, `buildSecretObfuscator`.
Three and only three sources feed the obfuscator:

1. **Environment variables** — `collectEnvSecrets` (`secrets/index.ts:188-199`). Gated on
   `SECRET_ENV_PATTERNS = /(?:KEY|SECRET|TOKEN|PASSWORD|PASS|AUTH|CREDENTIAL|PRIVATE|OAUTH)(?:_|$)/i`
   (`:185`) and `MIN_ENV_VALUE_LENGTH = 8` (`:182`). Matching is on the **variable name**,
   never on the value. `DATABASE_URL`, `SENTRY_DSN`, `STRIPE_SK`, `GH_PAT` do not match.
2. **`secrets.yml`** — user-authored, project + global (`secrets/index.ts:165-180`). Empty
   for every install that has not hand-written one.
3. **One built-in regex** — `builtinCredentialSecretEntries` (`secrets/index.ts:214-224`),
   whose `content` is literally `SENSITIVE_TOKEN_RE.source`: the same six vendor prefixes
   as M2.

**Runtime path:** `obfuscateProviderContext` (`secrets/message-transform.ts:323-327`) ->
`obfuscateMessages` (`:278-318`) -> per-message `obfuscator.obfuscate(text, sharedRegexSecretValues)`.

**Observed behavior:** Message bodies (user, `toolResult`, user-attributed developer, and
assistant replay content) are rewritten to keyed HMAC placeholders such as
`$$GITHUBTOKEN_3P8W5JH1TK2Q:L$$`. `deobfuscateToolArguments`
(`message-transform.ts:106-112`) restores them before local tool execution, so exact-match
edits still work. The engineering here is sound — placeholder bases are HMAC-keyed under a
per-install key so a transcript reader cannot dictionary them back
(`secrets/placeholder.ts:189-193`), and the key file redacts itself
(`secrets/obfuscator.ts:195-205`). **But what it can see is bounded entirely by the three
sources above.**

**Conclusion:** M1 protects values the user already told it about, plus six vendor token
shapes. It is a known-value redactor. It performs no discovery.

**Confidence: HIGH**

---

### Finding 3 — The only content-derived detector is a six-prefix allowlist with a fail-open entropy gate

**Evidence:** `packages/ai/src/providers/transform-messages.ts:418-419`:

```
export const SENSITIVE_TOKEN_RE =
  /(?<![a-zA-Z0-9_*-])(gh[opusr]_[a-zA-Z0-9_*]{36,}|github_pat_[a-zA-Z0-9_*]{36,}|
   glpat-[a-zA-Z0-9_*-]{20,}|sk-proj-[a-zA-Z0-9_*-]{36,}|sk-ant-[a-zA-Z0-9_*-]{36,}|
   sk-[a-zA-Z0-9_*-]{48,})(?![a-zA-Z0-9_*-])/gi;
```

Covered: GitHub PAT/OAuth, GitHub fine-grained PAT, GitLab PAT, OpenAI project key,
Anthropic key, legacy OpenAI key. That is the complete list.

**NOT covered — searched, MISSING:**

| Credential class | Detector present? |
|---|---|
| `-----BEGIN ... PRIVATE KEY-----` (PEM / OpenSSH / RSA / EC / PKCS8) | **MISSING** |
| AWS access key `AKIA...` / `ASIA...` and secret access key | **MISSING** |
| JWT (`eyJ...` three-segment) | **MISSING** |
| Slack `xox[bpsa]-` | **MISSING** |
| Google API key `AIza...` | **MISSING** |
| Stripe `sk_live_` / `rk_live_` | **MISSING** |
| Database URL with inline password (`postgres://u:p@h/db`) | **MISSING** |
| Generic high-entropy / base64 blob | **MISSING** on the LLM path |
| `.htpasswd`, `.netrc`, kubeconfig, `.pgpass` | **MISSING** |

Search method: repo-wide regex for `AKIA`, `BEGIN [A-Z ]*PRIVATE KEY`, `id_rsa`,
`id_ed25519`, `\.pem`, `netrc`, `gitleaks|trufflehog|detect-secrets|secretlint|semgrep`,
`shannon|entropy`. The only `AKIA` occurrence in the tree is a **documentation example** of
a user-authored `secrets.yml` rule (`docs/secrets.md`) — the user is expected to write it
themselves. It is not shipped.

The only Shannon-entropy implementation in the repo is
`packages/mnemopi/src/core/content-sanitizer.ts:46-63` (`shannonEntropy`,
`ENTROPY_THRESHOLD = 5.0`), used by `looksLikeBase64Blob` (`:60-63`) to move large base64
payloads into blob storage. It is gated on `content.length < SIZE_BASE64_CHECK` (100_000),
is a **size** optimization for the memory subsystem, and is not on the provider path. It is
not a secret detector.

**Fail-open entropy gate:** `hasPlausibleCredentialEntropy` (`transform-messages.ts:421-439`)
requires at least 2 of {lowercase, uppercase, digit, `[_-]`} in the token body.
`redactSensitiveCredentials:459` reads
`if (!hasPlausibleCredentialEntropy(match)) return match;` — an all-lowercase or all-numeric
key that already matched the prefix regex is **returned verbatim**. The uncertain case
transmits.

**Conclusion:** Content-derived detection is a narrow vendor-prefix allowlist. The most
common high-severity credential formats — private keys, AWS, JWTs, DB URLs — have no
detector at all, in either mechanism, at any setting.

**Confidence: HIGH**

---

### Finding 4 — No mechanism BLOCKS; every mechanism rewrites and forwards

**Evidence:** signatures of every filter on the path:

- `obfuscateProviderContext(obfuscator, context): Context` (`message-transform.ts:323`) —
  returns a Context, never throws, no deny branch.
- `obfuscateMessages(obfuscator, messages): Message[]` (`message-transform.ts:278`) — same.
- `redactSensitiveCredentials(text): string` (`transform-messages.ts:456`) — same.
- `redactSensitiveCredentialsInMessages(messages): Message[]` (`transform-messages.ts:505`)
  — same.

MISSING: searched `packages/coding-agent/src/secrets` and `packages/ai/src/providers` for
any abort / throw / deny keyed on secret detection. There is no equivalent of "a credential
was detected, hold the request and ask the user". Detection never changes control flow —
only bytes.

**Conclusion:** DETECT + REDACT only. No BLOCK, and no WARN surfaced to the user on the
outbound path. A credential the redactor does not recognize is not merely unredacted —
nothing anywhere notices it left.

**Confidence: HIGH**

---

## Part 2 — Concrete traces: what actually reaches the provider

Default configuration means `secrets.enabled: false`, `tools.approvalMode: "yolo"`.
Credential values below are masked; no real secret is reproduced.

Tool-call path is identical for all five traces: `ReadTool.execute` -> text content blocks
-> `toolResult` message -> `convertToLlmFinal` (`sdk.ts:3408-3412`) ->
`transformProviderContext` (`sdk.ts:3447`) -> provider stream.

### Trace A — `read .env`

**Approval:** `ReadTool.approval` (`tools/read.ts:710-717`) returns tier `"read"` for an
ordinary path. Default `approvalMode: "yolo"` auto-approves every tier
(`settings-schema.ts:4223-4226`). **No prompt.** MISSING: no path denylist —
searched `packages/coding-agent/src/tools` for `\.env`, `id_rsa`, `\.pem`, `credentials`
as guards. The only `.env` reference in the tools tree is `tools/glob.ts:142-144`, an
**example in the tool description** teaching the model how to glob `.env*` past gitignore.

**Default config — reaches the provider verbatim:**

- `DATABASE_URL=postgres://app:<MASKED>@db.internal:5432/prod`
- `AWS_SECRET_ACCESS_KEY=<MASKED>`
- `STRIPE_SECRET=sk_live_<MASKED>` — `sk_live_` is not in `SENSITIVE_TOKEN_RE`; the `sk-`
  arm requires a hyphen and 48+ chars.
- `GITHUB_TOKEN=ghp_<MASKED 36+>`

**Enabled config — partial.** If `process.env` already holds the same value under a name
matching `SECRET_ENV_PATTERNS`, it is registered as a plain secret and placeheld. Note
`packages/utils/src/env.ts:240-257` autoloads `~/.env`, the config-root `.env`, the
agent-dir `.env` and `getProjectDir()/.env` into `Bun.env`, so a cwd-root `.env` is often
covered **by value** through that route. A `.env` in a subdirectory, a `.env.production`, or
any key whose **name** misses the pattern (`DATABASE_URL`, `SENTRY_DSN`) is **not** covered.
`ghp_...` is covered by the built-in regex.

### Trace B — `read ~/.ssh/id_rsa` (SSH private key)

**Approval:** tier `"read"`. No prompt at default. No path guard.

**Default and enabled config — identical:** the full
`-----BEGIN OPENSSH PRIVATE KEY----- <MASKED BODY> -----END OPENSSH PRIVATE KEY-----`
block reaches the provider **verbatim**. MISSING: no PEM detector in `SENSITIVE_TOKEN_RE`,
in `builtinCredentialSecretEntries`, or anywhere in `packages/coding-agent/src/secrets`.
Enabling `secrets.enabled` changes nothing for this case.

### Trace C — `read server.pem` (TLS private key)

Identical to Trace B. MISSING detector. Verbatim to the provider at every setting.

### Trace D — `read ~/.aws/credentials`

**Default and enabled config:** `aws_access_key_id = AKIA<MASKED>` and
`aws_secret_access_key = <MASKED>` both reach the provider **verbatim**. `AKIA` is MISSING
from every shipped pattern; `docs/secrets.md` offers it only as a sample rule the user must
author themselves in `secrets.yml`. Enabling `secrets.enabled` without also hand-writing
that rule changes nothing.

### Trace E — JWT in a log file (`read` or `grep` of `app.log`)

**Default and enabled config:** `Authorization: Bearer eyJhbGciOi<MASKED>` reaches the
provider **verbatim**. MISSING: no JWT detector. Note the asymmetry — the MCP error path
does redact `Bearer ...` (`mcp/errors.ts:90`), and `docs/secrets.md` shows a `bearer` regex
as a user-authored example, but neither is applied to LLM-outbound content.

### Trace summary

| Target | Prompt before READ? | Filtered before SEND (default) | Filtered before SEND (`secrets.enabled`) |
|---|---|---|---|
| `.env` | No (yolo) | **No** | Partial — name-matched env values plus `gh*`/`sk-` shapes only |
| SSH private key | No | **No** | **No** |
| PEM / TLS key | No | **No** | **No** |
| AWS credentials | No | **No** | **No** (unless user hand-wrote the rule) |
| JWT in log | No | **No** | **No** |

---

## Part 3 — Permission prompt vs. outbound filtering

These are different controls, and the repo implements only the first as a default-present
surface — and even that is default-disabled.

**Permission prompt — asks before READING.** `tools/approval.ts`. Policy is per-tool tier
(`"read" | "write" | "exec"`) crossed with `tools.approvalMode`. `ReadTool` declares
`"read"` (`tools/read.ts:710-717`). This control is:

- **path-blind** — it never inspects the target path for sensitivity. `.env`, `id_rsa`,
  `.aws/credentials` and `README.md` are the same tier. The only path-sensitive branch
  escalates `ssh://` targets and PDF-image reads to `"exec"` — availability and cost
  concerns, not secrecy.
- **content-blind** — it fires before the bytes exist.
- **off by default** — `approvalMode: "yolo"` auto-approves every tier.

Even with `always-ask`, approving one read authorizes **transmission of whatever that file
contains**. The user is answering "may the agent look at this path", not "may these bytes go
to Anthropic". A user who approves reading `config/` has not consented to shipping an
embedded key.

**Outbound filtering — inspects what is SENT.** M1 + M2. This **is** a privacy boundary: it
sits at `transformProviderContext`, the last host-controlled point before the provider
stream. It is off by default and narrow when on.

**A third control that is neither:** `tools.approval.<tool>: deny`
(`approval.ts:136-151`, `:250-251`) can disable the `read` tool wholesale. That is a blunt
capability switch, not a secret control, and it does not constrain `bash`, `grep`, or MCP
tools, which reach the same bytes.

---

## Part 4 — Local paths, usernames, hostnames, internal IPs (lower severity)

### Finding 5 — Absolute paths containing the OS username are transmitted on every request

**Evidence:**

- `packages/coding-agent/src/prompts/system/project-prompt.md:3-6` — the `<workstation>`
  block, rendered from `environment`.
- `packages/coding-agent/src/system-prompt.ts:357-371`, `getEnvironmentInfo` — emits `OS`
  (`os.platform()` + `os.release()`), `Distro` (`os.type()`), `Kernel`
  (`getKernelIdentity`, `:351-355` — the full uname build string), `Arch`, `CPU`
  (`getCpuModel`, `:329-341`; on Linux parsed from `/proc/cpuinfo`), `GPU`, and `Terminal`
  (`getTerminalName`, `:272-283` — `TERM_PROGRAM` plus version).
- `packages/coding-agent/src/prompts/system/date-cwd-reminder.md:2` —
  `Today: {{date}}; current working directory: '{{cwd}}'.`
- `packages/coding-agent/src/sdk.ts:3465-3470` —
  `dateCwdReminder.transform(transformed, formatLocalCalendarDate(), normalizePromptPath(sessionManager.getCwd()))`,
  injected into the outbound context on every request.
- `packages/coding-agent/src/utils/prompt-path.ts:1-3` — `normalizePromptPath` is
  `value.replace(/\\/g, "/")`. Backslash-to-slash only. **No home-directory elision.**

**Observed behavior:** Every request carries the absolute cwd — on macOS
`/Users/<username>/...`, on Linux `/home/<username>/...` — plus a detailed hardware and
kernel fingerprint. Tool results compound this: `read`, `grep`, `glob` and `bash` all emit
absolute paths in their output.

**Notable asymmetry (OBSERVED):** the codebase has a home-eliding helper and applies it to
**terminal rendering** — `shortenPath` is mandated by `AGENTS.md`'s "TUI Sanitization"
section (`AGENTS.md:229-234`, which explicitly names "paths -> leak home directory" as the
hazard), and `session/ttsr-coordinator.ts:191-194` computes `~/`-relative paths. None of
that is applied to the provider payload. Path sanitization in this repo protects the
**terminal**, not the **provider**.

**Conclusion:** OS username, home-path layout, kernel build string, CPU/GPU model and
terminal program are transmitted unconditionally. `secrets.enabled` does not affect this:
`obfuscateProviderContext` (`message-transform.ts:322-327`) rewrites only `context.messages`
— its own doc comment states that the static system prompt and tool schemas pass through
unchanged.

**Confidence: HIGH**

### Finding 6 — No hostname is injected by the harness; internal hostnames and IPs reach the provider only via tool output

**Evidence:** `getEnvironmentInfo` (`system-prompt.ts:361-368`) contains no `os.hostname()`
entry; MISSING repo-wide in the prompt-building path. Hostname is collected elsewhere —
`cli/usage-cli.ts:966-978` renders a per-client hostname from broker usage data — but that
is the auth-broker telemetry surface, not LLM context.

RFC1918 and loopback detection exists at `config/append-only-context-mode.ts:36-51`
(`hasLocalLoopbackBaseUrl`), but its purpose is selecting a local-inference code path, not
filtering egress.

**Conclusion:** Internal hostnames and IPs are not injected by the harness itself. They
reach the provider whenever tool output contains them (`bash` running `hostname`,
`kubectl`, `docker`; `read` of a hosts file or k8s manifest) — with no filter on that path.
The system prompt explicitly advertises `hostname` and `whoami` as available shell builtins
(`prompts/tools/bash.md:11`).

**Confidence: HIGH**

### Finding 7 — No PII detection of any kind

**Evidence:** MISSING. Repo-wide search for PII detection (`pii`, `personal.?data`, `gdpr`,
email/phone/SSN/credit-card pattern matching applied to outbound content) returned no
implementation. The `usage-cli.ts:62-65` account-email masking is a terminal display helper
(it shortens colliding account identities to forms like `an*`, `ca*9*`) and never touches
model context.

**Conclusion:** Names, emails, customer records, or any other PII present in a file the
model reads are transmitted unmodified at every setting.

**Confidence: HIGH**

---

## Part 5 — Fail-open vs. fail-closed

**Every observed uncertainty path transmits. There is no fail-closed branch.**

| Uncertainty | Code | Behavior |
|---|---|---|
| Invalid regex in `secrets.yml` | `secrets/obfuscator.ts:157-160` — `catch { }` with comment "Invalid regex - skip silently" | Entry dropped. That secret ships. |
| `secrets.yml` unparseable or not an array | `secrets/index.ts:275-278`, `:295-297` — `logger.warn(...); return []` | **All** entries dropped. Every secret in the file ships. Warning goes to the log, not the user. |
| Individual entry invalid | `secrets/index.ts:322-357`, `validateEntry` | `logger.warn` + skip. That secret ships. |
| Regex match lacks entropy | `transform-messages.ts:459` — `if (!hasPlausibleCredentialEntropy(match)) return match;` | Original returned. Ships. |
| Value shorter than 8 chars | `secrets/placeholder.ts:18` (`MIN_OBFUSCATE_SECRET_LEN`), applied `obfuscator.ts:136-139` | Entry skipped as a false-positive guard. Ships. |
| Replace-mode regex with unresolvable short fallback | `obfuscator.ts:149-156` | Rule dropped (comment: "rather than risk a real secret round-tripping unredacted"). Ships. |
| Placeholder key unwritable | `secrets/index.ts:119-123` | `logger.warn`, continue with a process-ephemeral key. Session proceeds. |
| Credential shape not in `SENSITIVE_TOKEN_RE` | by construction | Ships silently. |

The `obfuscator.ts:149-156` case is the closest thing to a conservative choice in the
subsystem, and even it resolves by **dropping the rule** (so the secret transmits) rather
than by refusing to send. The design consistently prioritises "never corrupt the
conversation" over "never leak" — a defensible product tradeoff, but it means the control
cannot be characterised as a boundary.

**Conclusion:** The outbound filter is **fail-open** at every decision point. On uncertainty
it transmits.

**Confidence: HIGH**

---

## Part 6 — Supporting observations

### Finding 8 — System prompt is redacted by M2, never by M1

`obfuscateProviderContext` (`message-transform.ts:322-327`) rewrites only `context.messages`.
The system prompt passes through M1 untouched by explicit design (doc comment at `:322-323`;
asserted by `test/secrets-obfuscator.test.ts:160-176`). M2 does cover it —
`normalizeSystemPrompts` (`packages/ai/src/utils.ts:28-34`) maps `redactSensitiveCredentials`
over each prompt — but that is the same six-prefix pattern behind the same default-off flag.
Content injected into the system prompt from `AGENTS.md`, `.omp/` rules, or skills is
therefore unprotected by M1.

**Confidence: HIGH**

### Finding 9 — Sub-surfaces: advisor, subagent, handoff

- **Advisor** (`advisor/runtime.ts:713-738`, `advisor/delta-split.ts:69-93`): the primary
  transcript passes through the same obfuscator before reaching the advisor model,
  including `obfuscateToolArguments` for nested `details` (`runtime.ts:1576-1578`, whose
  comment notes that a shallow pass "leaks any secret a background job's label happens to
  contain"). Correctly wired; inherits every limitation of M1.
- **Subagents** (`task/executor.ts:3542`, `task/persisted-revive.ts:133`): spawn via
  `createAgentSession`, which rebuilds the obfuscator from the same `secrets.enabled` gate
  (`sdk.ts:1481`). Consistent — including consistently off.
- **Handoff and side-requests** (`session/session-handoff.ts:172`,
  `session/agent-session.ts:8882`): both route through `obfuscateProviderContext`.

No outbound gap found among these relative to the main path.

**Confidence: MEDIUM** (wiring traced by reading, not executed)

### Finding 10 — Redaction is correctly ordered before rasterization and blob upload

`sdk.ts:3447-3449`: `obfuscateProviderContext` runs **first**, then `snapcompactInline`,
then image clamping, then `blobBroker.decorateContext`. The inline comment states the
intent: obfuscate first so secrets are redacted from text before snapcompact rasterizes it
into PNG frames. This matters because the blob broker can publish those bytes to an external
host. The ordering is correct — a redacted secret cannot be recovered from the rendered PNG.
It inherits M1's coverage limits, so an undetected credential is rasterized and potentially
uploaded.

**Confidence: HIGH**

### Finding 11 — Documentation vs. code discrepancies

`docs/secrets.md` is broadly accurate on mechanics and correctly states "Disabled by
default". Two gaps worth recording:

1. The opening sentence — "Prevents sensitive values (API keys, tokens, passwords) from
   being sent to LLM providers" — overstates scope. OBSERVED: with `secrets.enabled: true`
   and no user-authored `secrets.yml`, passwords are covered only if they arrived via an
   environment variable whose **name** matches `SECRET_ENV_PATTERNS`. No password-shaped
   detector exists.
2. `AKIA[0-9A-Z]{16}`, `bearer ...` and `postgres://...` appear in `docs/secrets.md` as
   `secrets.yml` examples. A reader may take them as shipped coverage. OBSERVED: they are
   not — `builtinCredentialSecretEntries` (`secrets/index.ts:214-224`) returns exactly one
   entry, `SENSITIVE_TOKEN_RE`.

**Confidence: HIGH**

---

## Reviewed paths

```
packages/ai/src/providers/transform-messages.ts         M2, SENSITIVE_TOKEN_RE, entropy gate, role coverage
packages/ai/src/utils.ts                                normalizeSystemPrompts
packages/coding-agent/src/secrets/index.ts              sources, buildSecretObfuscator, load + validate
packages/coding-agent/src/secrets/obfuscator.ts         SecretObfuscator, constructor, keying
packages/coding-agent/src/secrets/message-transform.ts  obfuscate / deobfuscate entry points
packages/coding-agent/src/secrets/placeholder.ts        MIN_OBFUSCATE_SECRET_LEN, key requirement
packages/coding-agent/src/sdk.ts                        obfuscator gate + transformProviderContext
packages/coding-agent/src/config/settings-schema.ts     secrets.enabled, tools.approvalMode
packages/coding-agent/src/config/settings.ts            configureCredentialRedaction hook
packages/coding-agent/src/system-prompt.ts              getEnvironmentInfo, workstation block
packages/coding-agent/src/prompts/system/               project-prompt.md, date-cwd-reminder.md
packages/coding-agent/src/tools/read.ts                 approval tier, path handling
packages/coding-agent/src/tools/approval.ts             tier / policy resolution
packages/coding-agent/src/tools/grep.ts, glob.ts        output paths, no content filter
packages/coding-agent/src/session/                      session-provider-boundary, session-handoff, agent-session
packages/coding-agent/src/advisor/                      runtime.ts, delta-split.ts
packages/coding-agent/src/task/                         executor.ts, persisted-revive.ts
packages/coding-agent/src/commands/share.ts             M4
packages/coding-agent/src/mcp/                          errors.ts, json-rpc.ts (M5)
packages/coding-agent/src/blob-broker/                  provider-file-types.ts (M6)
packages/coding-agent/src/tools/browser/tab-worker.ts   M7 redactUrlCredentials
packages/coding-agent/src/cli/usage-cli.ts              M8 account masking
packages/mnemopi/src/core/content-sanitizer.ts          entropy helper (not a secret filter)
packages/utils/src/env.ts                               dotenv autoload into Bun.env
packages/coding-agent/src/utils/prompt-path.ts          normalizePromptPath
crates/pi-tasks/src, crates/pi-natives/src              no redaction (searched)
docs/secrets.md                                         claim-vs-code comparison
```

Negative searches (each returned no outbound-path implementation): `dlp`, `pii`,
`gitleaks|trufflehog|detect-secrets|secretlint|semgrep`, `AKIA`,
`BEGIN [A-Z ]*PRIVATE KEY`, `id_rsa|id_ed25519`, `\.pem`, `netrc`, credential-file path
denylists under `tools/`, and home-directory elision applied to the provider payload.
