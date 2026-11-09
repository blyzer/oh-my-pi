# omp-exec-local

Local `omp-exec` backend generic over the `omp_exec::ExecHost` contract.

The backend does not spawn processes itself and does not depend on `omp-envd`. It opens an
environment shell session, submits a quoted argv script through `ExecHost`, maps the host event
stream to `omp-exec` events, and relies on `ExecRun` cancellation for process-tree cleanup. envd's
concrete host implements the trait. The in-process proof is `crates/envd/tests/exec_local_backend.rs`.
