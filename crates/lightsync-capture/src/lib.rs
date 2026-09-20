#![forbid(unsafe_code)]

//! Asynchronous desktop capture through XDG ScreenCast/PipeWire, with a grim fallback.

use std::fmt;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result as AnyResult};
use ashpd::desktop::ResponseError;
use ashpd::desktop::{
    PersistMode,
    screencast::{CursorMode, Screencast, SourceType},
};
use async_trait::async_trait;
use pipewire as pw;
use pw::{properties::properties, spa};
use tokio::sync::{oneshot, watch};

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// Pixel layouts delivered by this crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    Bgrx,
    Bgra,
    Rgbx,
    Rgba,
    /// Packed RGB, used by grim's PPM output.
    Rgb,
}

impl PixelFormat {
    #[must_use]
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb => 3,
            Self::Bgrx | Self::Bgra | Self::Rgbx | Self::Rgba => 4,
        }
    }

    #[must_use]
    pub const fn rgb_offsets(self) -> (usize, usize, usize) {
        match self {
            Self::Bgrx | Self::Bgra => (2, 1, 0),
            Self::Rgbx | Self::Rgba | Self::Rgb => (0, 1, 2),
        }
    }
}

/// Format metadata associated with one frame. It may change between frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameFormat {
    pub width: u32,
    pub height: u32,
    pub stride: i32,
    pub pixel_format: PixelFormat,
}

/// A capture frame that owns its mapped shared-memory bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnedFrame {
    format: FrameFormat,
    data: Vec<u8>,
}

impl OwnedFrame {
    pub fn new(format: FrameFormat, data: Vec<u8>) -> Result<Self, CaptureError> {
        validate_frame(format, data.len())?;
        Ok(Self { format, data })
    }

    #[must_use]
    pub const fn format(&self) -> FrameFormat {
        self.format
    }

    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    #[must_use]
    pub fn view(&self) -> FrameView<'_> {
        FrameView {
            format: self.format,
            data: &self.data,
        }
    }
}

/// Borrowed frame data suitable for direct consumption by a sampling engine.
#[derive(Clone, Copy, Debug)]
pub struct FrameView<'a> {
    pub format: FrameFormat,
    pub data: &'a [u8],
}

impl FrameView<'_> {
    /// Returns RGB bytes in logical top-to-bottom coordinates.
    #[must_use]
    pub fn rgb_at(&self, x: u32, y: u32) -> Option<[u8; 3]> {
        if x >= self.format.width || y >= self.format.height {
            return None;
        }
        let stride = effective_stride(self.format).ok()?;
        let stored_y = if self.format.stride < 0 {
            usize::try_from(self.format.height)
                .ok()?
                .checked_sub(1 + usize::try_from(y).ok()?)?
        } else {
            usize::try_from(y).ok()?
        };
        let offset = stored_y.checked_mul(stride)?.checked_add(
            usize::try_from(x)
                .ok()?
                .checked_mul(self.format.pixel_format.bytes_per_pixel())?,
        )?;
        let (red, green, blue) = self.format.pixel_format.rgb_offsets();
        Some([
            *self.data.get(offset + red)?,
            *self.data.get(offset + green)?,
            *self.data.get(offset + blue)?,
        ])
    }
}

/// Observable state of a capture request/session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureState {
    Idle,
    ConsentRequired,
    RequestingConsent,
    Connecting,
    Streaming,
    Cancelled,
    Stopping,
    Stopped,
    Failed(String),
}

/// Errors with cancellation and non-interactive consent represented explicitly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureError {
    UserCancelled,
    ConsentRequired,
    Closed,
    InvalidFrame(String),
    Backend(String),
    ShutdownTimedOut,
}

impl fmt::Display for CaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UserCancelled => formatter.write_str("screen capture was cancelled by the user"),
            Self::ConsentRequired => formatter.write_str("screen capture requires user consent"),
            Self::Closed => formatter.write_str("screen capture stopped"),
            Self::InvalidFrame(message) | Self::Backend(message) => formatter.write_str(message),
            Self::ShutdownTimedOut => {
                formatter.write_str("screen capture did not stop within 3 seconds")
            }
        }
    }
}

impl std::error::Error for CaptureError {}

#[async_trait]
pub trait Capture: Send {
    async fn next_frame(&mut self) -> Result<Arc<OwnedFrame>, CaptureError>;
    fn state(&self) -> CaptureState;
    async fn shutdown(&mut self) -> Result<(), CaptureError>;
}

/// Portal capabilities that can be queried without opening a capture session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PortalCapabilities {
    pub monitor: bool,
    pub window: bool,
    pub virtual_source: bool,
    pub hidden_cursor: bool,
    pub embedded_cursor: bool,
    pub metadata_cursor: bool,
}

