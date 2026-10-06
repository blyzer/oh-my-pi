# Parked architecture ideas: mbx (P15/P16), Omarchy AI, tuicr (P14)

Status: **parked, not scheduled.** Nothing here is implemented or approved. It is
planning input, kept out of the working context so it does not compete with pending
omp2 work. Not authoritative: `PLAN.md` and production code outrank it.

Recorded 2026-10-06 from the owner's design commentary (verbatim below).

## Index

- **mbx / Mr Boxington -> P16 (build cache), secondarily P15.** Cargo stays the
  planner; mbx wraps rustc. Ideas: planner != executor != cache, in-flight work
  dedup, `ActionRef`/`ExecutionKey`/`ResultRef`, shared immutable cache vs private
  incremental state, resource admission, bypass-rather-than-guess, `MBX_VERIFY`,
  tracing. Not interchangeable with BuildBuddy (own HTTP protocol, not REAPI).
  Empirical trial: draft PR #165 (`.github/workflows/mbx-trial.yml`, never merge);
  compare against the sccache baseline in `docs/audits/sccache-mac-measurement.md`.
- **Omarchy AI -> agent runtime plane.** `AgentRuntimeSpec`, `AccountLease`, quota
  as a schedulable resource, `QuotaAwareScheduler`, `HarnessAdapter`,
  `AgentTelemetry`, canonical Skill plus adapters; do not default to unattended.
  Closest existing code: `crates/ai/src/account/*`.
- **tuicr -> P14 review (R1-R12).** ReviewIR/ReviewSession, structured findings with
  lifecycle, semantic anchors, coverage accounting, event-driven notifications
  instead of polling, forge/VCS adapters. Integration options: A external CLI,
  B Rust library, C own UI.

Token-efficiency findings are in `2026-10-06-omp2-token-efficiency-audit.md`; the
rest waits until the owner schedules it.

---

## Verbatim commentary

Este sí toca directamente P16, y secundariamente P15. De todos los enlaces recientes, mr-boxington es probablemente el primero que yo consideraría usar realmente en el desarrollo de OMP², no solo estudiar como referencia.

mbx es un cache/build coordinator para Rust que se coloca alrededor de Cargo/rustc. Mantiene Cargo como planner, pero reutiliza compilaciones equivalentes entre worktrees/CI, gestiona target/, coordina compilaciones concurrentes y mantiene incremental state privado para crates que están cambiando. No necesita daemon persistente. 

La arquitectura conceptual es más interesante de lo que parece:

                 Cargo
                   │
             plans build DAG
                   │
                   ▼
                 mbx
          ┌────────┼─────────┐
          │        │         │
      key/action  cache   scheduler
          │        │         │
          │        │    CPU + memory
          │        │      permits
          ▼        ▼         │
       rustc ← hit/miss ─────┘
          │
          ▼
    content-addressed
        outputs

1. Esto valida nuestra separación P16: planner ≠ executor ≠ cache

Una tentación para OMP² sería construir un “super build system”.

Yo no lo haría.

Mr. Boxington deja a Cargo resolver dependencias y construir el build plan; mbx interviene debajo, alrededor de las compilaciones. 

Eso sugiere para P16:

OMP² Build/Exec Plane
BuildPlanner
    Cargo / Bazel / other native system
          │
          ▼
ExecutionCoordinator
          │
          ├── cache
          ├── dedup
          ├── resource admission
          ├── tracing
          └── remote execution/cache

No:

OMP² reimplements Cargo/Bazel

Eso encaja con nuestra decisión previa de Bazel/BuildBuddy para el cálculo distribuido, mientras OMP² se ocupa de coordinación y semántica.

⸻

2. Su idea más interesante para P15: in-flight work deduplication

Supón seis agentes:

agent A → cargo test
agent B → cargo check
agent C → cargo test
agent D → cargo clippy
agent E → cargo build
agent F → cargo test

Sin coordinación, Cargo individualmente puede lanzar una enorme cantidad de compiladores.

mbx mantiene un pool global de permisos y además hace que compilaciones idénticas concurrentes esperen a que la primera termine, para después restaurar el resultado en vez de ejecutarlas de nuevo. En su benchmark de seis jobs, reportan un pico de 32 compiladores con scheduler frente a 162 sin él; además el workload terminó 9 segundos antes en ese caso. 

Esto es extremadamente relevante para multi-agent OMP².

Hoy podríamos tener:

Agent A
   └─ Verify(repo)
Agent B
   └─ Verify(repo)
Agent C
   └─ Verify(repo)

El orchestration layer debería reconocer:

ExecutionKey(A.verify)
    ==
ExecutionKey(B.verify)

y hacer:

                    execution E17
                   /      |      \
                A waits  B waits  C waits
                         │
                         ▼
                      result
                   /      |      \
                  A       B       C

