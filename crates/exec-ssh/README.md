# omp-exec-ssh

SSH execution backend for `omp-exec`, using envd's configured `SshService` authority.

The backend never shells out to an SSH binary. Host keys, authentication policy, connection
limits, deadlines, and bounded stdout/stderr remain owned by envd; this crate only maps the
authority's result into `omp-exec` events and typed outcomes.
