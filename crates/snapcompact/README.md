# omp-snapcompact

`omp-snapcompact` is OMP's pure-Rust bitmap archive renderer for context compaction. It rasterizes pre-normalized conversation text with bundled bitmap and TrueType fonts, then encodes compact PNG frames for model requests.

The crate owns bounded text-to-image rendering, sentence-colored and monochrome ink variants, repeated-line redundancy, configurable cell geometry, one- or two-column layouts, and dimmed tool-output spans. Its `archive` module adds provider-aware frame shapes, atomic data-URL elision, bounded text chunking, framing, and savings accounting. Rendering is deterministic and allocation-bounded, while text normalization, wrapping, and provider request construction remain caller responsibilities.

The producer lives in `omp-agent`: when a compaction runs with the `snapcompact` strategy (`/compact snapcompact`, or `ai_compaction_strategy snapcompact` for automatic and bare `/compact` runs), the `CompactionDirector` serializes the hidden history with `archive::push_normalized` and the dim/line-break markers, renders it through `render_archive` for the route's codec, provider, and model, stores the frames in the session CAS, and journals them on `compaction@1`. Routes without image input, `ai_vision off`, and archives that miss the frame or savings budget fall back to the soft summary with a notice.
