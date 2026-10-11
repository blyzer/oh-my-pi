//! Where a request can run: operating system, architecture, host needs, and
//! resources.

use omp_core::{SparseSet, sparse_index::TrySparseIndex};
use strum::{Display, EnumString, FromRepr, IntoStaticStr};

/// An operating system a request targets or a backend offers.
#[derive(
	Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Display, EnumString, IntoStaticStr,
)]
#[strum(serialize_all = "kebab-case", ascii_case_insensitive)]
pub enum Os {
	/// Linux.
	Linux,
	/// macOS.
	Macos,
	/// Windows.
	Windows,
}

/// A CPU architecture a request targets or a backend offers.
#[derive(
	Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Display, EnumString, IntoStaticStr,
)]
#[strum(ascii_case_insensitive)]
pub enum Arch {
	/// 64-bit x86.
	#[strum(serialize = "x86_64")]
	X86_64,
	/// 64-bit Arm.
	#[strum(serialize = "aarch64")]
	Aarch64,
}

/// One operating system and architecture pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Platform {
	/// The operating system.
	pub os:   Os,
	/// The architecture.
	pub arch: Arch,
}

impl Platform {
	/// Linux on 64-bit x86, the first remote platform (ADR 0040 Part G).
	pub const LINUX_X86_64: Self = Self { os: Os::Linux, arch: Arch::X86_64 };
	/// macOS on Apple silicon.
	pub const MACOS_AARCH64: Self = Self { os: Os::Macos, arch: Arch::Aarch64 };

	/// Pairs an operating system with an architecture.
	#[inline]
	pub const fn new(os: Os, arch: Arch) -> Self {
		Self { os, arch }
	}
}

/// A host capability a command needs beyond a plain process over its inputs.
///
/// Discriminants are stable bit positions in [`NeedSet`].
#[derive(
	Debug,
	Clone,
	Copy,
	PartialEq,
	Eq,
	Hash,
	PartialOrd,
	Ord,
	Display,
	EnumString,
	IntoStaticStr,
	FromRepr,
)]
#[repr(u8)]
#[strum(serialize_all = "kebab-case", ascii_case_insensitive)]
pub enum Need {
	/// A real pseudo-terminal (P7, TUI QA).
	Pty            = 0,
	/// The macOS Seatbelt sandbox (P12).
	Seatbelt       = 1,
	/// A container runtime.
	Docker         = 2,
	/// The macOS SDK (Apple linkers and frameworks).
	MacosSdk       = 3,
	/// A GPU.
	Gpu            = 4,
	/// Writes into the developer's workspace (`just fmt`, `cargo fix`,
	/// codegen). A remote action cannot write the local workspace, so it would
	/// exit 0 having changed nothing.
	WorkspaceWrite = 5,
	/// Timing measured on this host (P8 recordings); another machine's timing
	/// answers a different question.
	HostTiming     = 6,
}

/// A [`Need`] index that names no need.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("index does not name an execution need")]
pub struct NeedIndexError {
	/// The rejected index.
	pub index: usize,
}

impl TrySparseIndex for Need {
	type Error = NeedIndexError;

	#[inline]
	fn index(&self) -> usize {
		*self as usize
	}

	fn try_from_index(index: usize) -> Result<Self, Self::Error> {
		u8::try_from(index)
			.ok()
			.and_then(Self::from_repr)
			.ok_or(NeedIndexError { index })
	}
}

/// A set of [`Need`]s.
pub type NeedSet = SparseSet<Need>;

/// The platform a request must run on and what it needs from that host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformRequirement {
	/// Required operating system.
	pub os:            Os,
	/// Required architecture.
	pub arch:          Arch,
	/// Whether the command needs the network (cargo fetching crates) — what it
	/// needs, not what it tolerates.
	pub needs_network: bool,
	/// Host capabilities the command needs.
	pub needs:         NeedSet,
}

impl PlatformRequirement {
	/// The required operating system and architecture as one pair.
	#[inline]
	pub const fn platform(&self) -> Platform {
		Platform { os: self.os, arch: self.arch }
	}
}

/// Resources a request declares. Zero in a field means "not declared": the
/// backend applies its own default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResourceRequirements {
	/// CPU in thousandths of a core.
	pub cpu_millis:   u32,
	/// Memory in bytes.
	pub memory_bytes: u64,
	/// Scratch disk in bytes.
	pub disk_bytes:   u64,
}
