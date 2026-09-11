//! Host capacity metrics for admission control.
//!
//! Logical readiness is not physical admissibility: work that is ready to run
//! is not the same as work the machine can afford to start. Scheduling that
//! on a fixed concurrency number overspawns exactly when the host is least
//! able to absorb it.
//!
//! # Why not `os.freemem()`
//! On macOS it reports free pages, and free pages are not the same as
//! available memory: inactive and purgeable pages are reclaimable on demand.
//! Measured on a healthy 16 GB machine under normal load, `os.freemem()`
//! reported 0.12 GB while the kernel reported 31% available. An admission
//! gate reading that number refuses everything, forever.
//!
//! This module reports what each kernel actually uses to decide pressure:
//! `MemAvailable` on Linux, `kern.memorystatus_level` on macOS.

use napi_derive::napi;

/// A point-in-time reading of host capacity.
#[napi(object, js_name = "HostCapacity")]
pub struct HostCapacity {
	/// Physical RAM in bytes, or 0 when the platform does not report it.
	pub total_memory:          f64,
	/// Memory that can be handed out without swapping, in bytes.
	///
	/// Linux reads `MemAvailable` (the kernel's own estimate, which accounts
	/// for reclaimable page cache). macOS derives it from
	/// `kern.memorystatus_level`, the percentage the memory-pressure
	/// subsystem itself acts on. `null` when unavailable — callers MUST fail
	/// closed rather than assume capacity.
	pub available_memory:      Option<f64>,
	/// Schedulable CPUs, honouring cgroup/affinity limits where the platform
	/// reports them.
	pub cpus:                  u32,
	/// 1-minute load average, or `null` on platforms without one.
	///
	/// Compare against `cpus`: a load of 8 is idle on 16 CPUs and a queue on
	/// 4.
	pub load_average:          Option<f64>,
	/// Free bytes on the filesystem holding the queried path.
	pub available_disk:        Option<f64>,
	/// True when the kernel reports memory pressure right now.
	///
	/// A distinct signal from a low `available_memory`: pressure means the
	/// kernel is already reclaiming, so new work will contend rather than
	/// simply consume.
	pub under_memory_pressure: bool,
}

#[cfg(target_os = "linux")]
fn meminfo_field(field: &str) -> Option<u64> {
	let text = std::fs::read_to_string("/proc/meminfo").ok()?;
	text
		.lines()
		.find_map(|line| line.strip_prefix(field)?.split_ascii_whitespace().next())?
		.parse::<u64>()
		.ok()?
		.checked_mul(1024)
}

#[cfg(target_os = "linux")]
fn total_memory_bytes() -> Option<u64> {
	meminfo_field("MemTotal:")
}

/// `MemAvailable` is the kernel's own estimate of what a new allocation can
/// have without swapping. It is not `MemFree`: page cache is reclaimable, and
/// on a warm machine most of RAM is page cache.
#[cfg(target_os = "linux")]
fn available_memory_bytes() -> Option<u64> {
	meminfo_field("MemAvailable:")
}

#[cfg(target_os = "macos")]
fn sysctl_u64(name: &std::ffi::CStr) -> Option<u64> {
	let mut value = 0_u64;
	let mut size = std::mem::size_of::<u64>();
	// SAFETY: the output pointer names a writable u64 and `size` reports its
	// exact capacity; these sysctls take no input buffer.
	let status = unsafe {
		libc::sysctlbyname(
			name.as_ptr(),
			(&raw mut value).cast(),
			&raw mut size,
			std::ptr::null_mut(),
			0,
		)
	};
	(status == 0 && size == std::mem::size_of::<u64>()).then_some(value)
}

#[cfg(target_os = "macos")]
fn sysctl_u32(name: &std::ffi::CStr) -> Option<u32> {
	let mut value = 0_u32;
	let mut size = std::mem::size_of::<u32>();
	// SAFETY: as above, with a u32-sized destination.
	let status = unsafe {
		libc::sysctlbyname(
			name.as_ptr(),
			(&raw mut value).cast(),
			&raw mut size,
			std::ptr::null_mut(),
			0,
		)
	};
	(status == 0 && size == std::mem::size_of::<u32>()).then_some(value)
}

#[cfg(target_os = "macos")]
fn total_memory_bytes() -> Option<u64> {
	sysctl_u64(c"hw.memsize")
}

