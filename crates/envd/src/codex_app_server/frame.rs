//! Re-exports [`crate::line_reader`] under the name the Codex adapter uses.
//!
//! `LineReader` and `FrameError` now live in `crate::line_reader`; this shim
//! keeps the Codex adapter's internal imports unchanged.
pub(super) use crate::line_reader::{FrameError, LineReader};
