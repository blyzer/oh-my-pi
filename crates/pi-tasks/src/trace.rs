//! Binary append-only trace: a fixed-stride event log plus an interned string
//! table.
//!
//! Text was the wrong wire here. A JSONL line re-spells `"phase":"plan"` and
//! `"kind":"phase_started"` on every event and forces a parse per line on every
//! reader poll. Instead:
//!
//! - **Every event is exactly [`RECORD_LEN`] bytes**, little-endian, so a
//!   reader seeks event `n` at `HEADER_LEN + n * RECORD_LEN` and tails by byte
//!   offset with zero parsing. Cursor == byte offset == sequence number.
//! - **Strings are content-interned once** into `strings.bin` and referenced by
//!   `u32` id, the same "store the hash, not the bytes" trade the workspace
//!   walker makes. A phase name repeated across 400 events costs 4 bytes each.
//! - **The run id is the directory**, not a field. What the key already tells
//!   you is not repeated per record.
//!
//! The encoding IS the ABI: [`OFF_TS`]..[`OFF_VALUE`] are the offsets a reader
//! in another language (a `DataView` in the TypeScript layer, a mmap in a UI
//! process) uses directly. No `unsafe`, no transmute — explicit
//! `to_le_bytes`/`from_le_bytes`, so layout is identical on every target.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// `"PITR"`, event log.
pub const EVENTS_MAGIC: u32 = 0x5049_5452;
/// `"PIST"`, string table.
pub const STRINGS_MAGIC: u32 = 0x5049_5354;
/// Bumped to 2 by `PhaseTokens`, to 3 by `PhaseRewound`. `verify_header`
/// requires an exact match, so a trace written by another version is refused
/// with its path rather than half-decoded: a resumable run is worth less than
/// a wrong one. A rewind in particular cannot be skipped — a reader that
/// ignored it would rebuild the wrong cursor.
pub const FORMAT_VERSION: u16 = 3;
/// Bytes before the first record in either file.
pub const HEADER_LEN: u64 = 16;
/// Stride of one event record.
pub const RECORD_LEN: usize = 32;

// ── Record ABI: byte offsets within one record ──────────────────────────────
pub const OFF_TS: usize = 0; // u64 unix millis
pub const OFF_KIND: usize = 8; // u8  EventKind
pub const OFF_FLAGS: usize = 9; // u8  bit0 = ok
pub const OFF_ATTEMPT: usize = 10; // u16
pub const OFF_PHASE: usize = 12; // u32 string id
pub const OFF_OWNER: usize = 16; // u32 string id
pub const OFF_GATE: usize = 20; // u32 string id
pub const OFF_DETAIL: usize = 24; // u32 string id
pub const OFF_VALUE: usize = 28; // u32 counter

const FLAG_OK: u8 = 1 << 0;
/// String id 0 is the empty string and is never written to the table, so an
/// absent field costs nothing.
pub const NO_STRING: u32 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EventKind {
	RunStarted = 1,
	PhaseStarted = 2,
	PhaseRetry = 3,
	GateCheck = 4,
	PhaseRejected = 5,
	PhaseFinished = 6,
	RunFinished = 7,
	/// One member of a fusion panel answered. The fan-out happens in the caller
	/// (only it can spawn a model), so without this record a fusion phase would
	/// be one opaque span instead of N comparable opinions.
	PanelOpinion = 8,
	/// A run picked up from this trace after dying mid-flight. Records after it
	/// belong to the continuation, so a terminal record is not necessarily the
	/// end of the story.
	RunResumed = 9,
	/// What one attempt of a phase cost, in tokens, reported by the caller —
	/// only it talks to a model. Its own record because `value` already carries
	/// the violation count on both rejection paths, and a rejected attempt
	/// spends tokens too: charging only the accepted one undercounts exactly
	/// the expensive part of a run.
	PhaseTokens = 10,
	/// The run went backwards: a phase was rejected and sent an earlier phase
	/// its failure instead of retrying in place. `phase` is the target, and
	/// `attempt` is the budget that target has now spent.
	///
	/// A reader cannot skip this: passed phases stop equalling the cursor the
	/// moment one of them runs twice, so resume derives position from these
	/// records rather than by counting finishes.
	PhaseRewound = 11,
}

