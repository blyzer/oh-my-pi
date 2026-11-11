# omp-ssh

Native SSH and SFTP sessions, plus the configured-host authority (`hosts.toml`).

envd used to own this. ADR 0049 D3.2 (ENVD-07) moves it here so `omp-exec-ssh`
can drive `SshService` without depending on `omp-envd`, and so `russh` is not
a dependency of the environment daemon. envd's `ssh://` reads stay in envd
behind the `cap-ssh` feature, which calls this crate. The feature is on by
default in `omp-app`. A build with `cap-ssh` off does not register the `ssh`
scheme.

The crate performs the SSH and SFTP I/O. It does not own the document
authority, the tool roster, or the execution-placement policy.
