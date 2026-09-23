# omp-shell-builtins

`omp-shell-builtins` provides in-process command-line utilities and process-management commands for the OMP shell. It exposes separate registration lists for general utilities such as `cat`, `grep`, `sed`, `ls`, and checksum tools, and for process-oriented commands such as `pgrep`, `pkill`, `pidwait`, `ps`, `top`, `sleep`, `timeout`, and `nohup`. Its `omp-sh` binary is the batteries-included composition of these registries with `omp-shell`.

## Structure

- `factory` assembles the `utility_builtins` and `process_builtins` registration lists consumed by the shell engine.
- `host` adapts shell streams, working directory, exported environment, cancellation, argument parsing, and exit status to synchronous utility implementations.
- `proc_match` and `proc_snapshot` provide shared process discovery and matching support; `ProcInfo` and `ProcessStatus` are part of the crate's public API.
- Command modules implement individual filesystem, text-processing, checksum, system-information, and process-control builtins. `cksum` contains shared checksum machinery used by the digest commands.

## Filesystem assumptions in tests

Timestamp tests distinguish what a filesystem stores from what it keeps. A
modification stamp written by `touch` stays put, so it is asserted exactly
everywhere. An access stamp need not: on a volume mounted with access-time
updates enabled, something outside the test — an indexer, a scanner — can read
the file and move the stamp to the current time, and mere seconds are enough.

The CI runner is such a volume. Its tests run under `$TMPDIR`, which resolves to
`/System/Volumes/Data`, an APFS volume mounted without `noatime`, unlike
`/System/Volumes/VM` and the simulator volumes beside it. A stamp set there was
observed reverting to the current time within the same second, with no operation
performed on the file in between, while its modification stamp survived intact.
Measured there, the cause is a read from outside the test, which Spotlight and an
endpoint scanner on that machine both have reason to make, arriving within
milliseconds of a freshly written file's close. Without `strictatime` the volume
refreshes the access stamp on such a read only when it is not later than the
modification stamp. So a stamp can be lost even between a time-setting call and
a `stat` issued at once, or an `fstat` through a descriptor held across the call,
roughly once in a hundred.

So `touch`'s access-stamp tests carry a control file: stamped beside the
subject, never passed to the utility, and read beside it. It reports what the
volume did to an untouched file over the same interval. Where the control kept
its stamp, the contract is asserted exactly; where it did not, the subject is
held to the values it may legitimately carry, which still rejects a wrong stamp
the utility could write. This keeps the strict assertion wherever a filesystem
can support it rather than lowering it everywhere, and keeps Darwin covered
rather than skipped. Where a contract cannot be proven end to end on such a
volume, it is pinned instead at the seam that applies the stamps, read back with
no interval. Where even that read can miss, as for the time-setting calls
themselves, a miss must read as an access later than the stamp the file carried
going in, and the requested stamp must still read back exactly on at least one
of a few fresh attempts: nothing but the call can have written it there.

## Philosophy

Builtins run inside the shell so pipelines, redirections, the shell working directory, exported variables, and cancellation remain scoped to each command rather than relying on process-global state. General utilities and process-control commands stay independently selectable because embedders may choose different registration policies, including withholding destructive utilities.

The implementations were ported from `pi-builtins`, with individual commands also retaining attribution to sources such as uutils, findutils, and earlier `pi-shell` implementations. Keep ported code close enough to its upstream source to make maintenance practical, and preserve the source-level copyright, license, and attribution notices when updating it.