impl EventKind {
	pub const fn from_u8(byte: u8) -> Option<Self> {
		match byte {
			1 => Some(Self::RunStarted),
			2 => Some(Self::PhaseStarted),
			3 => Some(Self::PhaseRetry),
			4 => Some(Self::GateCheck),
			5 => Some(Self::PhaseRejected),
			6 => Some(Self::PhaseFinished),
			7 => Some(Self::RunFinished),
			8 => Some(Self::PanelOpinion),
			9 => Some(Self::RunResumed),
			10 => Some(Self::PhaseTokens),
			11 => Some(Self::PhaseRewound),
			_ => None,
		}
	}
}

/// One decoded event. `ts_ms` is stamped at emit; `seq` is derived from the
/// record's position, never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
	pub seq: u64,
	pub ts_ms: u64,
	pub kind: EventKind,
	pub ok: bool,
	pub attempt: u16,
	pub phase: u32,
	pub owner: u32,
	pub gate: u32,
	pub detail: u32,
	pub value: u32,
}

/// Builder for the 32-byte record. String fields are ids obtained from
/// [`Tracer::intern`].
#[derive(Debug, Clone, Copy)]
pub struct EventRecord {
	kind: EventKind,
	ok: bool,
	attempt: u16,
	phase: u32,
	owner: u32,
	gate: u32,
	detail: u32,
	value: u32,
}

impl EventRecord {
	pub const fn new(kind: EventKind) -> Self {
		Self {
			kind,
			ok: false,
			attempt: 0,
			phase: NO_STRING,
			owner: NO_STRING,
			gate: NO_STRING,
			detail: NO_STRING,
			value: 0,
		}
	}

	pub const fn ok(mut self, ok: bool) -> Self {
		self.ok = ok;
		self
	}

	pub const fn attempt(mut self, attempt: u32) -> Self {
		self.attempt = attempt as u16;
		self
	}

	pub const fn phase(mut self, id: u32) -> Self {
		self.phase = id;
		self
	}

	pub const fn owner(mut self, id: u32) -> Self {
		self.owner = id;
		self
	}

	pub const fn gate(mut self, id: u32) -> Self {
		self.gate = id;
		self
	}

	pub const fn detail(mut self, id: u32) -> Self {
		self.detail = id;
		self
	}

	pub const fn value(mut self, value: u32) -> Self {
		self.value = value;
		self
	}

	fn encode(&self, ts_ms: u64) -> [u8; RECORD_LEN] {
		let mut buf = [0u8; RECORD_LEN];
		buf[OFF_TS..OFF_TS + 8].copy_from_slice(&ts_ms.to_le_bytes());
		buf[OFF_KIND] = self.kind as u8;
		buf[OFF_FLAGS] = u8::from(self.ok) * FLAG_OK;
		buf[OFF_ATTEMPT..OFF_ATTEMPT + 2].copy_from_slice(&self.attempt.to_le_bytes());
		buf[OFF_PHASE..OFF_PHASE + 4].copy_from_slice(&self.phase.to_le_bytes());
		buf[OFF_OWNER..OFF_OWNER + 4].copy_from_slice(&self.owner.to_le_bytes());
		buf[OFF_GATE..OFF_GATE + 4].copy_from_slice(&self.gate.to_le_bytes());
		buf[OFF_DETAIL..OFF_DETAIL + 4].copy_from_slice(&self.detail.to_le_bytes());
		buf[OFF_VALUE..OFF_VALUE + 4].copy_from_slice(&self.value.to_le_bytes());
		buf
	}
}

