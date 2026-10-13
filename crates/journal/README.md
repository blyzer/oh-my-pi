# omp-journal

`omp-journal` owns the durable, crash-tolerant history of an omp session. A session is one flat `.oms` file containing raw Server-Sent Events frames, plus content-addressed blobs for payloads that should not be repeated inline.

The crate is intentionally structural rather than behavioral. It assigns monotonic identities, commits complete frames, recovers a torn tail, and selects a branch through `prior` links. It does not interpret session state: replay and materialization belong to the DOM/session layer, so the journal remains the sole durable authority without becoming a second state model.

Journal GC holds an exclusive namespace lease from authoritative `.oms` inventory through the content-addressed-store sweep. Complete branch histories, child jobs, checkpoints, and imported sessions retain their referenced blobs until their journal history is pruned. Dry-run and apply use the same age and reachability plan; traversal bounds are proven before CAS deletion begins, and cancellation stops at filesystem and destructive-operation boundaries.

Reads are bounded. `Journal::open`, `scan`, `scan_prefix` and GC inventory stream the file one frame at a time instead of loading it whole, and a frame that grows past `sse::FRAME_HARD_CAP` (a maximal 1 MiB payload plus 64 KiB of field lines) without its committing blank line fails with `JournalError::FrameTooLarge` before any more of it is buffered. The writer refuses to encode a frame above the same cap, so a journal this build wrote is never rejected by its own reader. A journal written by a newer omp (an unknown kind name or revision, or a genesis `version` above `data::GENESIS_VERSION`) fails closed with `JournalError::NewerRevision` / `NewerGenesisVersion`.
