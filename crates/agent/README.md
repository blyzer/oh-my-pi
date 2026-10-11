# omp-agent

Agent kernel: the turn loop, central tool dispatch, cancellation tree, and Directors, all folding into `omp-session`.

Environment `DoneProjected` terminals carry source-line `ProjectionSpan`
candidates alongside their bounded model parts. Central byte and line bounds
produce the final sorted, deduplicated `VisibilityReceipt`; only completely
retained rows are returned to the exact registered tool revision through
`Registry::authorize_visibility`. A receipt with no retained rows is still
returned when candidates were supplied. Terminals from older producers with
no candidates keep the existing no-authorization behavior. Typed verdict
decode failures and authority rejection propagate as dispatch errors.
For externally routed calls the executor also acknowledges that receipt with
the owning environment host. The host checks the exact revision and typed
verdict, refuses receipt lines absent from the tool's projection, and invokes
the registered authority. This application-only DATA operation requires
document-read authority and is unavailable to extension-host connections.
