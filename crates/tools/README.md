# omp-tools

`omp-tools` implements OMP's revisioned built-in executors: document reads, hashline edits, persistent shell sessions, and workspace search.

Each tool owns or borrows an environment-side resource through a concrete generic adapter. Streaming arguments may prepare leases and previews, but only the explicit commitment frame authorizes effects. Durable payloads remain dialect-neutral truth; model-facing parts are deterministic projections produced by the tool revision that created them.

Every spec states where its effects happen (`omp_tool::Confinement`). A tool is `Host` unless every effect its declaration leaves out runs in a process spawned under the environment's exec sandbox (or an in-process shell builtin checked by that sandbox's path policy): only `bash@2` and `hub@2` declare `ExecSandbox`, and envd's `only_sandbox_spawning_tools_are_exec_sandboxed` test fails if another tool claims it. Approval reads the marker, so claiming `ExecSandbox` without spawning through the sandbox would let a sandbox-kept default `yolo` auto-approve host effects.