pub async fn portal_capabilities() -> Result<PortalCapabilities, CaptureError> {
    let proxy = Screencast::new().await.map_err(portal_backend_error)?;
    let sources = proxy
        .available_source_types()
        .await
        .map_err(portal_backend_error)?;
    let cursors = proxy
        .available_cursor_modes()
        .await
        .map_err(portal_backend_error)?;
    Ok(PortalCapabilities {
        monitor: sources.contains(SourceType::Monitor),
        window: sources.contains(SourceType::Window),
        virtual_source: sources.contains(SourceType::Virtual),
        hidden_cursor: cursors.contains(CursorMode::Hidden),
        embedded_cursor: cursors.contains(CursorMode::Embedded),
        metadata_cursor: cursors.contains(CursorMode::Metadata),
    })
}

/// Metadata the portal exposes for the selected monitor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectedSource {
    pub id: Option<String>,
    pub mapping_id: Option<String>,
    pub position: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
}

#[derive(Clone, Debug)]
pub struct PortalOptions {
    /// When false, a missing restore token returns `ConsentRequired` without opening a chooser.
    pub interactive: bool,
    /// Previously returned portal token. It is opaque and must not be parsed.
    pub restore_token: Option<String>,
    /// Optional private file used to load and atomically save the restore token.
    pub restore_token_path: Option<PathBuf>,
}

impl Default for PortalOptions {
    fn default() -> Self {
        Self {
            interactive: true,
            restore_token: None,
            restore_token_path: None,
        }
    }
}

/// Configuration for the default portal-first capture path.
#[derive(Clone, Debug)]
pub struct CaptureOptions {
    pub portal: PortalOptions,
    /// Use grim only when the portal/PipeWire backend is unavailable or fails.
    /// Cancellation and explicit consent requirements are never bypassed.
    pub grim_fallback: bool,
    pub grim_output: Option<String>,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            portal: PortalOptions::default(),
            grim_fallback: true,
            grim_output: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveBackend {
    PortalPipeWire,
    Grim,
}

/// The default portal/PipeWire session, optionally falling back to grim.
pub enum CaptureSession {
    Portal(PortalCapture),
    Grim(GrimCapture),
}

impl CaptureSession {
    pub async fn start(options: CaptureOptions) -> Result<Self, CaptureError> {
        match PortalCapture::start(options.portal).await {
            Ok(capture) => Ok(Self::Portal(capture)),
            Err(CaptureError::Backend(_)) if options.grim_fallback => {
                Ok(Self::Grim(GrimCapture::new(options.grim_output)))
            }
            Err(error) => Err(error),
        }
    }

    #[must_use]
    pub const fn backend(&self) -> ActiveBackend {
        match self {
            Self::Portal(_) => ActiveBackend::PortalPipeWire,
            Self::Grim(_) => ActiveBackend::Grim,
        }
    }
}

#[async_trait]
impl Capture for CaptureSession {
    async fn next_frame(&mut self) -> Result<Arc<OwnedFrame>, CaptureError> {
        match self {
            Self::Portal(capture) => capture.next_frame().await,
            Self::Grim(capture) => capture.next_frame().await,
        }
    }

    fn state(&self) -> CaptureState {
        match self {
            Self::Portal(capture) => capture.state(),
            Self::Grim(capture) => capture.state(),
        }
    }

    async fn shutdown(&mut self) -> Result<(), CaptureError> {
        match self {
            Self::Portal(capture) => capture.shutdown().await,
            Self::Grim(capture) => capture.shutdown().await,
        }
    }
}

#[derive(Clone, Debug)]
enum FrameEvent {
    Frame(Arc<OwnedFrame>),
    Error(CaptureError),
    Closed,
}

/// Low-latency capture of one portal-selected monitor.
pub struct PortalCapture {
    frames: watch::Receiver<Option<FrameEvent>>,
    states: watch::Receiver<CaptureState>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
    selected_source: SelectedSource,
    restore_token: Option<String>,
}

impl PortalCapture {
    pub async fn start(options: PortalOptions) -> Result<Self, CaptureError> {
        let (states_tx, states) = watch::channel(CaptureState::Idle);
        Self::start_with_state(options, states_tx, states).await
    }

    /// Starts capture while publishing states to a caller-owned observer.
    pub async fn start_observed(
        options: PortalOptions,
        states_tx: watch::Sender<CaptureState>,
    ) -> Result<Self, CaptureError> {
        let states = states_tx.subscribe();
        Self::start_with_state(options, states_tx, states).await
    }

