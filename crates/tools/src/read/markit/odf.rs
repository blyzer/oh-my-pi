//! Shared `OpenDocument` package validation and conversion.
//!
//! ODT, ODS, and ODP use the same bounded ZIP/XML parser. Keeping the
//! package identity check and error adaptation here prevents the three
//! format frontends from growing subtly different container rules.

use omp_core::Str;
use quick_xml::{Error as XmlError, events::Event};

use super::{
	MarkitError, convert_with_anydoc,
	ooxml::{Archive, attribute, decode_xml_bytes, local_name, xml_reader},
};

/// Internal validation failure before conversion reaches the model-facing
/// [`MarkitError`] boundary.
#[derive(Debug, thiserror::Error)]
enum OdfError {
	#[error("unexpected OpenDocument mimetype '{declared}'")]
	UnexpectedMimetype { declared: String },
	#[error("unexpected OpenDocument body '{body}'")]
	UnexpectedBody { body: String },
	#[error("unsupported OpenDocument format selector")]
	UnsupportedFormat,
	#[error("content.xml has no recognized OpenDocument body")]
	MissingRecognizedBody,
	#[error("content.xml has no OpenDocument body")]
	MissingBody,
	#[error("invalid XML: {xml_error}")]
	InvalidXml { xml_error: String },
	#[error("invalid content.xml: {source}")]
	InvalidContentXml {
		#[source]
		source: XmlError,
	},
	#[error("invalid manifest XML: {source}")]
	InvalidManifestXml {
		#[source]
		source: XmlError,
	},
}

impl From<String> for OdfError {
	fn from(xml_error: String) -> Self {
		Self::InvalidXml { xml_error }
	}
}

/// Converts one `OpenDocument` package after validating its declared identity.
///
/// `mimetype` is mandatory in conforming ODF packages, but real producers
/// occasionally omit it and identify the package in the manifest instead.
/// The shared anydoc parser accepts that recovery case. When `mimetype` is
/// present, however, a conflicting package kind is rejected rather than
/// interpreting (for example) a spreadsheet through an `.odt` extension.
pub(super) fn convert(
	bytes: &[u8],
	format: anydoc::Format,
	format_name: &'static str,
	expected_mimetype: &'static str,
) -> Result<Str, MarkitError> {
	let mut archive =
		Archive::open(bytes).map_err(|error| MarkitError::conversion(format_name, error))?;
	// Parse the optional manifest once. Encryption has precedence over
	// content validation because encrypted content.xml bytes are not XML.
	// A syntactically corrupt optional manifest proves nothing; archive and
	// entity-expansion limit failures from `read_xml` still propagate.
	let manifest = archive
		.read_xml("META-INF/manifest.xml")
		.map_err(|error| MarkitError::conversion(format_name, error))?
		.and_then(|xml| parse_manifest(&xml).ok())
		.unwrap_or_default();
	if manifest.encrypted {
		return Err(MarkitError::conversion(format_name, "document is encrypted"));
	}
	let declared = if let Some(mimetype) = archive
		.read_xml("mimetype")
		.map_err(|error| MarkitError::conversion(format_name, error))?
	{
		Some(
			decode_xml_bytes(&mimetype)
				.map_err(|error| {
					MarkitError::conversion_in(format_name, "mimetype is not valid UTF text", error)
				})?
				.trim()
				.to_owned(),
		)
	} else {
		manifest.root_mimetype
	};
	if let Some(declared) = declared {
		validate_mimetype(&declared, expected_mimetype)
			.map_err(|error| MarkitError::conversion_from(format_name, error))?;
	}
	let content = archive
		.read_xml("content.xml")
		.map_err(|error| MarkitError::conversion(format_name, error))?
		.ok_or_else(|| {
			MarkitError::conversion(format_name, "Invalid OpenDocument package: missing content.xml")
		})?;
	validate_content_body(&content, format)
		.map_err(|error| MarkitError::conversion_from(format_name, error))?;
	drop(archive);

	convert_with_anydoc(bytes, format, format_name)
}

fn validate_mimetype(declared: &str, expected: &str) -> Result<(), OdfError> {
	if declared == expected
		|| declared
			.strip_suffix("-template")
			.is_some_and(|base| base == expected)
	{
		return Ok(());
	}
	Err(OdfError::UnexpectedMimetype { declared: declared.to_owned() })
}

/// Ensures a package selected as one ODF kind does not silently render a
/// different office body. This matters when both package identity parts are
/// absent or stale because anydoc deliberately shares one ODF parser.
fn validate_content_body(xml: &[u8], format: anydoc::Format) -> Result<(), OdfError> {
	let expected = match format {
		anydoc::Format::Odt => b"text".as_slice(),
		anydoc::Format::Ods => b"spreadsheet".as_slice(),
		anydoc::Format::Odp => b"presentation".as_slice(),
		_ => return Err(OdfError::UnsupportedFormat),
	};
	let mut reader = xml_reader(xml);
	let mut in_body = false;
	loop {
		match reader.read_event() {
			Ok(Event::Start(event)) => {
				let qname = event.name();
				let name = local_name(qname.as_ref());
				if in_body {
					if name == expected {
						return Ok(());
					}
					if is_odf_body(name) {
						return Err(OdfError::UnexpectedBody {
							body: String::from_utf8_lossy(name).into_owned(),
						});
					}
				} else if name == b"body" {
					in_body = true;
				}
			},
			Ok(Event::Empty(event)) if in_body => {
				let qname = event.name();
				let name = local_name(qname.as_ref());
				if name == expected {
					return Ok(());
				}
				if is_odf_body(name) {
					return Err(OdfError::UnexpectedBody {
						body: String::from_utf8_lossy(name).into_owned(),
					});
				}
			},
			Ok(Event::End(event)) if local_name(event.name().as_ref()) == b"body" => {
				return Err(OdfError::MissingRecognizedBody);
			},
			Ok(Event::Eof) => return Err(OdfError::MissingBody),
			Ok(_) => {},
			Err(error) => return Err(OdfError::InvalidContentXml { source: error }),
		}
	}
}

fn is_odf_body(name: &[u8]) -> bool {
	name == b"text" || name == b"spreadsheet" || name == b"presentation"
}

/// Identity and encryption facts carried by an ODF package manifest.
#[derive(Default)]
struct ManifestInfo {
	root_mimetype: Option<String>,
	encrypted:     bool,
}

/// Parses package-wide manifest facts without depending on namespace
/// prefixes. It deliberately scans to EOF after finding the root entry
/// because encryption data may occur on a later file entry.
fn parse_manifest(xml: &[u8]) -> Result<ManifestInfo, OdfError> {
	let mut reader = xml_reader(xml);
	let mut info = ManifestInfo::default();
	loop {
		match reader.read_event() {
			Ok(Event::Start(event) | Event::Empty(event)) => {
				let qname = event.name();
				let name = local_name(qname.as_ref());
				if name == b"encryption-data" {
					info.encrypted = true;
				} else if name == b"file-entry"
					&& attribute(&reader, &event, b"full-path")?.as_deref() == Some("/")
				{
					info.root_mimetype = attribute(&reader, &event, b"media-type")?;
				}
			},
			Ok(Event::Eof) => return Ok(info),
			Ok(_) => {},
			Err(error) => return Err(OdfError::InvalidManifestXml { source: error }),
		}
	}
}