No ejecutar tres veces.

Esto merece convertirse en un invariant de P15/P16:

same deterministic action
+ same effective inputs
+ same execution environment
→ at-most-one in-flight execution

⸻

3. Esto conecta directamente con nuestro ResultRef

Herdsman nos llevó a:

ResultRef

Mr. Boxington muestra una implementación física de una idea equivalente:

ActionKey
      ↓
result
      ↓
content-addressed outputs

Para OMP²:

ActionRef {
    tool
    normalized_args
    environment
    input_refs[]
    policy
}
hash(ActionRef)
       │
       ▼
ExecutionKey

y:

ExecutionKey
   ↓
ResultRef

Entonces dos agentes no necesitan coordinarlo en lenguaje natural.

El Execution Plane puede verlo determinísticamente.

⸻

4. También encaja con nuestro CAS

Mr. Boxington sustituye las diferencias de path del checkout por placeholders al construir las claves, de modo que worktrees diferentes puedan reutilizar una compilación equivalente. 

Eso es exactamente la clase de canonicalization que necesitamos para OMP²:

/Users/enver/omp2-wt1/src/foo.rs
/Users/enver/omp2-wt2/src/foo.rs
/home/runner/work/omp2/src/foo.rs

no deberían producir tres identidades semánticas si el contenido efectivo es idéntico.

Queremos:

physical path
      ↓
workspace-relative canonical path
      ↓
content hash
      ↓
ArtifactRef

Esto enlaza muy bien con lo que sacamos de Bitwright sobre canonical structural identity.

⸻

5. learned incremental reuse es especialmente interesante

Mr. Boxington hace una distinción muy buena:

Shared action cache
    immutable reusable results
vs
Learned incremental state
    private mutable state

Cuando detecta que un crate empieza a cambiar repetidamente, conserva estado incremental privado para ese checkout. Ese estado no se publica al cache compartido. 

Esto es una idea muy aplicable a nuestro Context/Evidence architecture.

Podemos tener:

GLOBAL / SHAREABLE
CodeIR snapshot
EvidenceRef
compiled artifact
SCIP index segment
verified ResultRef
vs
LOCAL / EPHEMERAL
active parser state
working-set indexes
incremental compiler state
agent scratch state
unverified hypotheses

No todo lo útil para acelerar una sesión debe convertirse en estado global.

Muy importante.

⸻

6. Y la parte “learned” no usa ML

El nombre puede confundir.

Aquí learned_incremental significa que observa misses repetidos y cambia automáticamente de estrategia de compilación; no hay un modelo de ML detrás. Por ejemplo, un crate externo al workspace cambia a incremental después de varios misses consecutivos causados por cambios de fuente. 

Conceptualmente:

observe behavior
     ↓
classify workload
     ↓
change execution policy

Esto se parece mucho al sistema de adaptive policies que estábamos proponiendo después de OMP maxxed.

Podemos hacerlo para:

context strategy
search strategy
verification strategy
build strategy
cache strategy

sin meter un LLM.

⸻

7. Resource admission es muy relevante para tu M1 Pro 16 GB

Mr. Boxington no limita simplemente:

-j 8

Tiene un pool compartido de CPU + memoria. Por defecto usa los CPUs lógicos y un presupuesto del 85% de la RAM física; los procesos de compilación toman permisos de acuerdo con estimaciones de memoria obtenidas de ejecuciones anteriores. También puede reaccionar a memory pressure. 

Esto es exactamente lo que necesitaríamos si en OMP² tienes simultáneamente:

Rust compile
SCIP index
Tree-sitter processing
tests
agent subprocess
language server

especialmente en una máquina de 16 GB.

Yo generalizaría su modelo:

ResourcePermit {
    cpu
    memory
    io
    process_slots
    priority
}

Y P15 no debería permitir simplemente:

Tokio task ready
→ execute

sino:

task ready
     ↓
ResourceAuthority
     ↓
permit
     ↓
execute

Esto conecta muy bien con z0intelligence:

DecisionAuthority
        +
ExecutionAuthority
        +
ResourceAuthority

⸻

8. Una idea particularmente buena: cache hit no consume compiler permit

Eso parece trivial, pero tiene implicaciones arquitectónicas.

Una operación:

requested

no debería reservar recursos caros antes de saber si realmente tiene que ejecutarse.

El flujo correcto es:

request
   ↓
normalize
   ↓
cache lookup / join existing work
   │
   ├── hit ─────────→ return
   │
   ├── in-flight ───→ join
   │
   └── miss
         ↓
    acquire expensive permit
         ↓
       execute

No:

acquire resources
       ↓
discover cached

Esto debería convertirse en parte de nuestro ExecutionAuthority.

