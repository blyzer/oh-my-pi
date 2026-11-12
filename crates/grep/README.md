# omp-grep

Binding-free regex search over in-memory bytes and workspace files.

- **Engine.** Matching runs on ripgrep's regex engine and falls back to PCRE2
  for look-around, backreferences, and other constructs Rust's regex automata
  reject. `OMP_PCRE2_JIT` controls PCRE2 JIT.
- **Entry points.** `search` and `search_with_cancellation` scan in-memory
  bytes. `grep` and `grep_with_cancellation` collect results for a file or
  directory. `grep_stream` and `CompiledGrep` stream records into a
  `GrepSink` with bounded output and deadline/cancellation support.
- **Boundaries.** Filesystem traversal and parallel worker ownership stay in
  `omp-walker`; this crate owns only matching, context lines, and result
  shaping. Failures are structured `GrepError` variants. Each file is read up
  to `MAX_FILE_BYTES`.
- **Consumers.** `omp-envd` depends on this crate for its workspace search
  tool; the crate has no dependency on envd, the agent, or any host resource.

Targeted checks: `just check-pkg omp-grep` and `just test-pkg omp-grep`.