fn decode(seq: u64, buf: &[u8; RECORD_LEN]) -> std::io::Result<Event> {
	let kind = EventKind::from_u8(buf[OFF_KIND]).ok_or_else(|| {
		std::io::Error::new(std::io::ErrorKind::InvalidData, format!("unknown event kind {}", buf[OFF_KIND]))
	})?;
	Ok(Event {
		seq,
		ts_ms: u64::from_le_bytes(buf[OFF_TS..OFF_TS + 8].try_into().expect("8 bytes")),
		kind,
		ok: buf[OFF_FLAGS] & FLAG_OK != 0,
		attempt: u16::from_le_bytes(buf[OFF_ATTEMPT..OFF_ATTEMPT + 2].try_into().expect("2 bytes")),
		phase: u32::from_le_bytes(buf[OFF_PHASE..OFF_PHASE + 4].try_into().expect("4 bytes")),
		owner: u32::from_le_bytes(buf[OFF_OWNER..OFF_OWNER + 4].try_into().expect("4 bytes")),
		gate: u32::from_le_bytes(buf[OFF_GATE..OFF_GATE + 4].try_into().expect("4 bytes")),
		detail: u32::from_le_bytes(buf[OFF_DETAIL..OFF_DETAIL + 4].try_into().expect("4 bytes")),
		value: u32::from_le_bytes(buf[OFF_VALUE..OFF_VALUE + 4].try_into().expect("4 bytes")),
	})
}

fn header(magic: u32, record_len: u16) -> [u8; 16] {
	let mut buf = [0u8; 16];
	buf[0..4].copy_from_slice(&magic.to_le_bytes());
	buf[4..6].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
	buf[6..8].copy_from_slice(&record_len.to_le_bytes());
	buf
}

fn open_appending(path: &Path, magic: u32, record_len: u16) -> std::io::Result<File> {
	if let Some(parent) = path.parent() {
		std::fs::create_dir_all(parent)?;
	}
	let mut file = OpenOptions::new().create(true).read(true).append(true).open(path)?;
	if file.metadata()?.len() == 0 {
		file.write_all(&header(magic, record_len))?;
		file.flush()?;
	} else {
		verify_header(path, magic)?;
	}
	Ok(file)
}

fn verify_header(path: &Path, magic: u32) -> std::io::Result<()> {
	let mut file = File::open(path)?;
	let mut buf = [0u8; 16];
	file.read_exact(&mut buf)?;
	let found = u32::from_le_bytes(buf[0..4].try_into().expect("4 bytes"));
	let version = u16::from_le_bytes(buf[4..6].try_into().expect("2 bytes"));
	if found != magic || version != FORMAT_VERSION {
		return Err(std::io::Error::new(
			std::io::ErrorKind::InvalidData,
			format!("{}: not a v{FORMAT_VERSION} pi-tasks trace file", path.display()),
		));
	}
	Ok(())
}

struct StringTable {
	out: BufWriter<File>,
	ids: HashMap<Box<str>, u32>,
	next: u32,
}

impl StringTable {
	fn intern(&mut self, text: &str) -> std::io::Result<u32> {
		if text.is_empty() {
			return Ok(NO_STRING);
		}
		if let Some(id) = self.ids.get(text) {
			return Ok(*id);
		}
		let id = self.next;
		let bytes = text.as_bytes();
		self.out.write_all(&(bytes.len() as u32).to_le_bytes())?;
		self.out.write_all(bytes)?;
		self.out.flush()?;
		self.ids.insert(text.into(), id);
		self.next += 1;
		Ok(id)
	}
}

/// Writer half. Cheap to share: interning is a hash lookup after the first
/// sighting, and an emit is one 32-byte write.
pub struct Tracer {
	dir: PathBuf,
	events: Mutex<BufWriter<File>>,
	strings: Mutex<StringTable>,
	seq: AtomicU64,
}