    async fn start_with_state(
        mut options: PortalOptions,
        states_tx: watch::Sender<CaptureState>,
        states: watch::Receiver<CaptureState>,
    ) -> Result<Self, CaptureError> {
        if options.restore_token.is_none()
            && let Some(path) = options.restore_token_path.as_deref()
        {
            options.restore_token = load_restore_token(path).await?;
        }
        if !options.interactive && options.restore_token.is_none() {
            states_tx.send_replace(CaptureState::ConsentRequired);
            return Err(CaptureError::ConsentRequired);
        }

        let (frames_tx, frames) = watch::channel(None);
        let (ready_tx, ready_rx) = oneshot::channel();
        let (stop, stop_rx) = oneshot::channel();
        let task = tokio::spawn(run_portal(options, states_tx, frames_tx, ready_tx, stop_rx));
        let ready = match ready_rx.await {
            Ok(result) => result,
            Err(_) => Err(CaptureError::Backend(
                "portal capture task stopped during startup".to_owned(),
            )),
        };
        match ready {
            Ok((selected_source, restore_token)) => Ok(Self {
                frames,
                states,
                stop: Some(stop),
                task: Some(task),
                selected_source,
                restore_token,
            }),
            Err(error) => {
                let _ = task.await;
                Err(error)
            }
        }
    }

    #[must_use]
    pub fn selected_source(&self) -> &SelectedSource {
        &self.selected_source
    }

    #[must_use]
    pub fn restore_token(&self) -> Option<&str> {
        self.restore_token.as_deref()
    }
}

#[async_trait]
impl Capture for PortalCapture {
    async fn next_frame(&mut self) -> Result<Arc<OwnedFrame>, CaptureError> {
        receive_latest(&mut self.frames).await
    }

    fn state(&self) -> CaptureState {
        self.states.borrow().clone()
    }

    async fn shutdown(&mut self) -> Result<(), CaptureError> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let Some(mut task) = self.task.take() else {
            return Ok(());
        };
        match tokio::time::timeout(SHUTDOWN_TIMEOUT, &mut task).await {
            Ok(_) => Ok(()),
            Err(_) => {
                task.abort();
                Err(CaptureError::ShutdownTimedOut)
            }
        }
    }
}

async fn receive_latest(
    receiver: &mut watch::Receiver<Option<FrameEvent>>,
) -> Result<Arc<OwnedFrame>, CaptureError> {
    loop {
        receiver.changed().await.map_err(|_| CaptureError::Closed)?;
        let event = receiver.borrow_and_update().clone();
        match event {
            Some(FrameEvent::Frame(frame)) => return Ok(frame),
            Some(FrameEvent::Error(error)) => return Err(error),
            Some(FrameEvent::Closed) => return Err(CaptureError::Closed),
            None => {}
        }
    }
}

type PortalReady = Result<(SelectedSource, Option<String>), CaptureError>;

async fn run_portal(
    options: PortalOptions,
    states: watch::Sender<CaptureState>,
    frames: watch::Sender<Option<FrameEvent>>,
    ready: oneshot::Sender<PortalReady>,
    mut stop: oneshot::Receiver<()>,
) {
    let result = run_portal_inner(&options, &states, &frames, &mut stop, ready).await;
    if let Err(error) = result {
        states.send_replace(state_for_error(&error));
        frames.send_replace(Some(FrameEvent::Error(error)));
    }
}