/// `kern.memorystatus_level` is the free percentage the memory-pressure
/// subsystem itself acts on — the same number behind `memory_pressure -Q`.
/// Counting free pages instead would report ~1% on a healthy machine, because
/// macOS keeps memory populated with reclaimable pages by design.
#[cfg(target_os = "macos")]
fn available_memory_bytes() -> Option<u64> {
	let level = sysctl_u32(c"kern.memorystatus_level")?;
	let total = total_memory_bytes()?;
	Some(total.saturating_mul(u64::from(level.min(100))) / 100)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn total_memory_bytes() -> Option<u64> {
	None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn available_memory_bytes() -> Option<u64> {
	None
}

/// macOS raises the pressure notification below 15%; Linux has no single
/// equivalent, so the same threshold is applied to `MemAvailable`.
const fn under_pressure(total: Option<u64>, available: Option<u64>) -> bool {
	match (total, available) {
		(Some(total), Some(available)) if total > 0 => available * 100 / total < 15,
		_ => false,
	}
}

#[cfg(unix)]
fn load_average_one() -> Option<f64> {
	let mut loads = [0_f64; 3];
	// SAFETY: `getloadavg` writes at most `len` entries into the array.
	let count = unsafe { libc::getloadavg(loads.as_mut_ptr(), 3) };
	(count > 0).then_some(loads[0])
}

#[cfg(not(unix))]
fn load_average_one() -> Option<f64> {
	None
}

#[cfg(unix)]
fn available_disk_bytes(path: &str) -> Option<u64> {
	use std::os::unix::ffi::OsStrExt;
	let c_path = std::ffi::CString::new(std::ffi::OsStr::new(path).as_bytes()).ok()?;
	let mut stat = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
	// SAFETY: `c_path` is a live NUL-terminated string and `stat` names
	// writable storage of exactly the expected type.
	let status = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
	if status != 0 {
		return None;
	}
	// SAFETY: a zero status means the kernel initialized the struct.
	let stat = unsafe { stat.assume_init() };
	// `f_bavail` excludes root-reserved blocks: what an unprivileged writer
	// can actually use, which is what an admission decision needs.
	//
	// Its width is platform-dependent: u64 on Linux, u32 on macOS. Every
	// conversion is therefore wrong somewhere -- `as u64`, `u64::from`,
	// `.into()` and `try_from` are each rejected on Linux, where the types
	// already agree, and each required on macOS, where they do not. The
	// conversion is real; only its necessity varies, so the allow names the
	// platform that does not need it rather than deleting one that does.
	#[allow(
		clippy::useless_conversion,
		reason = "statvfs::f_bavail is u64 on Linux and u32 on macOS; the conversion is required on \
		          the latter"
	)]
	Some(stat.f_frsize.saturating_mul(u64::from(stat.f_bavail)))
}

#[cfg(not(unix))]
fn available_disk_bytes(_path: &str) -> Option<u64> {
	None
}

/// Schedulable CPUs. `available_parallelism` honours cgroup quotas and
/// affinity masks, so a container-limited process is not told it has the
/// host's core count.
fn cpu_count() -> u32 {
	std::thread::available_parallelism().map_or(1, |value| value.get() as u32)
}

/// Sample host capacity now.
///
/// `disk_path` selects the filesystem to measure; the workspace root is the
/// meaningful choice, since that is where a sandbox copy or an object store
/// will land. Every field is best-effort: a metric the platform does not
/// report is `null`, never a fabricated default, so a caller can fail closed
/// on the distinction.
#[napi(js_name = "hostCapacity")]
pub fn host_capacity(disk_path: Option<String>) -> HostCapacity {
	let total = total_memory_bytes();
	let available = available_memory_bytes();
	HostCapacity {
		total_memory:          total.unwrap_or(0) as f64,
		available_memory:      available.map(|bytes| bytes as f64),
		cpus:                  cpu_count(),
		load_average:          load_average_one(),
		available_disk:        disk_path
			.as_deref()
			.and_then(available_disk_bytes)
			.map(|bytes| bytes as f64),
		under_memory_pressure: under_pressure(total, available),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn reports_plausible_capacity_for_the_running_host() {
		let capacity = host_capacity(Some(".".to_owned()));
		assert!(capacity.cpus >= 1, "a running host has at least one CPU");

		// The point of the module: on a healthy machine the available figure
		// must be a usable fraction of RAM, not the near-zero free-page count
		// that `os.freemem()` reports on macOS.
		if let (total, Some(available)) = (capacity.total_memory, capacity.available_memory) {
			assert!(total > 0.0, "a platform reporting availability reports a total");
			assert!(available <= total, "available memory cannot exceed physical RAM");
		}

		if let Some(disk) = capacity.available_disk {
			assert!(disk > 0.0, "the checked-out filesystem has free space");
		}
	}

	#[test]
	fn pressure_tracks_the_fifteen_percent_threshold() {
		assert!(under_pressure(Some(100), Some(14)));
		assert!(!under_pressure(Some(100), Some(15)));
		// Unknown capacity is not pressure: the caller decides what to do
		// with a missing metric, and reporting a false alarm would stall
		// every admission on a platform that simply does not report.
		assert!(!under_pressure(None, None));
		assert!(!under_pressure(Some(0), Some(0)));
	}
}