impl Tracer {
	/// Opens (or creates) the trace pair inside `dir`. Reopening an existing
	/// run appends; existing records keep their sequence numbers.
	pub fn create(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
		let dir = dir.into();
		std::fs::create_dir_all(&dir)?;
		let events_path = dir.join("events.bin");
		let strings_path = dir.join("strings.bin");

		let events = open_appending(&events_path, EVENTS_MAGIC, RECORD_LEN as u16)?;
		let strings_file = open_appending(&strings_path, STRINGS_MAGIC, 0)?;
		let existing = events.metadata()?.len().saturating_sub(HEADER_LEN) / RECORD_LEN as u64;
		let known = read_strings(&strings_path)?;

		Ok(Self {
			dir,
			events: Mutex::new(BufWriter::new(events)),
			strings: Mutex::new(StringTable {
				out: BufWriter::new(strings_file),
				next: known.len() as u32 + 1,
				ids: known.into_iter().enumerate().map(|(i, s)| (s.into(), i as u32 + 1)).collect(),
			}),
			seq: AtomicU64::new(existing),
		})
	}

	pub fn dir(&self) -> &Path {
		&self.dir
	}

	/// Interns `text`, returning its stable id. Repeated strings cost one hash
	/// lookup and zero bytes.
	pub fn intern(&self, text: &str) -> std::io::Result<u32> {
		self.strings.lock().unwrap_or_else(std::sync::PoisonError::into_inner).intern(text)
	}

	/// Appends one record and flushes it, so a reader in another process sees
	/// the event while the phase is still running. Returns its sequence number.
	pub fn emit(&self, record: EventRecord) -> std::io::Result<u64> {
		let buf = record.encode(unix_millis());
		let mut out = self.events.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
		out.write_all(&buf)?;
		out.flush()?;
		Ok(self.seq.fetch_add(1, Ordering::SeqCst))
	}
}

fn read_strings(path: &Path) -> std::io::Result<Vec<String>> {
	let mut file = match File::open(path) {
		Ok(file) => file,
		Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
		Err(err) => return Err(err),
	};
	let mut bytes = Vec::new();
	file.read_to_end(&mut bytes)?;
	let mut out = Vec::new();
	let mut cursor = HEADER_LEN as usize;
	while cursor + 4 <= bytes.len() {
		let len = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().expect("4 bytes")) as usize;
		cursor += 4;
		if cursor + len > bytes.len() {
			break; // torn tail: a writer is mid-append
		}
		out.push(String::from_utf8_lossy(&bytes[cursor..cursor + len]).into_owned());
		cursor += len;
	}
	Ok(out)
}

/// Reader half — a separate process (UI, `omp` CLI, TypeScript binding) tails
/// by sequence number without parsing anything.
#[derive(Debug)]
pub struct TraceReader {
	dir: PathBuf,
	events: File,
	strings: Vec<String>,
}

impl TraceReader {
	pub fn open(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
		let dir = dir.into();
		let events_path = dir.join("events.bin");
		verify_header(&events_path, EVENTS_MAGIC)?;
		let strings = read_strings(&dir.join("strings.bin"))?;
		Ok(Self { events: File::open(&events_path)?, dir, strings })
	}

	/// Complete records currently on disk.
	pub fn count(&self) -> std::io::Result<u64> {
		Ok(self.events.metadata()?.len().saturating_sub(HEADER_LEN) / RECORD_LEN as u64)
	}

	/// Every complete record from `seq` onward. A partially written trailing
	/// record is left for the next call rather than decoded.
	pub fn read_from(&mut self, seq: u64) -> std::io::Result<Vec<Event>> {
		self.strings = read_strings(&self.dir.join("strings.bin"))?;
		let total = self.count()?;
		if seq >= total {
			return Ok(Vec::new());
		}
		self.events.seek(SeekFrom::Start(HEADER_LEN + seq * RECORD_LEN as u64))?;
		let mut out = Vec::with_capacity((total - seq) as usize);
		let mut buf = [0u8; RECORD_LEN];
		for index in seq..total {
			self.events.read_exact(&mut buf)?;
			out.push(decode(index, &buf)?);
		}
		Ok(out)
	}