async fn run_portal_inner(
    options: &PortalOptions,
    states: &watch::Sender<CaptureState>,
    frames: &watch::Sender<Option<FrameEvent>>,
    stop: &mut oneshot::Receiver<()>,
    ready: oneshot::Sender<PortalReady>,
) -> Result<(), CaptureError> {
    states.send_replace(CaptureState::RequestingConsent);
    let proxy = Screencast::new().await.map_err(portal_backend_error)?;
    let session = proxy.create_session().await.map_err(portal_backend_error)?;
    proxy
        .select_sources(
            &session,
            CursorMode::Hidden,
            SourceType::Monitor.into(),
            false,
            options.restore_token.as_deref(),
            PersistMode::ExplicitlyRevoked,
        )
        .await
        .map_err(portal_backend_error)?;

    let response = tokio::select! {
        _ = &mut *stop => {
            let _ = session.close().await;
            states.send_replace(CaptureState::Stopped);
            let _ = ready.send(Err(CaptureError::Closed));
            return Ok(());
        }
        result = async {
            let request = proxy.start(&session, None).await.map_err(portal_backend_error)?;
            request.response().map_err(portal_backend_error)
        } => result,
    };
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            let _ = ready.send(Err(error.clone()));
            let _ = session.close().await;
            return Err(error);
        }
    };
    let Some(stream) = response.streams().first() else {
        let error = CaptureError::Backend("the portal returned no monitor stream".to_owned());
        let _ = ready.send(Err(error.clone()));
        let _ = session.close().await;
        return Err(error);
    };
    let selected = SelectedSource {
        id: stream.id().map(ToOwned::to_owned),
        mapping_id: stream.mapping_id().map(ToOwned::to_owned),
        position: stream.position(),
        size: stream.size(),
    };
    let restore_token = response.restore_token().map(ToOwned::to_owned);
    if let (Some(path), Some(token)) = (options.restore_token_path.as_deref(), &restore_token)
        && let Err(error) = save_restore_token(path, token).await
    {
        tracing::warn!(%error, "could not persist portal restore token");
    }
    let remote = proxy
        .open_pipe_wire_remote(&session)
        .await
        .map_err(portal_backend_error)?;

    states.send_replace(CaptureState::Connecting);
    let (pw_stop, pw_stop_rx) = pw::channel::channel();
    let node_id = stream.pipe_wire_node_id();
    let thread_frames = frames.clone();
    let thread_states = states.clone();
    let (worker_done_tx, mut worker_done_rx) = oneshot::channel();
    let worker = thread::Builder::new()
        .name("lightsync-pipewire".to_owned())
        .spawn(move || {
            let result = run_pipewire(node_id, remote, thread_frames, thread_states, pw_stop_rx)
                .map_err(|error| format!("{error:#}"));
            let _ = worker_done_tx.send(result);
        })
        .map_err(|error| {
            CaptureError::Backend(format!("failed to start PipeWire thread: {error}"))
        })?;
    let _ = ready.send(Ok((selected, restore_token)));

    let stopped_by_request = tokio::select! {
        _ = &mut *stop => {
            states.send_replace(CaptureState::Stopping);
            let _ = pw_stop.send(());
            true
        }
        result = &mut worker_done_rx => {
            let result = result.map_err(|_| CaptureError::Backend(
                "PipeWire completion signal was lost".to_owned()
            ))?;
            if let Err(error) = result {
                let _ = session.close().await;
                return Err(CaptureError::Backend(format!("PipeWire capture failed: {error}")));
            }
            false
        }
    };
    let _ = session.close().await;
    let joined = tokio::task::spawn_blocking(move || worker.join())
        .await
        .map_err(|error| CaptureError::Backend(format!("PipeWire join failed: {error}")))?;
    match joined {
        Ok(()) if stopped_by_request => {
            states.send_replace(CaptureState::Stopped);
            frames.send_replace(Some(FrameEvent::Closed));
            Ok(())
        }
        Ok(()) => Err(CaptureError::Backend(
            "PipeWire capture worker stopped unexpectedly".to_owned(),
        )),
        Err(_) => Err(CaptureError::Backend(
            "PipeWire capture thread panicked".to_owned(),
        )),
    }
}

struct PipeWireData {
    format: spa::param::video::VideoInfoRaw,
    frames: watch::Sender<Option<FrameEvent>>,
    states: watch::Sender<CaptureState>,
}

fn run_pipewire(
    node_id: u32,
    remote: OwnedFd,
    frames: watch::Sender<Option<FrameEvent>>,
    states: watch::Sender<CaptureState>,
    stop: pw::channel::Receiver<()>,
) -> AnyResult<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let stop_loop = mainloop.clone();
    let _stop = stop.attach(mainloop.loop_(), move |()| stop_loop.quit());
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_fd_rc(remote, None)?;
    let stream = pw::stream::StreamRc::new(
        core,
        "lightsync-screen-capture",
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;
    let data = PipeWireData {
        format: Default::default(),
        frames: frames.clone(),
        states: states.clone(),
    };
    let state_loop = mainloop.clone();
    let param_loop = mainloop.clone();
    let process_loop = mainloop.clone();
    let _listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(move |_, data, _, new| match new {
            pw::stream::StreamState::Streaming => {
                data.states.send_replace(CaptureState::Streaming);
            }
            pw::stream::StreamState::Error(message) => {
                let error = CaptureError::Backend(format!("PipeWire stream failed: {message}"));
                data.states.send_replace(state_for_error(&error));
                data.frames.send_replace(Some(FrameEvent::Error(error)));
                state_loop.quit();
            }
            pw::stream::StreamState::Unconnected => {
                let error = CaptureError::Backend("PipeWire stream disconnected".to_owned());
                data.states.send_replace(state_for_error(&error));
                data.frames.send_replace(Some(FrameEvent::Error(error)));
                state_loop.quit();
            }
            _ => {}
        })
        .param_changed(move |_, data, id, param| {
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Some(param) = param else {
                publish_pipewire_error(
                    data,
                    &param_loop,
                    "PipeWire provided an empty video format",
                );
                return;
            };
            if let Err(error) = data.format.parse(param) {
                let error =
                    CaptureError::Backend(format!("unsupported PipeWire video format: {error:?}"));
                data.states.send_replace(state_for_error(&error));
                data.frames.send_replace(Some(FrameEvent::Error(error)));
                param_loop.quit();
            }
        })
        .process(move |stream, data| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let Some(plane) = buffer.datas_mut().first_mut() else {
                publish_pipewire_error(data, &process_loop, "PipeWire buffer has no data plane");
                return;
            };
            let chunk = plane.chunk();
            let offset = chunk.offset() as usize;
            let size = chunk.size() as usize;
            let stride = chunk.stride();
            let Some(mapped) = plane.data() else {
                publish_pipewire_error(data, &process_loop, "PipeWire buffer is not mapped");
                return;
            };
            let Some(pixel_format) = pixel_format(data.format.format()) else {
                publish_pipewire_error(
                    data,
                    &process_loop,
                    "PipeWire selected an unsupported pixel format",
                );
                return;
            };
            let format = FrameFormat {
                width: data.format.size().width,
                height: data.format.size().height,
                stride,
                pixel_format,
            };
            match copy_pipewire_frame(mapped, offset, size, format) {
                Ok(frame) => {
                    data.frames
                        .send_replace(Some(FrameEvent::Frame(Arc::new(frame))));
                }
                Err(error) => {
                    data.states.send_replace(state_for_error(&error));
                    data.frames.send_replace(Some(FrameEvent::Error(error)));
                    process_loop.quit();
                }
            }
        })
        .register()?;

    let obj = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
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
            spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            spa::param::video::VideoFormat::BGRx,
            spa::param::video::VideoFormat::BGRx,
            spa::param::video::VideoFormat::BGRA,
            spa::param::video::VideoFormat::RGBx,
            spa::param::video::VideoFormat::RGBA,
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            spa::utils::Rectangle {
                width: 1920,
                height: 1080
            },
            spa::utils::Rectangle {
                width: 1,
                height: 1
            },
            spa::utils::Rectangle {
                width: 16384,
                height: 16384
            }
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: 60, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction { num: 120, denom: 1 }
        ),
    );
    let values = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )?
    .0
    .into_inner();
    let parameter = spa::pod::Pod::from_bytes(&values).context("invalid PipeWire format pod")?;
    stream.connect(
        spa::utils::Direction::Input,
        Some(node_id),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut [parameter],
    )?;
    mainloop.run();
    Ok(())
}

