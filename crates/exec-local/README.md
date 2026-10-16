# omp-exec-local

Local `omp-exec` backend backed by envd's `ExecHost` authority.

The backend does not spawn processes itself. It opens an environment shell session, submits a
quoted argv script through `ExecHost`, maps the host event stream to `omp-exec` events, and relies
on `ExecRun` RAII cancellation for process-tree cleanup.
