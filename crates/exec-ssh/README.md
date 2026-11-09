# omp-exec-ssh

SSH execution backend for `omp-exec`, using `omp-ssh`'s configured `SshService`.

The backend never shells out to an SSH binary and does not depend on `omp-envd`. Host keys,
authentication policy, connection limits, deadlines, and bounded stdout/stderr stay in
`omp-ssh`; this crate maps that result into `omp-exec` events and typed outcomes.