fn publish_pipewire_error(
    data: &PipeWireData,
    mainloop: &pw::main_loop::MainLoopRc,
    message: &str,
) {
    let error = CaptureError::InvalidFrame(message.to_owned());
    data.states.send_replace(state_for_error(&error));
    data.frames.send_replace(Some(FrameEvent::Error(error)));
    mainloop.quit();
}

fn copy_pipewire_frame(
    mapped: &[u8],
    offset: usize,
    chunk_size: usize,
    format: FrameFormat,
) -> Result<OwnedFrame, CaptureError> {
    let row_bytes = usize::try_from(format.width)
        .ok()
        .and_then(|width| width.checked_mul(format.pixel_format.bytes_per_pixel()))
        .ok_or_else(|| invalid_frame("PipeWire row size overflows"))?;
    let height =
        usize::try_from(format.height).map_err(|_| invalid_frame("PipeWire frame is too tall"))?;
    let stride = if format.stride == 0 {
        isize::try_from(row_bytes).map_err(|_| invalid_frame("PipeWire stride is too large"))?
    } else {
        format.stride as isize
    };
    if stride.unsigned_abs() < row_bytes {
        return Err(invalid_frame("PipeWire stride is shorter than a pixel row"));
    }
    let required = height
        .saturating_sub(1)
        .checked_mul(stride.unsigned_abs())
        .and_then(|prefix| prefix.checked_add(row_bytes))
        .ok_or_else(|| invalid_frame("PipeWire frame size overflows"))?;
    if chunk_size < required {
        return Err(invalid_frame("PipeWire chunk is truncated"));
    }

    let mut output = Vec::with_capacity(
        row_bytes
            .checked_mul(height)
            .ok_or_else(|| invalid_frame("PipeWire frame size overflows"))?,
    );
    let offset =
        isize::try_from(offset).map_err(|_| invalid_frame("PipeWire offset is too large"))?;
    for row in 0..height {
        let row = isize::try_from(row).map_err(|_| invalid_frame("PipeWire row is too large"))?;
        let start = offset
            .checked_add(
                row.checked_mul(stride)
                    .ok_or_else(|| invalid_frame("PipeWire row offset overflows"))?,
            )
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| invalid_frame("PipeWire row starts outside the mapped buffer"))?;
        let end = start
            .checked_add(row_bytes)
            .ok_or_else(|| invalid_frame("PipeWire row offset overflows"))?;
        output.extend_from_slice(
            mapped
                .get(start..end)
                .ok_or_else(|| invalid_frame("PipeWire row is outside the mapped buffer"))?,
        );
    }
    OwnedFrame::new(
        FrameFormat {
            stride: i32::try_from(row_bytes)
                .map_err(|_| invalid_frame("PipeWire row is too wide"))?,
            ..format
        },
        output,
    )
}

fn pixel_format(format: spa::param::video::VideoFormat) -> Option<PixelFormat> {
    match format {
        spa::param::video::VideoFormat::BGRx => Some(PixelFormat::Bgrx),
        spa::param::video::VideoFormat::BGRA => Some(PixelFormat::Bgra),
        spa::param::video::VideoFormat::RGBx => Some(PixelFormat::Rgbx),
        spa::param::video::VideoFormat::RGBA => Some(PixelFormat::Rgba),
        _ => None,
    }
}

