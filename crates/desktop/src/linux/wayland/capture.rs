use std::{cell::RefCell, io, os::fd::OwnedFd, rc::Rc};

use ashpd::desktop::{
	PersistMode,
	screencast::{CursorMode, Screencast, SourceType},
};
use image::RgbaImage;
use pipewire as pw;
use pipewire::{
	context::ContextRc,
	main_loop::MainLoopRc,
	spa::buffer,
	stream::{StreamBox, StreamFlags},
};
use pw::properties::properties;
use spa::{
	param::{
		self,
		video::{self, VideoInfoRaw},
	},
	pod,
	pod::serialize::PodSerializer,
	utils,
};
use tokio::runtime::Runtime;

use super::portal::{read_token, store_token};
use crate::error::{CoreResult, DesktopError, ErrorCode};

const SCREENCAST_TOKEN: &str = "screencast-token";

/// Failure while negotiating a Wayland screencast or reading a PipeWire frame;
/// rendered once at the `capture` boundary.
#[derive(Debug, thiserror::Error)]
enum WaylandCaptureError {
	#[error("ScreenCast portal")]
	Portal(#[source] ashpd::Error),
	#[error("ScreenCast CreateSession")]
	CreateSession(#[source] ashpd::Error),
	#[error("ScreenCast SelectSources")]
	SelectSources(#[source] ashpd::Error),
	#[error("ScreenCast Start")]
	Start(#[source] ashpd::Error),
	#[error("ScreenCast permission")]
	Permission(#[source] ashpd::Error),
	#[error("ScreenCast returned no monitor stream")]
	NoMonitorStream,
	#[error("ScreenCast OpenPipeWireRemote")]
	OpenPipeWireRemote(#[source] ashpd::Error),
	#[error("PipeWire main loop")]
	MainLoop(#[source] pw::Error),
	#[error("PipeWire context")]
	Context(#[source] pw::Error),
	#[error("PipeWire remote")]
	Remote(#[source] pw::Error),
	#[error("PipeWire stream")]
	Stream(#[source] pw::Error),
	#[error("PipeWire listener")]
	Listener(#[source] pw::Error),
	#[error("PipeWire format serialization")]
	FormatSerialization(#[source] pod::serialize::GenError),
	#[error("PipeWire rejected format parameters")]
	InvalidFormatPod,
	#[error("PipeWire connect")]
	Connect(#[source] pw::Error),
	#[error("PipeWire stream ended before producing a frame")]
	NoFrame,
	#[error("PipeWire negotiated an empty frame")]
	EmptyFrame,
	#[error("PipeWire returned unsupported frame stride {0}")]
	UnsupportedStride(i32),
	#[error("PipeWire frame buffer is not memory-mapped")]
	UnmappedBuffer,
	#[error("PipeWire frame size overflow")]
	FrameSizeOverflow,
	#[error("PipeWire frame offset is outside the mapped buffer")]
	FrameOffsetOutOfBounds,
	#[error("PipeWire negotiated unsupported pixel format {0:?}")]
	UnsupportedPixelFormat(video::VideoFormat),
	#[error("PipeWire row size overflow")]
	RowSizeOverflow,
	#[error("PipeWire frame buffer is short: {len} bytes for {width}x{height} stride {stride}")]
	ShortFrameBuffer { len: usize, width: u32, height: u32, stride: usize },
	#[error("failed to construct PipeWire RGBA frame")]
	FrameConstruction,
}

async fn open_screencast() -> Result<(u32, OwnedFd), WaylandCaptureError> {
	let portal = Screencast::new()
		.await
		.map_err(WaylandCaptureError::Portal)?;
	let session = portal
		.create_session()
		.await
		.map_err(WaylandCaptureError::CreateSession)?;
	let restore_token = read_token(SCREENCAST_TOKEN);
	portal
		.select_sources(
			&session,
			CursorMode::Embedded,
			SourceType::Monitor.into(),
			true,
			restore_token.as_deref(),
			PersistMode::ExplicitlyRevoked,
		)
		.await
		.map_err(WaylandCaptureError::SelectSources)?;
	let response = portal
		.start(&session, None)
		.await
		.map_err(WaylandCaptureError::Start)?
		.response()
		.map_err(WaylandCaptureError::Permission)?;
	store_token(SCREENCAST_TOKEN, response.restore_token());
	let stream = response
		.streams()
		.first()
		.ok_or(WaylandCaptureError::NoMonitorStream)?;
	let node = stream.pipe_wire_node_id();
	let fd = portal
		.open_pipe_wire_remote(&session)
		.await
		.map_err(WaylandCaptureError::OpenPipeWireRemote)?;
	Ok((node, fd))
}

struct UserData {
	format: VideoInfoRaw,
}

fn rgba_from_buffer(
	format: &VideoInfoRaw,
	data: &mut buffer::Data,
) -> Result<RgbaImage, WaylandCaptureError> {
	let size = format.size();
	let width = size.width;
	let height = size.height;
	if width == 0 || height == 0 {
		return Err(WaylandCaptureError::EmptyFrame);
	}
	let chunk = data.chunk();
	let offset = chunk.offset() as usize;
	let bytes = chunk.size() as usize;
	let stride = chunk.stride();
	if stride <= 0 {
		return Err(WaylandCaptureError::UnsupportedStride(stride));
	}
	let stride = stride as usize;
	let source = data.data().ok_or(WaylandCaptureError::UnmappedBuffer)?;
	let end = offset
		.checked_add(bytes)
		.ok_or(WaylandCaptureError::FrameSizeOverflow)?
		.min(source.len());
	let source = source
		.get(offset..end)
		.ok_or(WaylandCaptureError::FrameOffsetOutOfBounds)?;
	let pixel_size = match format.format() {
		video::VideoFormat::RGB | video::VideoFormat::BGR => 3,
		video::VideoFormat::RGBA
		| video::VideoFormat::RGBx
		| video::VideoFormat::BGRA
		| video::VideoFormat::BGRx => 4,
		other => return Err(WaylandCaptureError::UnsupportedPixelFormat(other)),
	};
	let row_bytes = (width as usize)
		.checked_mul(pixel_size)
		.ok_or(WaylandCaptureError::RowSizeOverflow)?;
	if stride < row_bytes || source.len() < stride.saturating_mul(height as usize) {
		return Err(WaylandCaptureError::ShortFrameBuffer {
			len: source.len(),
			width,
			height,
			stride,
		});
	}
	let mut rgba = vec![
		0;
		(width as usize)
			.saturating_mul(height as usize)
			.saturating_mul(4)
	];
	for y in 0..height as usize {
		let row = &source[y * stride..y * stride + row_bytes];
		for x in 0..width as usize {
			let input = &row[x * pixel_size..];
			let output = &mut rgba[(y * width as usize + x) * 4..];
			match format.format() {
				video::VideoFormat::RGB | video::VideoFormat::RGBA | video::VideoFormat::RGBx => {
					output[..4].copy_from_slice(&[
						input[0],
						input[1],
						input[2],
						if pixel_size == 4 && format.format() == video::VideoFormat::RGBA {
							input[3]
						} else {
							255
						},
					]);
				},
				_ => output[..4].copy_from_slice(&[
					input[2],
					input[1],
					input[0],
					if pixel_size == 4 && format.format() == video::VideoFormat::BGRA {
						input[3]
					} else {
						255
					},
				]),
			}
		}
	}
	RgbaImage::from_raw(width, height, rgba).ok_or(WaylandCaptureError::FrameConstruction)
}

fn grab_pipewire_frame(node: u32, fd: OwnedFd) -> Result<RgbaImage, WaylandCaptureError> {
	pw::init();
	let mainloop = MainLoopRc::new(None).map_err(WaylandCaptureError::MainLoop)?;
	let context = ContextRc::new(&mainloop, None).map_err(WaylandCaptureError::Context)?;
	let core = context
		.connect_fd_rc(fd, None)
		.map_err(WaylandCaptureError::Remote)?;
	let stream = StreamBox::new(&core, "omp-computer-capture", properties! {
		*pw::keys::MEDIA_TYPE => "Video",
		*pw::keys::MEDIA_CATEGORY => "Capture",
		*pw::keys::MEDIA_ROLE => "Screen",
	})
	.map_err(WaylandCaptureError::Stream)?;
	let result: Rc<RefCell<Option<Result<RgbaImage, WaylandCaptureError>>>> =
		Rc::new(RefCell::new(None));
	let callback_result = Rc::clone(&result);
	let callback_loop = mainloop.clone();
	let _listener = stream
		.add_local_listener_with_user_data(UserData { format: Default::default() })
		.param_changed(|_, user, id, param| {
			let Some(param) = param else {
				return;
			};
			if id == param::ParamType::Format.as_raw() {
				let _ = user.format.parse(param);
			}
		})
		.process(move |stream, user| {
			let Some(mut buffer) = stream.dequeue_buffer() else {
				return;
			};
			let Some(data) = buffer.datas_mut().first_mut() else {
				return;
			};
			*callback_result.borrow_mut() = Some(rgba_from_buffer(&user.format, data));
			callback_loop.quit();
		})
		.register()
		.map_err(WaylandCaptureError::Listener)?;
	let object = spa::pod::object!(
		spa::utils::SpaTypes::ObjectParamFormat,
		spa::param::pw::spa::param::ParamType::EnumFormat,
		spa::pod::property!(
			spa::param::format::FormatProperties::MediaType,
			Id,
			spa::param::format::MediaType::Video
		),
		spa::pod::property!(
			spa::param::format::FormatProperties::MediaSubtype,
			Id,
			spa::param::format::MediaSubtype::Raw
		),
		spa::pod::property!(
			spa::param::format::FormatProperties::pw::spa::param::video::VideoFormat,
			Choice,
			Enum,
			Id,
			spa::param::video::pw::spa::param::video::VideoFormat::BGRx,
			spa::param::video::pw::spa::param::video::VideoFormat::BGRx,
			spa::param::video::pw::spa::param::video::VideoFormat::BGRA,
			spa::param::video::pw::spa::param::video::VideoFormat::RGBx,
			spa::param::video::pw::spa::param::video::VideoFormat::RGBA,
			spa::param::video::pw::spa::param::video::VideoFormat::RGB,
			spa::param::video::pw::spa::param::video::VideoFormat::BGR
		),
		spa::pod::property!(
			spa::param::format::FormatProperties::VideoSize,
			Choice,
			Range,
			Rectangle,
			spa::utils::Rectangle { width: 1920, height: 1080 },
			spa::utils::Rectangle { width: 1, height: 1 },
			spa::utils::Rectangle { width: 16384, height: 16384 }
		)
	);
	let values = PodSerializer::serialize(io::Cursor::new(Vec::new()), &pod::Value::Object(object))
		.map_err(WaylandCaptureError::FormatSerialization)?
		.0
		.into_inner();
	let param = pod::Pod::from_bytes(&values).ok_or(WaylandCaptureError::InvalidFormatPod)?;
	stream
		.connect(
			utils::Direction::Input,
			Some(node),
			StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
			&mut [param],
		)
		.map_err(WaylandCaptureError::Connect)?;
	mainloop.run();
	result
		.borrow_mut()
		.take()
		.unwrap_or(Err(WaylandCaptureError::NoFrame))
}

pub(super) fn capture(runtime: &Runtime) -> CoreResult<RgbaImage> {
	let (node, fd) = runtime.block_on(open_screencast()).map_err(|err| {
		DesktopError::caused(ErrorCode::CaptureFailed, "wayland screencast unavailable", &err)
	})?;
	grab_pipewire_frame(node, fd).map_err(|err| {
		DesktopError::caused(ErrorCode::CaptureFailed, "wayland screencast failed", &err)
	})
}