⸻

9. MBX_VERIFY=1 encaja perfecto con Bitwright

Mr. Boxington tiene un modo de qualification donde compila mientras consulta el cache y compara el resultado restaurado con el nuevo resultado. Es caro, pero sirve para validar que las reglas de caching son correctas. 

Es exactamente:

optimized path
     │
     ├───────────┐
     ▼           ▼
 cached        recompute
 result         result
     │           │
     └─────compare

Esto es equivalente al principio que sacamos de Bitwright/NeverD:

optimization
must preserve semantics

Para OMP² podemos tener:

fast path
    ↓
ResultRef
occasionally:
ResultRef
    vs
ground-truth recomputation

Esto permitiría calibrar la confianza del cache/planner.

⸻

10. La filosofía bypass rather than guess vuelve a aparecer

Mr. Boxington no intenta cachear una invocación que no puede modelar correctamente.

Si no puede describir suficientemente una operación —por ejemplo ciertos linkers, rutas no modeladas o compilaciones incrementales— hace bypass y deja que la compilación normal ocurra. 

Otra vez aparece nuestro patrón:

NeverD:
unsupported → don't lift
Bitwright:
unproven → don't transform
Mr Boxington:
unmodeled → don't cache

Y para OMP²:

uncertain optimization
      ≠
permission to optimize

Eso ya está apareciendo como un principio transversal muy fuerte.

⸻

11. Observability: event stream + Perfetto

También me gusta mucho que el sistema tenga streams por compilación y pueda exportar traces a Perfetto con fases como:

* key construction;
* cache lookup;
* transfer;
* restore;
* scheduler wait;
* compiler execution;
* store.

Esto es casi exactamente lo que quiero para Neper/P15:

Agent turn
│
├─ context selection
├─ CSL lookup
├─ LSP
├─ model
├─ tool admission
├─ queue wait
├─ execution
├─ verification
└─ result publish

en una trace común:

OpenTelemetry
      ↓
Perfetto / trace UI

Así podríamos responder preguntas como:

Why did this agent take 41s?

con:

12s waiting for compilation permit
8s duplicated semantic lookup
14s model call
5s verification
2s everything else

en vez de adivinar.

⸻

12. ¿Mr. Boxington vs BuildBuddy?

Aquí no los consideraría sustitutos directos.

Tú ya habías decidido usar BuildBuddy remotamente para recálculo/build, y esa decisión puede mantenerse.

Mr. Boxington es específicamente Rust/Cargo-oriented y ofrece:

local worktree reuse
local incremental strategy
local compiler coordination
Cargo-aware action caching
target management

BuildBuddy es más:

Bazel remote execution
remote cache
distributed build infrastructure

Mr. Boxington tiene además su propio protocolo HTTP de remote cache, no REAPI/Bazel Remote Execution API. Sus backends documentados son su cache server, S3-compatible storage y GitHub Actions cache; por eso no asumiría compatibilidad directa con BuildBuddy. 

Por tanto, para OMP² yo contemplaría:

Developer laptop
Cargo
  ↓
mbx
  ├─ local CAS
  ├─ local incremental
  └─ local scheduler
Heavy/reproducible pipeline
Bazel
  ↓
BuildBuddy
  ├─ remote cache
  └─ remote execution

No intentaría forzar:

mbx → BuildBuddy cache

sin construir un adapter explícito.

⸻

13. Yo sí lo probaría en OMP² ahora

A diferencia de varios repos anteriores:

Herdsman        → study
z0              → extract concepts
NeverD          → study
MetalCyan       → benchmark
Bitwright       → study / possible backend

para Mr. Boxington diría:

Mr Boxington
    → benchmark AND seriously consider using

porque OMP² es Rust-first y vamos a tener muchos worktrees/agentes/builds paralelos. El caso de uso coincide casi exactamente. Sus benchmarks actuales muestran, por ejemplo, un warm build de hk de ~1.3 s con 354 hits y un next-commit build de 9.2 s frente a 18.9 s de Cargo uncached; pero el propio proyecto advierte correctamente que estas cifras dependen del shape del proyecto y recomienda medir el workload real. 

⸻

Cómo actualizaría P16

Yo añadiría explícitamente:

P16 — Build / Execution
P16.1 Native Cargo baseline
P16.2 mbx local cache
P16.3 mbx machine scheduler
P16.4 multi-worktree reuse
P16.5 concurrent-agent dedup
P16.6 learned incremental loop
P16.7 CAS normalization
P16.8 BuildBuddy/Bazel remote path
P16.9 resource admission
P16.10 execution tracing
P16.11 qualification / recomputation
P16.12 cache correctness under mutation

Y medir:

cold
warm
1% edit
worktree clone
10 agents
100 agents requesting overlapping builds
compiler memory
wall time
CPU time
disk
cache hit ratio
duplicate work avoided
time-to-first-diagnostic

Esto sería muchísimo más representativo que simplemente:

Rust vs Zig compile time

⸻

El patrón común empieza a ser muy claro

Con los repos que has ido trayendo:

Proyecto	Principio
Herdsman	don’t duplicate ownership
z0intelligence	don’t confuse evidence with authority
NeverD	don’t invent unsupported semantics
MetalCyan	don’t ignore applicability
Bitwright	don’t transform without justification
OMP maxxed	don’t use one context policy for every model
Mr. Boxington	don’t repeat deterministic work you can identify and reuse

Y ese último principio es extremadamente importante para OMP².

Yo lo formularía para nuestra arquitectura como:

Compute once.
Name by effective inputs.
Verify when necessary.
Share by immutable reference.
Recompute only when invalidated.

Eso sirve tanto para builds como para CSL queries, semantic indexes, verification runs, ResultRefs e incluso partes del contexto del agente. Ahí es donde Mr. Boxington deja de ser solamente “un Cargo cache” y se vuelve una referencia arquitectónica muy útil para OMP². 

Sí. Omarchy AI aporta una capa que todavía no habíamos aislado claramente en OMP²: el Agent Runtime Control Plane del usuario.

No intenta mejorar cómo razona un agente. Lo interesante es que trata Claude Code, Codex, OMP, Pi, OpenCode, Grok, Hermes, Cursor, etc. como runtimes intercambiables administrados por el sistema operativo. Hoy Omarchy expone todos esos CLIs mediante launchers lazy-loaded administrados con mise; incluso incluye omp explícitamente. 

Yo lo situaría así:

Herdsman        → orchestration between agents
z0intelligence  → decision/control semantics
OMP² Context    → context/state management
CSL             → code semantics
Mr Boxington    → build/execution coordination
Omarchy AI      → USER/HOST AGENT CONTROL PLANE

1. La idea central: harness ≠ model ≠ account ≠ session

Omarchy está empezando a separar correctamente cuatro cosas que muchos sistemas mezclan.

Por ejemplo:

Harness
  Claude Code
  Codex
  OMP
  Pi
  OpenCode
  Ori
Provider/model
  OpenAI
  Anthropic
  OpenRouter
  ...
Account
  personal
  work
  account-2
Session
  particular coding task

Esto es especialmente visible con Ori, que puede ejecutar otros harnesses (ori claude, ori codex, ori opencode) contra modelos de OpenRouter. 

Eso refuerza muchísimo algo que deberíamos hacer en OMP²:

AgentDefinition
       ≠
HarnessRuntime
       ≠
ModelEndpoint
       ≠
CredentialProfile
       ≠
ExecutionSession

Yo formalizaría algo parecido a:

AgentRuntimeSpec {
    harness,
    provider,
    model,
    account_ref,
    context_policy,
    tool_policy,
    execution_policy,
}

No:

agent = "claude"

porque eso ya es demasiado ambiguo.

⸻

2. AccountLease: esto me parece muy relevante

Omarchy soporta varias suscripciones Claude/Codex/Grok simultáneamente y puede cambiar cuál está activa. Más interesante todavía: las sesiones que ya están ejecutándose permanecen asociadas a la cuenta con la que empezaron, aunque cambies la cuenta activa para sesiones nuevas. 

Eso me parece el comportamiento correcto.

En OMP² lo convertiría en:

Session
   │
   └── AccountLease
          provider = OpenAI
          account  = work
          acquired_at = ...

Mientras:

new Session
   ↓
AccountSelector
   ↓
personal / work / ...

No debemos cambiar credenciales debajo de una ejecución viva:

running agent
    ↓
account changes globally
    ↓
??? authentication/context discontinuity

Omarchy evita eso.

Para P15 añadiría

ProviderLease
AccountLease
ModelLease

como identidad inmutable de una ejecución.

⸻

3. Las cuotas se convierten en un recurso del scheduler

Esto es probablemente la lección más importante.

El panel de Omarchy presenta por cuenta:

* límites de sesión de 5 horas;
* límites semanales;
* saldo prepaid;
* tokens;
* uso por modelo;
* reset de las ventanas.

Puede incluso hacer autoswitch de cuenta al alcanzar un umbral configurable; por defecto alerta cerca del 95%, y en modo auto puede cambiar a otra cuenta con más margen. 

Eso significa que:

API/subscription quota

ya no es simplemente un error 429 que descubre el agente.

Es un recurso schedulable.

Para OMP² yo añadiría:

ResourceAuthority
 ├── CPU
 ├── memory
 ├── IO
 ├── build slots
 ├── context budget
 ├── token budget
 └── provider quota       ← Omarchy insight

