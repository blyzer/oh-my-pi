# 60 Python footprint: vendored interpreter and built binaries

> Part of the OMP2 gateway-capabilities research. Measurement for deliverable 8. Audited commit `85d219ae7f`.

## Commands
```
cd /Users/enverfrancisco/repositories/oh-my-pi
du -sh vendor/python*/install
for d in vendor/python*/; do cat $d/pyo3-config.txt | head -12; done
stat -f '%N size=%z bytes mtime=%Sm' target/release/omp target/debug/omp   # only if they exist; nothing was built
```
## Environment
```
arm64
Darwin 27.2.0
17179869184
10
23:00  up 3 days, 11:24, 1 user, load averages: 7.43 14.91 16.16
/opt/homebrew/bin/python3
Python 3.15.0
```
## Raw output (vendored interpreter)
```
182M	vendor/python-release/install
166M	vendor/python/install
vendor/python
vendor/python-release
vendor/python-release/
implementation=CPython
version=3.14
shared=false
abi3=false
lib_name=python3.14t
lib_dir=/Users/enverfrancisco/repositories/oh-my-pi/vendor/python-release/install/lib/python3.14t/config-3.14t-darwin
executable=/Users/enverfrancisco/repositories/oh-my-pi/vendor/python-release/install/bin/python3.14t
pointer_width=64
build_flags=Py_GIL_DISABLED
suppress_build_script_link_lines=false
vendor/python/
implementation=CPython
version=3.14
shared=false
abi3=false
lib_name=python3.14td
lib_dir=/Users/enverfrancisco/repositories/oh-my-pi/vendor/python/install/lib/python3.14t/config-3.14td-darwin
executable=/Users/enverfrancisco/repositories/oh-my-pi/vendor/python/install/bin/python3.14td
pointer_width=64
build_flags=Py_DEBUG,Py_GIL_DISABLED
suppress_build_script_link_lines=false
```
## Raw output (built omp binaries under target)
```
total 24
drwxr-xr-x@  6 enverfrancisco  staff   192 Nov 25 02:07 .
drwxr-xr-x@ 56 enverfrancisco  staff  1792 Nov 27 12:10 ..
-rw-r--r--@  1 enverfrancisco  staff  7159 Nov 27 22:18 .rustc_info.json
-rw-r--r--@  1 enverfrancisco  staff   177 Nov 25 02:03 CACHEDIR.TAG
drwxr-xr-x@  8 enverfrancisco  staff   256 Nov 25 02:07 debug
drwxr-xr-x@  2 enverfrancisco  staff    64 Nov 25 02:07 tmp
done
```
## What this shows
`du -sh` is the size of the local python-build-standalone install tree, a build input, not a shipped artifact. `stat` sizes are of whatever binary happened to be built earlier; their build profile and age are unknown (see mtime), so they are not a baseline for a binary size delta. The size of the interpreter inside the binary (stdlib blob plus static CPython) was `not measured`: it needs one release build with and without `omp-py` (see deliverable 8 plan).

Back to the [index](../../README.md).

## Addendum: other built binaries found
```
$ stat -f '%N size=%z bytes mtime=%Sm' /private/tmp/omp-shared-target/debug/omp_e2e_host
/private/tmp/omp-shared-target/debug/omp_e2e_host size=735424144 bytes mtime=Nov 27 22:42:17 2026
stat: target/debug/omp_e2e_host: stat: No such file or directory
```
`omp_e2e_host` is a debug-profile e2e test host that re-enters the Python child roles (`crates/e2e/src/bin/omp_e2e_host.rs`); it is not the `omp` binary and a debug build is not a size baseline.