/// Portable compatibility backend using `grim -t ppm -`.
pub struct GrimCapture {
    output: Option<String>,
    minimum_interval: Duration,
    last_capture: Option<tokio::time::Instant>,
    state: CaptureState,
}

impl GrimCapture {
    #[must_use]
    pub fn new(output: Option<String>) -> Self {
        Self {
            output,
            minimum_interval: Duration::from_millis(16),
            last_capture: None,
            state: CaptureState::Idle,
        }
    }

    #[must_use]
    pub fn with_minimum_interval(mut self, interval: Duration) -> Self {
        self.minimum_interval = interval;
        self
    }
}

#[async_trait]
impl Capture for GrimCapture {
    async fn next_frame(&mut self) -> Result<Arc<OwnedFrame>, CaptureError> {
        if matches!(self.state, CaptureState::Stopped | CaptureState::Stopping) {
            return Err(CaptureError::Closed);
        }
        if let Some(last) = self.last_capture {
            tokio::time::sleep_until(last + self.minimum_interval).await;
        }
        let output_name = self.output.clone();
        let output = tokio::task::spawn_blocking(move || capture_grim(output_name.as_deref()))
            .await
            .map_err(|error| CaptureError::Backend(format!("grim task failed: {error}")))??;
        self.last_capture = Some(tokio::time::Instant::now());
        self.state = CaptureState::Streaming;
        Ok(Arc::new(parse_ppm(&output)?))
    }

    fn state(&self) -> CaptureState {
        self.state.clone()
    }

    async fn shutdown(&mut self) -> Result<(), CaptureError> {
        self.state = CaptureState::Stopped;
        Ok(())
    }
}

