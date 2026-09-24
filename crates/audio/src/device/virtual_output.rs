//! In-process speaker with no hardware behind it.
//!
//! A dedicated thread requests one period of samples per period of wall time
//! and discards them, so the gapless queue, gain, drain, and abort semantics
//! of [`crate::audio::PlaybackStream`] run unchanged while nothing reaches a
//! real device. Hosts and tests that must not depend on the machine's audio
//! stack (headless runners, sleeping or missing output devices) open this
//! instead of a platform backend.

use std::{
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
	},
	thread,
	time::Duration,
};

use super::{DeviceConfig, PlaybackFill};
use crate::{VoiceError, VoiceResult};

/// Running virtual speaker clock.
pub struct VirtualPlayback {
	stopped: Arc<AtomicBool>,
	thread:  Option<thread::JoinHandle<()>>,
}

impl VirtualPlayback {
	/// Starts rendering `fill` at the configured period.
	pub(super) fn start(config: DeviceConfig, mut fill: PlaybackFill) -> VoiceResult<Self> {
		let period = Duration::from_millis(u64::from(config.period_ms));
		let mut output = vec![0.0; config.period_samples()];
		let stopped = Arc::new(AtomicBool::new(false));
		let thread_stopped = Arc::clone(&stopped);
		let thread = thread::Builder::new()
			.name("omp-audio-virtual-output".to_owned())
			.spawn(move || {
				while !thread_stopped.load(Ordering::Acquire) {
					fill(&mut output);
					thread::park_timeout(period);
				}
			})
			.map_err(|source| VoiceError::Backend { source: Arc::new(source) })?;
		Ok(Self { stopped, thread: Some(thread) })
	}

	/// Stops the clock and waits out any in-flight callback, like the native
	/// backends. Idempotent.
	pub(super) fn stop(&mut self) {
		self.stopped.store(true, Ordering::Release);
		if let Some(thread) = self.thread.take() {
			thread.thread().unpark();
			let _ = thread.join();
		}
	}
}

impl Drop for VirtualPlayback {
	fn drop(&mut self) {
		self.stop();
	}
}