	/// The same records as [`Self::read_from`], undecoded. This is the ABI
	/// path: the bytes go straight to a foreign reader (a `DataView` over an
	/// N-API `Buffer`, an mmap in a UI process) which indexes them with the
	/// `OFF_*` offsets. Rust never builds an intermediate object per event.
	pub fn read_raw(&mut self, seq: u64) -> std::io::Result<Vec<u8>> {
		self.strings = read_strings(&self.dir.join("strings.bin"))?;
		let total = self.count()?;
		if seq >= total {
			return Ok(Vec::new());
		}
		self.events.seek(SeekFrom::Start(HEADER_LEN + seq * RECORD_LEN as u64))?;
		let mut out = vec![0u8; (total - seq) as usize * RECORD_LEN];
		self.events.read_exact(&mut out)?;
		Ok(out)
	}

	/// The interned table in id order: `strings()[id - 1]`.
	pub fn strings(&self) -> &[String] {
		&self.strings
	}

	/// Resolves an interned id. Unknown ids resolve to `""`, never panic.
	pub fn text(&self, id: u32) -> &str {
		if id == NO_STRING {
			return "";
		}
		self.strings.get(id as usize - 1).map_or("", String::as_str)
	}
}

fn unix_millis() -> u64 {
	SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::test_support::TempDir;

	#[test]
	fn record_abi_offsets_are_stable() {
		// A reader in another language indexes these offsets directly; moving
		// one is a format break, not a refactor.
		assert_eq!(RECORD_LEN, 32);
		assert_eq!((OFF_TS, OFF_KIND, OFF_FLAGS, OFF_ATTEMPT), (0, 8, 9, 10));
		assert_eq!((OFF_PHASE, OFF_OWNER, OFF_GATE, OFF_DETAIL, OFF_VALUE), (12, 16, 20, 24, 28));

		let encoded = EventRecord::new(EventKind::GateCheck)
			.ok(true)
			.attempt(2)
			.phase(7)
			.owner(8)
			.gate(9)
			.detail(10)
			.value(11)
			.encode(0x0102_0304_0506_0708);
		assert_eq!(&encoded[OFF_TS..OFF_TS + 8], &0x0102_0304_0506_0708u64.to_le_bytes());
		assert_eq!(encoded[OFF_KIND], EventKind::GateCheck as u8);
		assert_eq!(encoded[OFF_FLAGS], 1);
		assert_eq!(u16::from_le_bytes(encoded[OFF_ATTEMPT..OFF_ATTEMPT + 2].try_into().unwrap()), 2);
		assert_eq!(u32::from_le_bytes(encoded[OFF_VALUE..OFF_VALUE + 4].try_into().unwrap()), 11);
	}

	#[test]
	fn events_round_trip_through_the_binary_log() {
		let dir = TempDir::new("trace-roundtrip");
		let tracer = Tracer::create(dir.path()).expect("create");
		let plan = tracer.intern("plan").expect("intern");
		let planner = tracer.intern("planner").expect("intern");

		assert_eq!(tracer.emit(EventRecord::new(EventKind::RunStarted)).expect("emit"), 0);
		assert_eq!(
			tracer
				.emit(EventRecord::new(EventKind::PhaseStarted).phase(plan).owner(planner).attempt(1))
				.expect("emit"),
			1
		);

		let mut reader = TraceReader::open(dir.path()).expect("open");
		let events = reader.read_from(0).expect("read");
		assert_eq!(events.len(), 2);
		assert_eq!(events[0].kind, EventKind::RunStarted);
		assert_eq!(events[1].kind, EventKind::PhaseStarted);
		assert_eq!(events[1].attempt, 1);
		assert_eq!(reader.text(events[1].phase), "plan");
		assert_eq!(reader.text(events[1].owner), "planner");
		assert_eq!(reader.text(NO_STRING), "");
	}

	#[test]
	fn repeated_strings_are_stored_once() {
		let dir = TempDir::new("trace-intern");
		let tracer = Tracer::create(dir.path()).expect("create");
		let first = tracer.intern("build").expect("intern");
		for _ in 0..64 {
			assert_eq!(tracer.intern("build").expect("intern"), first);
		}
		// header + one 4-byte length + "build"
		let size = std::fs::metadata(dir.path().join("strings.bin")).expect("meta").len();
		assert_eq!(size, HEADER_LEN + 4 + 5);
	}

	#[test]
	fn a_reader_tails_a_live_run_by_sequence() {
		let dir = TempDir::new("trace-tail");
		let tracer = Tracer::create(dir.path()).expect("create");
		let phase = tracer.intern("plan").expect("intern");
		tracer.emit(EventRecord::new(EventKind::PhaseStarted).phase(phase)).expect("emit");

		let mut reader = TraceReader::open(dir.path()).expect("open");
		let first = reader.read_from(0).expect("read");
		assert_eq!(first.len(), 1, "flushed before the run ended");

		// Writer keeps going against the same open reader.
		let gate = tracer.intern("artifacts_exist").expect("intern");
		tracer.emit(EventRecord::new(EventKind::GateCheck).phase(phase).gate(gate).ok(true)).expect("emit");

		let next = reader.read_from(1).expect("read");
		assert_eq!(next.len(), 1, "cursor resumes, no re-read");
		assert_eq!(next[0].seq, 1);
		assert_eq!(reader.text(next[0].gate), "artifacts_exist");
		assert!(reader.read_from(2).expect("read").is_empty());
	}

	#[test]
	fn reopening_a_run_continues_its_sequence() {
		let dir = TempDir::new("trace-reopen");
		let first = Tracer::create(dir.path()).expect("create");
		let id = first.intern("plan").expect("intern");
		first.emit(EventRecord::new(EventKind::RunStarted)).expect("emit");
		drop(first);

		let second = Tracer::create(dir.path()).expect("reopen");
		assert_eq!(second.intern("plan").expect("intern"), id, "table survives reopen");
		assert_eq!(second.emit(EventRecord::new(EventKind::RunFinished)).expect("emit"), 1);
		assert_eq!(TraceReader::open(dir.path()).expect("open").count().expect("count"), 2);
	}

	#[test]
	fn a_foreign_file_is_rejected_instead_of_decoded() {
		let dir = TempDir::new("trace-foreign");
		dir.write("events.bin", "not a trace file at all");
		let err = TraceReader::open(dir.path()).expect_err("must reject");
		assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
	}

	#[test]
	fn raw_records_carry_the_same_events_a_foreign_reader_can_index() {
		let dir = TempDir::new("trace-raw");
		let tracer = Tracer::create(dir.path()).expect("create");
		let phase = tracer.intern("plan").expect("intern");
		let owner = tracer.intern("planner").expect("intern");
		tracer.emit(EventRecord::new(EventKind::PhaseStarted).phase(phase).owner(owner)).expect("emit");
		tracer.emit(EventRecord::new(EventKind::RunFinished).ok(true)).expect("emit");

		let mut reader = TraceReader::open(dir.path()).expect("open");
		let raw = reader.read_raw(0).expect("raw");
		assert_eq!(raw.len(), 2 * RECORD_LEN, "fixed stride, no framing");

		// Index the second record the way a DataView would.
		let second = RECORD_LEN;
		assert_eq!(raw[second + OFF_KIND], EventKind::RunFinished as u8);
		assert_eq!(raw[second + OFF_FLAGS] & 1, 1);
		assert_eq!(u32::from_le_bytes(raw[OFF_PHASE..OFF_PHASE + 4].try_into().expect("4")), phase);

		assert_eq!(reader.strings(), ["plan", "planner"], "id order == table order");
		assert_eq!(reader.read_raw(1).expect("raw").len(), RECORD_LEN, "cursor resumes");
		assert!(reader.read_raw(2).expect("raw").is_empty());
	}
}