fn capture_grim(output_name: Option<&str>) -> Result<Vec<u8>, CaptureError> {
    let mut command = std::process::Command::new("grim");
    command.args(["-t", "ppm"]);
    if let Some(output) = output_name {
        command.args(["-o", output]);
    }
    let output = command
        .arg("-")
        .output()
        .map_err(|error| CaptureError::Backend(format!("failed to start grim: {error}")))?;
    if !output.status.success() {
        return Err(CaptureError::Backend(format!(
            "grim failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

pub fn parse_ppm(data: &[u8]) -> Result<OwnedFrame, CaptureError> {
    let mut parser = PpmParser { data, cursor: 0 };
    if parser.token()? != b"P6" {
        return Err(invalid_frame("PPM magic must be P6"));
    }
    let width = parser.number("width")?;
    let height = parser.number("height")?;
    let maximum = parser.number("maximum color value")?;
    if width == 0 || height == 0 || maximum != 255 {
        return Err(invalid_frame(
            "PPM dimensions or maximum color value are invalid",
        ));
    }
    parser.consume_raster_separator()?;
    let stride = width
        .checked_mul(3)
        .ok_or_else(|| invalid_frame("PPM row size overflows"))?;
    let size = stride
        .checked_mul(height)
        .ok_or_else(|| invalid_frame("PPM image size overflows"))?;
    let end = parser
        .cursor
        .checked_add(size)
        .ok_or_else(|| invalid_frame("PPM image size overflows"))?;
    let pixels = data
        .get(parser.cursor..end)
        .ok_or_else(|| invalid_frame("PPM image is truncated"))?;
    let format = FrameFormat {
        width: u32::try_from(width).map_err(|_| invalid_frame("PPM width is too large"))?,
        height: u32::try_from(height).map_err(|_| invalid_frame("PPM height is too large"))?,
        stride: i32::try_from(stride).map_err(|_| invalid_frame("PPM stride is too large"))?,
        pixel_format: PixelFormat::Rgb,
    };
    OwnedFrame::new(format, pixels.to_vec())
}

struct PpmParser<'a> {
    data: &'a [u8],
    cursor: usize,
}

impl<'a> PpmParser<'a> {
    fn skip_layout(&mut self) {
        loop {
            while self
                .data
                .get(self.cursor)
                .is_some_and(u8::is_ascii_whitespace)
            {
                self.cursor += 1;
            }
            if self.data.get(self.cursor) != Some(&b'#') {
                break;
            }
            while self
                .data
                .get(self.cursor)
                .is_some_and(|byte| *byte != b'\n')
            {
                self.cursor += 1;
            }
        }
    }

    fn token(&mut self) -> Result<&'a [u8], CaptureError> {
        self.skip_layout();
        let start = self.cursor;
        while self
            .data
            .get(self.cursor)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'#')
        {
            self.cursor += 1;
        }
        if start == self.cursor {
            return Err(invalid_frame("PPM header is incomplete"));
        }
        Ok(&self.data[start..self.cursor])
    }

    fn number(&mut self, name: &str) -> Result<usize, CaptureError> {
        let token = self.token()?;
        let text = std::str::from_utf8(token)
            .map_err(|_| invalid_frame(format!("PPM {name} is not text")))?;
        text.parse()
            .map_err(|_| invalid_frame(format!("PPM {name} is invalid")))
    }

    fn consume_raster_separator(&mut self) -> Result<(), CaptureError> {
        match self.data.get(self.cursor) {
            Some(b'\r') if self.data.get(self.cursor + 1) == Some(&b'\n') => self.cursor += 2,
            Some(byte) if byte.is_ascii_whitespace() => self.cursor += 1,
            _ => return Err(invalid_frame("PPM raster separator is missing")),
        }
        Ok(())
    }
}

fn validate_frame(format: FrameFormat, data_len: usize) -> Result<(), CaptureError> {
    if format.width == 0 || format.height == 0 {
        return Err(invalid_frame("frame dimensions must be non-zero"));
    }
    let stride = effective_stride(format)?;
    let row_bytes = usize::try_from(format.width)
        .ok()
        .and_then(|width| width.checked_mul(format.pixel_format.bytes_per_pixel()))
        .ok_or_else(|| invalid_frame("frame row size overflows"))?;
    if stride < row_bytes {
        return Err(invalid_frame("frame stride is shorter than a pixel row"));
    }
    let height = usize::try_from(format.height).map_err(|_| invalid_frame("frame is too tall"))?;
    let minimum = (height - 1)
        .checked_mul(stride)
        .and_then(|prefix| prefix.checked_add(row_bytes))
        .ok_or_else(|| invalid_frame("frame size overflows"))?;
    if data_len < minimum {
        return Err(invalid_frame("frame data is truncated"));
    }
    Ok(())
}

fn effective_stride(format: FrameFormat) -> Result<usize, CaptureError> {
    if format.stride == 0 {
        usize::try_from(format.width)
            .ok()
            .and_then(|width| width.checked_mul(format.pixel_format.bytes_per_pixel()))
            .ok_or_else(|| invalid_frame("frame row size overflows"))
    } else {
        usize::try_from(format.stride.unsigned_abs())
            .map_err(|_| invalid_frame("frame stride is too large"))
    }
}

fn invalid_frame(message: impl Into<String>) -> CaptureError {
    CaptureError::InvalidFrame(message.into())
}

fn portal_backend_error(error: ashpd::Error) -> CaptureError {
    match error {
        ashpd::Error::Response(ResponseError::Cancelled)
        | ashpd::Error::Portal(ashpd::PortalError::Cancelled(_)) => CaptureError::UserCancelled,
        ashpd::Error::Portal(ashpd::PortalError::NotAllowed(_)) => CaptureError::ConsentRequired,
        other => CaptureError::Backend(format!("screen capture portal failed: {other}")),
    }
}

fn state_for_error(error: &CaptureError) -> CaptureState {
    match error {
        CaptureError::UserCancelled => CaptureState::Cancelled,
        CaptureError::ConsentRequired => CaptureState::ConsentRequired,
        other => CaptureState::Failed(other.to_string()),
    }
}

async fn load_restore_token(path: &Path) -> Result<Option<String>, CaptureError> {
    match tokio::fs::read_to_string(path).await {
        Ok(token) => {
            let token = token.trim();
            Ok((!token.is_empty()).then(|| token.to_owned()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(CaptureError::Backend(format!(
            "failed to read portal restore token: {error}"
        ))),
    }
}

async fn save_restore_token(path: &Path, token: &str) -> AnyResult<()> {
    use std::os::unix::fs::PermissionsExt;

    let parent = path.parent().context("restore token path has no parent")?;
    tokio::fs::create_dir_all(parent).await?;
    tokio::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).await?;
    let temporary = parent.join(format!(".restore-token-{}.tmp", std::process::id()));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options.open(&temporary).await?;
    use tokio::io::AsyncWriteExt;
    if let Err(error) = file.write_all(token.as_bytes()).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error.into());
    }
    file.sync_all().await?;
    drop(file);
    if let Err(error) = tokio::fs::rename(&temporary, path).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ppm_comments_crlf_and_leading_pixel_whitespace() {
        let mut ppm = b"P6\r\n# grim output\r\n2 1\r\n255\r\n".to_vec();
        ppm.extend_from_slice(&[b' ', 2, 3, 4, 5, 6]);
        let frame = parse_ppm(&ppm).expect("PPM parses");

        assert_eq!(frame.format().width, 2);
        assert_eq!(frame.view().rgb_at(0, 0), Some([b' ', 2, 3]));
        assert_eq!(frame.view().rgb_at(1, 0), Some([4, 5, 6]));
    }

    #[test]
    fn rejects_truncated_ppm() {
        let error = parse_ppm(b"P6\n2 2\n255\n123").expect_err("truncated image rejected");
        assert!(matches!(error, CaptureError::InvalidFrame(_)));
    }

    #[test]
    fn all_pipewire_formats_map_to_rgb() {
        let cases = [
            (PixelFormat::Bgrx, [3, 2, 1, 0]),
            (PixelFormat::Bgra, [3, 2, 1, 255]),
            (PixelFormat::Rgbx, [1, 2, 3, 0]),
            (PixelFormat::Rgba, [1, 2, 3, 255]),
        ];
        for (pixel_format, pixel) in cases {
            let frame = OwnedFrame::new(
                FrameFormat {
                    width: 1,
                    height: 1,
                    stride: 4,
                    pixel_format,
                },
                pixel.to_vec(),
            )
            .expect("valid frame");
            assert_eq!(frame.view().rgb_at(0, 0), Some([1, 2, 3]));
        }
    }

    #[test]
    fn handles_positive_negative_and_implicit_stride() {
        let top = [255, 0, 0, 0, 0, 0, 0, 0];
        let bottom = [0, 0, 255, 0, 0, 0, 0, 0];
        let data = [top, bottom].concat();
        let positive = OwnedFrame::new(
            FrameFormat {
                width: 1,
                height: 2,
                stride: 8,
                pixel_format: PixelFormat::Rgbx,
            },
            data.clone(),
        )
        .expect("positive stride");
        assert_eq!(positive.view().rgb_at(0, 0), Some([255, 0, 0]));
        assert_eq!(positive.view().rgb_at(0, 1), Some([0, 0, 255]));

        let negative = OwnedFrame::new(
            FrameFormat {
                width: 1,
                height: 2,
                stride: -8,
                pixel_format: PixelFormat::Rgbx,
            },
            data,
        )
        .expect("negative stride");
        assert_eq!(negative.view().rgb_at(0, 0), Some([0, 0, 255]));
        assert_eq!(negative.view().rgb_at(0, 1), Some([255, 0, 0]));

        let implicit = OwnedFrame::new(
            FrameFormat {
                width: 1,
                height: 1,
                stride: 0,
                pixel_format: PixelFormat::Rgba,
            },
            vec![7, 8, 9, 255],
        )
        .expect("implicit stride");
        assert_eq!(implicit.view().rgb_at(0, 0), Some([7, 8, 9]));
    }

    #[test]
    fn pipewire_negative_stride_is_copied_from_the_chunk_origin_top_down() {
        let bottom = [0, 0, 255, 0, 0, 0, 0, 0];
        let top = [255, 0, 0, 0, 0, 0, 0, 0];
        let mapped = [bottom, top].concat();
        let frame = copy_pipewire_frame(
            &mapped,
            8,
            12,
            FrameFormat {
                width: 1,
                height: 2,
                stride: -8,
                pixel_format: PixelFormat::Rgbx,
            },
        )
        .expect("negative-stride chunk");
        assert_eq!(frame.format().stride, 4);
        assert_eq!(frame.view().rgb_at(0, 0), Some([255, 0, 0]));
        assert_eq!(frame.view().rgb_at(0, 1), Some([0, 0, 255]));
    }

    #[test]
    fn pipewire_copy_rejects_truncated_chunks() {
        let error = copy_pipewire_frame(
            &[0; 8],
            0,
            8,
            FrameFormat {
                width: 1,
                height: 2,
                stride: 8,
                pixel_format: PixelFormat::Rgbx,
            },
        )
        .expect_err("truncated chunk");
        assert!(matches!(error, CaptureError::InvalidFrame(_)));
    }

    #[tokio::test]
    async fn latest_frame_replaces_unread_frames() {
        let (sender, mut receiver) = watch::channel(None);
        for red in [1, 2, 3] {
            let frame = OwnedFrame::new(
                FrameFormat {
                    width: 1,
                    height: 1,
                    stride: 3,
                    pixel_format: PixelFormat::Rgb,
                },
                vec![red, 0, 0],
            )
            .expect("valid frame");
            sender.send_replace(Some(FrameEvent::Frame(Arc::new(frame))));
        }

        let frame = receive_latest(&mut receiver).await.expect("latest frame");
        assert_eq!(frame.view().rgb_at(0, 0), Some([3, 0, 0]));
    }

    #[test]
    fn rejects_short_stride_and_accepts_last_row_without_trailing_padding() {
        let short = OwnedFrame::new(
            FrameFormat {
                width: 2,
                height: 1,
                stride: 4,
                pixel_format: PixelFormat::Rgb,
            },
            vec![0; 6],
        );
        assert!(short.is_err());

        let compact_last_row = OwnedFrame::new(
            FrameFormat {
                width: 1,
                height: 2,
                stride: 8,
                pixel_format: PixelFormat::Rgba,
            },
            vec![0; 12],
        );
        assert!(compact_last_row.is_ok());
    }
}