Entonces P15 podría hacer:

Task
  ↓
requires frontier reasoning
  ↓
ProviderSelector
OpenAI account A
   96% weekly
OpenAI account B
   38%
Claude
   61%
      ↓
policy / capability / cost
      ↓
select appropriate lease

Esto conecta directamente con los 429 usage_limit_reached que ya te han aparecido trabajando con OMP: quota debería ser observable antes de dispatch, no descubrirse solamente por failure.

⸻

4. Pero yo NO copiaría su autoswitch ciegamente

Omarchy puede elegir otra cuenta por headroom.

OMP² debería considerar además:

available quota
+
model capability
+
task requirement
+
context compatibility
+
cache affinity
+
cost
+
latency

Porque:

account B has more quota

no implica:

moving this task there is optimal

Especialmente después de nuestra discusión de remote compaction:

provider continuity matters

Así que:

QuotaAwareSelector

sí.

Pero no:

largest remaining percentage wins

como regla universal.

⸻

5. Su collector contract es excelente

La UI de Omarchy no conoce íntimamente cada agente.

Los collectors generan registros JSON en:

~/.local/state/omarchy/agents/usage/

y el panel simplemente consume ese contrato. Para añadir otro agente, implementas un collector que produzca el record esperado. 

Muy buen diseño:

Claude ── collector ──┐
Codex  ── collector ──┤
OMP    ── collector ──┼→ UsageRecord → Control Plane
Grok   ── collector ──┤
X      ── collector ──┘

Para OMP² lo generalizaría:

Runtime Adapter
       │
       ├── capabilities()
       ├── usage()
       ├── launch()
       ├── resume()
       ├── interrupt()
       ├── context_state()
       └── health()

y todos producen tipos comunes.

Eso es bastante mejor que:

if claude {...}
else if codex {...}
else if omp {...}

⸻

6. Y su telemetry contract encaja con nuestro EventLog

Omarchy recopila estadísticas locales de distintos lugares:

* transcripts de Claude;
* sesiones Codex;
* Pi/OpenCode en ciertos casos;
* endpoints de usage;
* billing de Fireworks;

y las normaliza en una vista común. 

OMP² debería hacer esto todavía mejor:

Runtime events
     ↓
Normalization
     ↓
AgentTelemetry
{
  provider
  model
  account
  session
  prompt_tokens
  output_tokens
  cached_tokens
  context_compactions
  model_latency
  tool_latency
  failures
  quota_remaining
}

y después correlacionarlo con:

DecisionIR
ExecutionRef
VerificationIR
ResultRef

Entonces podríamos calcular algo muchísimo más interesante que:

tokens this week

como:

tokens / verified change
tokens / accepted decision
cost / bug fixed
context tokens / semantic state preserved
failed reasoning / model
cache benefit / model

Ese sería el verdadero OMP² telemetry plane.

⸻

7. Cross-machine aggregation también es interesante

Omarchy permite combinar estadísticas de varias máquinas mediante una carpeta sincronizada; cada equipo produce snapshots y el panel los agrega. 

Nosotros probablemente no haríamos el sync de esa manera, pero la semántica es útil:

Laptop
Runner
Workstation
Remote Build
      │
      ▼
Unified agent telemetry

Para nuestro caso:

MacBook
   OMP² interactive
BuildBuddy
   remote execution
CI
   verification
remote agents
   P15

deberían producir el mismo EventLog/provenance schema.

⸻

8. Skills: Omarchy hace algo muy simple pero correcto

Su omarchy skill se instala una vez y se symlinkea hacia:

~/.claude/skills
~/.codex/skills
~/.pi/agent/skills
~/.gemini/config/skills
~/.hermes/skills
~/.agents/skills

por lo que una fuente lógica puede ser consumida por distintos harnesses. 

Esto refuerza nuestra idea de que:

Skill
    ≠
Claude skill
    ≠
OMP skill
    ≠
Codex skill

Debe existir:

Canonical Skill
       │
       ▼
HarnessAdapter
 ├── Claude
 ├── Codex
 ├── OMP
 └── Pi

Omarchy resuelve el problema por filesystem symlinks.

OMP² puede hacerlo semánticamente:

SkillIR / SkillManifest
          ↓
adapter
          ↓
harness-specific representation

Especialmente si terminamos adoptando agent-plugins.org o una convención común.

⸻

9. Algo que NO copiaría: unattended como default conceptual

Omarchy lanza varios agentes desde sus shortcuts en modos auto-approve/“don’t-stop-to-ask”. El propio manual advierte que realmente pueden hacer cosas. 

Esto está bien para una distro opinionada.

Pero para OMP² yo separaría:

Launch ergonomics

de:

Execution authority

Nunca:

keyboard shortcut
   ↓
yolo permission

sino:

Agent launch
    ↓
ExecutionAuthority
    ↓
ToolAdmission
    ↓
risk policy

Nuestra capa inspirada por z0 sigue siendo superior aquí.

⸻

10. Omarchy incluso reconoce el problema con skills

El manual recomienda tratar su skill como experimental, usar primero plan mode y estar preparado para rollback/reinstall de configuración si el agente modifica algo incorrectamente. 

Eso confirma otra de nuestras líneas:

skill instruction
        ≠
verified capability

Un skill debería declarar:

capabilities
preconditions
allowed effects
verification
rollback

no simplemente ser Markdown diciéndole al LLM:

"here is how you edit Hyprland"

⸻

11. Omarchy tampoco cambia nuestra decisión sobre local LLM

Tiene LM Studio y Ollama como opciones de usuario. 

Pero eso es:

model hosting capability

y no obliga a que OMP² haga:

local LLM recalc

Así que nuestra decisión permanece:

no required local inference
BuildBuddy remote compute
frontier models where reasoning is needed
deterministic local kernels otherwise

Sin conflicto.

⸻

Cómo actualizaría la arquitectura

Añadiría explícitamente un plano:

                               OMP²
 ┌──────────┬───────────┬────────────┬────────────┬───────────┐
 │          │           │            │            │
Context   Semantic   Cognition   Orchestration  Runtime
 │          │           │            │            │
CLM        CSL       z0-like      Herdsman     Omarchy-like
 │          │           │            │            │
 │          │           │            │      ┌─────┼─────┐
 │          │           │            │      │     │     │
 │          │           │            │   harness model account
 │          │           │            │      │     │     │
 └──────────┴───────────┴────────────┴──────┴─────┴─────┘
                              │
                              ▼
                       Resource Authority
                  CPU / RAM / quota / cost / IO
                              │
                              ▼
                       Execution Plane
                              │
                    Mr Boxington / BuildBuddy

Yo llamaría la nueva pieza:

Agent Runtime Plane

o quizá:

Harness Control Plane

⸻

Qué copiaría concretamente de Omarchy

Omarchy	OMP²
lazy harness launchers	HarnessAdapter
default agent	default RuntimeProfile
multiple subscriptions	AccountProfile
active account pinned per session	AccountLease
quota dashboard	QuotaTelemetry
autoswitch	QuotaAwareScheduler
common usage records	AgentTelemetry schema
cross-machine merge	distributed EventLog
cross-harness skill symlinks	canonical Skill + adapters
ori harness + arbitrary model	explicit harness/model separation

Mi conclusión

Omarchy no aporta mucho a P14 semantic intelligence, pero sí añade una dimensión que nuestro P15 todavía estaba tratando demasiado implícitamente:

orquestar agentes no basta; también hay que orquestar los runtimes, modelos, credenciales, cuotas y capacidades sobre los que esos agentes viven.

Herdsman responde:

who owns the work?

z0:

who may authorize the action?

Mr. Boxington:

who should actually recompute it?

y Omarchy aporta:

on WHICH harness,
WHICH model,
WHICH account,
with HOW MUCH quota
should this execution run?

Eso sí lo incorporaría formalmente a P15, y especialmente AccountLease + QuotaAwareScheduler + HarnessAdapter me parecen adiciones concretas, no solo inspiración.

Sí. tuicr aporta una pieza bastante más importante para OMP² que “otra TUI bonita”: formaliza un review loop compartido entre humano y agente.

La idea central es:

agent produces change
        ↓
human inspects exact diff
        ↓
human leaves structured findings
        ↓
agent reads those findings programmatically
        ↓
agent fixes
        ↓
repeat

tuicr guarda sesiones persistentes de review, comentarios a nivel de review/archivo/línea/rango, estado de archivos/hunks revisados y los expone mediante CLI JSON y una API Rust. Además puede enviar el review a GitHub/GitLab/Gitea/Bitbucket/Azure DevOps/Gerrit o entregarlo directamente a un coding agent. 

Eso me hace pensar que nos falta una representación explícita en OMP²: ReviewIR.

ReviewIR

Hasta ahora llevábamos aproximadamente:

CodeIR
  ↓
QueryIR
  ↓
DecisionIR
  ↓
ChangeIR
  ↓
VerificationIR

Pero review y verification no son lo mismo.

Un test puede pasar y un humano todavía decir:

esta API está mal diseñada.

Y un reviewer puede aprobar una idea que después falla un property test.

Yo lo cambiaría a:

CodeIR
   ↓
QueryIR
   ↓
DecisionIR
   ↓
