//! TSP message recording for Tern's `surface-play`.
//!
//! With `OMP_TSP_RECORD=<file>`, every logical TSP message in both
//! directions is appended as one JSONL line
//! `{"t":ms,"dir":"out"|"in","verb":"f","params":{…},"body":{…}}`, the format
//! Tern's `surface-play` scenario command replays.

use std::{
	env,
	fs::{File, OpenOptions},
	io::{self, Write as _},
	path::Path,
	time::{SystemTime, UNIX_EPOCH},
};

use serde::{Serialize, Serializer, ser::SerializeMap};
use serde_json::value::RawValue;

/// Environment variable naming the recording file.
pub const RECORD_ENV: &str = "OMP_TSP_RECORD";

/// Direction of a recorded message.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
	/// Program → terminal.
	Out,
	/// Terminal → program.
	In,
}

struct Params<'a>(&'a [(&'a str, &'a str)]);

impl Serialize for Params<'_> {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		let mut map = serializer.serialize_map(Some(self.0.len()))?;
		for (key, value) in self.0 {
			map.serialize_entry(key, value)?;
		}
		map.end()
	}
}

#[derive(Serialize)]
#[serde(untagged)]
enum Body<'a> {
	Json(&'a RawValue),
	Text(&'a str),
}

#[derive(Serialize)]
struct Line<'a> {
	t:      u128,
	dir:    Direction,
	verb:   &'a str,
	params: Params<'a>,
	body:   Body<'a>,
}

/// An open recording.
#[derive(Debug)]
pub struct Recorder {
	file: File,
}

impl Recorder {
	/// Opens (creating) `path` for appending.
	///
	/// # Errors
	/// Returns the error opening the file.
	pub fn open(path: &Path) -> io::Result<Self> {
		Ok(Self { file: OpenOptions::new().create(true).append(true).open(path)? })
	}

	/// Opens the file `OMP_TSP_RECORD` names, if it is set and opens.
	#[must_use]
	pub fn from_env() -> Option<Self> {
		let path = env::var_os(RECORD_ENV)?;
		Self::open(Path::new(&path))
			.inspect_err(|error| {
				tracing::warn!(
					error = error as &dyn std::error::Error,
					"cannot open the TSP recording"
				);
			})
			.ok()
	}

	/// Appends one message. A JSON body is recorded as JSON, anything else
	/// (a base64 blob) as a string.
	///
	/// # Errors
	/// Returns the error writing the line.
	pub fn log(
		&mut self,
		dir: Direction,
		verb: &str,
		params: &[(&str, &str)],
		body: &[u8],
	) -> io::Result<()> {
		let text = String::from_utf8_lossy(body);
		let raw = serde_json::from_str::<&RawValue>(&text).ok();
		let line = Line {
			t: SystemTime::now()
				.duration_since(UNIX_EPOCH)
				.map_or(0, |elapsed| elapsed.as_millis()),
			dir,
			verb,
			params: Params(params),
			body: raw.map_or(Body::Text(&text), Body::Json),
		};
		serde_json::to_writer(&mut self.file, &line)?;
		self.file.write_all(b"\n")
	}
}

#[cfg(test)]
mod tests {
	use std::fs;

	use super::{Direction, Recorder};

	#[test]
	fn lines_carry_json_bodies_as_json_and_blobs_as_text() {
		let dir = tempfile::tempdir().expect("tempdir");
		let path = dir.path().join("rec.jsonl");
		let mut recorder = Recorder::open(&path).expect("open");
		recorder
			.log(Direction::Out, "f", &[], br#"{"sf":"s1","s":1,"ops":[]}"#)
			.expect("frame");
		recorder
			.log(Direction::Out, "b", &[("id", "ab"), ("mime", "image/png")], b"aGk=")
			.expect("blob");
		let text = fs::read_to_string(&path).expect("read");
		let lines: Vec<serde_json::Value> = text
			.lines()
			.map(|line| serde_json::from_str(line).expect("json line"))
			.collect();
		assert_eq!(lines[0]["dir"], "out");
		assert_eq!(lines[0]["verb"], "f");
		assert_eq!(lines[0]["body"]["sf"], "s1");
		assert_eq!(lines[1]["params"]["mime"], "image/png");
		assert_eq!(lines[1]["body"], "aGk=");
	}
}
