//! Owner-local dispatch tests.
//!
//! One module, split across two files so each stays under the
//! module-size budget. Paths remain `server::tests::*`.
include!("tests/protocol.rs");
include!("tests/probes.rs");