ChangeIR
   │
   ├─────────────→ ReviewIR
   │                  │
   │                  ├─ human findings
   │                  ├─ agent findings
   │                  ├─ coverage
   │                  └─ disposition
   │
   └─────────────→ VerificationIR
                      │
                      ├─ compile
                      ├─ tests
                      ├─ proof
                      └─ runtime evidence

y ambos alimentan:

          ReviewIR
              │
              ├──────┐
              │      │
              ▼      ▼
          Decision / Approval
              ▲
              │
       VerificationIR

⸻

1. La sesión de review es un objeto real

Esto es lo mejor de tuicr.

No trata los comentarios como texto efímero copiado al prompt.

Existe una review session persistida que puede:

* listarse;
* reabrirse;
* recibir nuevos comentarios;
* distinguir sesiones activas;
* conservar qué archivos fueron revisados;
* sobrevivir al cierre del TUI. 

Conceptualmente:

ReviewSession
     │
     ├── target
     ├── diff
     ├── participants
     ├── findings[]
     ├── coverage
     ├── lifecycle
     └── verdict

Para OMP²:

ReviewSession {
    id: ReviewRef,
    target: ChangeRef,
    baseline: ArtifactRef,
    candidate: ArtifactRef,
    reviewers: Vec<ActorRef>,
    findings: Vec<ReviewFinding>,
    coverage: ReviewCoverage,
    disposition: ReviewDisposition,
}

Esto encaja muchísimo mejor con nuestro EventLog/ResultRef que:

"paste these comments into Claude"

⸻

2. El feedback ya tiene estructura

Su CLI devuelve JSON con:

id
path
start_line
end_line
side
comment_type
author
lifecycle_state
content

y permite comments de:

review
file
line
line_range

Eso nos da casi directamente:

ReviewFinding {
    id,
    reviewer,
    target,
    severity,
    category,
    lifecycle,
    content,
}

Yo ampliaría las categorías:

BlockingIssue
Suggestion
Question
Nit
DesignConcern
SecurityConcern
PerformanceConcern
VerificationGap
Praise

y el lifecycle:

Open
Acknowledged
Fixed
RejectedWithReason
Waived
Obsolete
VerifiedResolved

Aquí RejectedWithReason es particularmente importante: el agente no debería interpretar todo review como orden obligatoria.

⸻

3. Pero las líneas no pueden ser nuestra identidad principal

Aquí es donde CSL puede superar bastante a tuicr.

Tuicr utiliza anchors tipo:

src/foo.rs:42
src/foo.rs:50-55

que funcionan perfectamente para una UI de diff.

Pero después de que el agente edita:

42 → 57

el anchor puede perder significado.

En OMP² usaría:

ReviewAnchor
   ├── semantic anchor
   │      SymbolId
   │      AST node
   │      ChangeIR operation
   │
   ├── diff anchor
   │      hunk
   │      old/new range
   │
   └── textual fallback
          path:line

Ejemplo:

Finding F17
symbol:
  PendingItemService.process()
change:
  ChangeIR.op[12]
fallback:
  src/service.rs:42-48

Entonces incluso después de una modificación podemos intentar:

semantic relocate(F17)

en lugar de perder el comentario.

Éste es un punto donde tuicr + CSL juntos son muchísimo más fuertes que tuicr solo.

⸻

4. Tiene ya un buen human-agent loop

El skill de tuicr puede abrir la TUI en:

* cmux;
* tmux;
* Zellij;
* Herdr;

y después el agente puede adjuntarse a la sesión y leer los comentarios estructurados. 

Fíjate cómo empiezan a conectarse los repos que has enviado:

Herdsman
    ↓
agent/work ownership
tuicr
    ↓
human review workspace
gh-dash-like Operator Plane
    ↓
system-wide control

Podríamos tener:

                        Operator Plane
       Tasks       Agents       Reviews       Approvals
         │           │             │              │
         │           │             ▼              │
         │           │        ReviewSession       │
         │           │             │              │
         │           └───── Agent A ◄─────────────┘
         │                          │
         └──────────────────────────┘

En otras palabras, el review deja de ocurrir dentro del chat.

Eso me gusta mucho.

⸻

5. Hay una limitación que OMP² debería corregir: polling

El skill dice explícitamente que tuicr no tiene push stream hacia el agente. El agente consulta:

tuicr review comments ...

y, mientras espera, recomienda polling aproximadamente cada 30 segundos. 

Para una herramienta independiente está bien.

OMP² ya tendría EventLog, así que haría:

ReviewFindingAdded
ReviewFindingUpdated
ReviewCompleted

como eventos:

Human TUI
    │
    ▼
ReviewStore
    │
    ▼
EventLog
    │
    ├────────────► Agent mailbox
    └────────────► Operator Plane

Sin polling.

Esto conecta directamente con P15/Herdsman:

Agent waits on mailbox
rather than polling filesystem.

⸻

6. review coverage también debería ser first-class

Hay un detalle sutil muy bueno.

Tuicr distingue:

reviewed_count
file_count

y el skill dice que:

reviewed_count == file_count
+
zero comments

es una review válida que significa “nothing to flag”, no “el usuario no hizo nada”. 

Esto es importante semánticamente.

Porque:

no findings

puede significar dos cosas completamente diferentes:

A) reviewer inspected everything
   and found nothing
B) reviewer inspected nothing

OMP² debe conservar:

ReviewCoverage {
    total_units,
    reviewed_units,
    skipped_units,
}

Nunca inferir:

findings.empty()
→ approved

Eso encaja muy bien con nuestro principio:

absence of evidence
≠
evidence of absence

⸻

7. Multi-VCS es otra buena decisión arquitectónica

Tuicr soporta:

git
jj
Mercurial

y varios forges por adapters. 

Esto señala que OMP² tampoco debería hacer:

ChangeIR = git diff

Necesitamos:

VcsAdapter
     │
     ▼
CanonicalDiff
     │
     ▼
ChangeIR

Entonces:

Git ──────┐
jj ───────┼─→ DiffRef → ChangeIR
Mercurial ┘

Igualmente:

GitHub ────────┐
GitLab ────────┤
Gerrit ────────┤
Azure DevOps ──┼→ ForgeReviewAdapter
Bitbucket ─────┤
Gitea ─────────┘

Esto combina bien con tu ambiente donde además de GitHub hemos hablado bastante de GitLab.

⸻

8. El modelo local draft → submit me gusta mucho

Los comentarios pueden permanecer como draft local antes de enviar algo al forge. Tuicr permite luego:

Comment
Approve
Request changes
Draft

según las capacidades del forge. 

En OMP² yo separaría:

Finding
    ↓
Disposition
    ↓
ExternalPublication

Porque:

agent found a potential bug

no implica:

post publicly on PR now

Debe existir:

Local
Reviewed
ApprovedForPublication
Published

Esto es nuevamente la filosofía z0:

knowledge
≠
authority to act externally

⸻

9. Creo que sí vale la pena considerar integración real

Con gh-dash dije:

copy UX,
don't integrate

Con tuicr sería distinto.

Está escrito en Rust, bajo MIT, y expone una API Rust para manipular su ReviewStore directamente. 

Así que contemplaría tres opciones:

A. external CLI adapter
       easiest
B. use tuicr Rust library
       tighter integration
C. implement our own review UI
       probably unnecessary initially

Yo empezaría con A, evaluaría B, y no haría C hasta encontrar una limitación real.

Es exactamente el tipo de componente donde OMP² debería evitar NIH.

⸻

10. tuicr + gh-dash no son redundantes

Los pondría en niveles diferentes:

gh-dash-style
        │
        ▼
whole-system Operator Plane
Tasks
Agents
Builds
Reviews
Approvals
Failures
Quota
        │
        ▼
tuicr-style
        │
        ▼
focused Change Review Surface
files
hunks
lines
comments
approval

Así:

OMP² Dashboard
Reviews (3)
 ├─ R17  P14 semantic planner
 ├─ R18  scheduler resource admission
 └─ R19  context checkpointing
               │
               ▼
            Enter
               │
               ▼
             tuicr

Eso me parece una UX muy fuerte.

⸻

Actualización que haría al diseño

Añadiría formalmente:

P14.R — Review Semantics
R1  ReviewRef / ReviewSession
R2  ChangeRef-bound review targets
R3  structured findings
R4  semantic anchors
R5  comment lifecycle
R6  coverage accounting
R7  human vs agent provenance
R8  ReviewIR ↔ ChangeIR
R9  ReviewIR ↔ VerificationIR
R10 forge adapters
R11 EventLog notifications
R12 external review surface adapter

Y nuestra cadena quedaría aproximadamente:

                   Evidence
                      │
                      ▼
CodeIR → QueryIR → DecisionIR → ChangeIR
                                 │   │
                         ┌───────┘   └────────┐
                         ▼                    ▼
                     ReviewIR          VerificationIR
                         │                    │
                         └────────┬───────────┘
                                  ▼
                           ApprovalDecision
                                  │
                        ┌─────────┴─────────┐
                      revise             accept
                        │                   │
                        └──→ ChangeIR       ▼
                                      publish/merge

De los últimos enlaces, tuicr sí descubre una pieza conceptual que yo considero faltante: hasta ahora habíamos formalizado cómo el agente entiende, decide, cambia y verifica código; no habíamos formalizado suficientemente cómo otro actor —humano o agente— revisa ese cambio y devuelve feedback estructurado antes de aceptarlo.

ReviewIR llena exactamente ese hueco.