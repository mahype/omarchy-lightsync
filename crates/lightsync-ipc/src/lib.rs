//! Async Unix-socket transport for the LightSync typed protocol.

use std::env;
use std::ffi::OsStr;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lightsync_domain::{
    ActionableError, Event, EventEnvelope, ProtocolError, Request, RequestEnvelope, Response,
    ResponseEnvelope, ResponsePayload, StatusSnapshot, from_ndjson, to_ndjson,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

pub const SOCKET_DIRECTORY: &str = "omarchy-lightsync";
pub const SOCKET_NAME: &str = "control.sock";
pub const DEFAULT_MAX_RECORD_SIZE: usize = 1024 * 1024;
pub const DEFAULT_IO_TIMEOUT: Duration = Duration::from_secs(10);

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("XDG_RUNTIME_DIR is not set")]
    RuntimeDirMissing,
    #[error("runtime or socket path must be absolute: {0}")]
    RelativePath(PathBuf),
    #[error("refusing symlink path: {0}")]
    Symlink(PathBuf),
    #[error("path is not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("path is not a Unix socket: {0}")]
    NotSocket(PathBuf),
    #[error("I/O timed out after {0:?}")]
    Timeout(Duration),
    #[error("peer closed the connection before completing a record")]
    UnexpectedEof,
    #[error("record exceeds the {limit}-byte limit")]
    RecordTooLarge { limit: usize },
    #[error("response ID mismatch: expected {expected}, received {received}")]
    IdMismatch { expected: String, received: String },
    #[error("unexpected watch response payload")]
    UnexpectedWatchResponse,
    #[error("daemon error: {}", .0.message)]
    Remote(ActionableError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Resolves `$XDG_RUNTIME_DIR/omarchy-lightsync/control.sock`.
pub fn runtime_socket_path() -> Result<PathBuf> {
    socket_path_for_runtime_dir(env::var_os("XDG_RUNTIME_DIR").as_deref())
}

fn socket_path_for_runtime_dir(runtime_dir: Option<&OsStr>) -> Result<PathBuf> {
    let runtime_dir = runtime_dir.ok_or(Error::RuntimeDirMissing)?;
    let runtime_dir = Path::new(runtime_dir);
    if runtime_dir.as_os_str().is_empty() {
        return Err(Error::RuntimeDirMissing);
    }
    require_absolute(runtime_dir)?;
    Ok(runtime_dir.join(SOCKET_DIRECTORY).join(SOCKET_NAME))
}

#[derive(Debug, Clone)]
pub struct Client {
    socket_path: PathBuf,
    max_record_size: usize,
    io_timeout: Duration,
    initial_stream: Arc<Mutex<Option<UnixStream>>>,
}

impl Client {
    /// Connects to the socket resolved from `XDG_RUNTIME_DIR`.
    pub async fn connect() -> Result<Self> {
        Self::connect_path(runtime_socket_path()?).await
    }

    /// Connects to an explicit absolute socket path.
    pub async fn connect_path(path: impl Into<PathBuf>) -> Result<Self> {
        let client = Self::with_socket_path(path)?;
        let stream = client.open_stream().await?;
        *client.initial_stream.lock().await = Some(stream);
        Ok(client)
    }

    /// Constructs a client without connecting, which is useful for reconnecting watchers.
    pub fn from_runtime() -> Result<Self> {
        Self::with_socket_path(runtime_socket_path()?)
    }

    /// Constructs a client for an explicit path without touching global environment state.
    pub fn with_socket_path(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        require_absolute(&path)?;
        Ok(Self {
            socket_path: path,
            max_record_size: DEFAULT_MAX_RECORD_SIZE,
            io_timeout: DEFAULT_IO_TIMEOUT,
            initial_stream: Arc::new(Mutex::new(None)),
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn set_max_record_size(&mut self, max_record_size: usize) {
        self.max_record_size = max_record_size;
    }

    pub fn set_io_timeout(&mut self, io_timeout: Duration) {
        self.io_timeout = io_timeout;
    }

    /// Performs one request on a dedicated connection and returns the typed payload.
    pub async fn request(&self, request: Request) -> Result<ResponsePayload> {
        let response = self.request_envelope(request).await?;
        match response.response {
            Response::Success { payload } => Ok(payload),
            Response::Error { error } => Err(Error::Remote(error)),
        }
    }

    /// Performs one request and preserves the response envelope.
    pub async fn request_envelope(&self, request: Request) -> Result<ResponseEnvelope> {
        let request = RequestEnvelope::new(request);
        let mut stream = self.acquire_stream().await?;
        write_record(&mut stream, &request, self.max_record_size, self.io_timeout).await?;
        let mut reader = BufReader::new(stream);
        let response: ResponseEnvelope =
            read_record(&mut reader, self.max_record_size, self.io_timeout).await?;
        response.validate_version()?;
        if response.id != request.id {
            return Err(Error::IdMismatch {
                expected: request.id.to_string(),
                received: response.id.to_string(),
            });
        }
        Ok(response)
    }

    /// Starts a reconnecting status subscription.
    pub fn watch_status(&self, options: WatchOptions) -> StatusSubscription {
        let (sender, receiver) = watch::channel(None);
        let (error_sender, errors) = mpsc::channel(options.channel_capacity.max(1));
        let (cancel, cancelled) = watch::channel(false);
        let client = self.clone();
        let task = tokio::spawn(async move {
            run_status_watch(client, options, sender, error_sender, cancelled).await;
        });
        StatusSubscription {
            receiver,
            errors,
            cancel,
            task,
        }
    }

    async fn open_stream(&self) -> Result<UnixStream> {
        validate_existing_socket(&self.socket_path)?;
        match timeout(self.io_timeout, UnixStream::connect(&self.socket_path)).await {
            Ok(result) => Ok(result?),
            Err(_) => Err(Error::Timeout(self.io_timeout)),
        }
    }

    async fn acquire_stream(&self) -> Result<UnixStream> {
        match self.initial_stream.lock().await.take() {
            Some(stream) => Ok(stream),
            None => self.open_stream().await,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WatchOptions {
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub channel_capacity: usize,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(5),
            channel_capacity: 16,
        }
    }
}

pub struct StatusSubscription {
    receiver: watch::Receiver<Option<StatusSnapshot>>,
    errors: mpsc::Receiver<Error>,
    cancel: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl StatusSubscription {
    pub async fn recv(&mut self) -> Option<Result<StatusSnapshot>> {
        tokio::select! {
            biased;
            changed = self.receiver.changed() => {
                changed.ok()?;
                self.receiver.borrow_and_update().clone().map(Ok)
            }
            error = self.errors.recv() => error.map(Err),
        }
    }

    pub fn cancel(&self) {
        let _ = self.cancel.send(true);
    }
}

impl Drop for StatusSubscription {
    fn drop(&mut self) {
        let _ = self.cancel.send(true);
        self.task.abort();
    }
}

async fn run_status_watch(
    client: Client,
    options: WatchOptions,
    sender: watch::Sender<Option<StatusSnapshot>>,
    error_sender: mpsc::Sender<Error>,
    mut cancelled: watch::Receiver<bool>,
) {
    let initial = options.initial_backoff.max(Duration::from_millis(1));
    let maximum = options.max_backoff.max(initial);
    let mut backoff = initial;

    loop {
        if *cancelled.borrow() {
            return;
        }
        let result = watch_session(&client, &sender, &mut cancelled).await;
        if *cancelled.borrow() || sender.is_closed() {
            return;
        }
        if let Err(error) = result
            && send_error_or_cancel(&error_sender, error, &mut cancelled)
                .await
                .is_err()
        {
            return;
        }
        tokio::select! {
            _ = sleep(backoff) => {}
            changed = cancelled.changed() => {
                if changed.is_err() || *cancelled.borrow() { return; }
            }
        }
        backoff = backoff.saturating_mul(2).min(maximum);
    }
}

async fn watch_session(
    client: &Client,
    sender: &watch::Sender<Option<StatusSnapshot>>,
    cancelled: &mut watch::Receiver<bool>,
) -> Result<()> {
    let mut stream = tokio::select! {
        result = client.acquire_stream() => result?,
        changed = cancelled.changed() => {
            let _ = changed;
            return Ok(());
        }
    };
    let request = RequestEnvelope::new(Request::WatchStatus { enabled: true });
    tokio::select! {
        result = write_record(&mut stream, &request, client.max_record_size, client.io_timeout) => result?,
        changed = cancelled.changed() => {
            let _ = changed;
            return Ok(());
        }
    }
    let mut reader = BufReader::new(stream);
    let response: ResponseEnvelope = tokio::select! {
        result = read_record(&mut reader, client.max_record_size, client.io_timeout) => result?,
        changed = cancelled.changed() => {
            let _ = changed;
            return Ok(());
        }
    };
    response.validate_version()?;
    if response.id != request.id {
        return Err(Error::IdMismatch {
            expected: request.id.to_string(),
            received: response.id.to_string(),
        });
    }
    match response.response {
        Response::Success {
            payload: ResponsePayload::Acknowledged,
        } => {}
        Response::Success {
            payload: ResponsePayload::Status(status),
        } => {
            if send_or_cancel(sender, status, cancelled).await.is_err() {
                return Ok(());
            }
        }
        Response::Success { .. } => return Err(Error::UnexpectedWatchResponse),
        Response::Error { error } => return Err(Error::Remote(error)),
    }

    loop {
        let event: EventEnvelope = tokio::select! {
            result = read_record_inner(&mut reader, client.max_record_size) => result?,
            changed = cancelled.changed() => {
                let _ = changed;
                return Ok(());
            }
        };
        event.validate_version()?;
        if let Event::StatusChanged(status) = event.event
            && send_or_cancel(sender, status, cancelled).await.is_err()
        {
            return Ok(());
        }
    }
}

async fn send_or_cancel(
    sender: &watch::Sender<Option<StatusSnapshot>>,
    value: StatusSnapshot,
    cancelled: &mut watch::Receiver<bool>,
) -> std::result::Result<(), ()> {
    if *cancelled.borrow() || sender.is_closed() {
        Err(())
    } else {
        sender.send_replace(Some(value));
        Ok(())
    }
}

async fn send_error_or_cancel(
    sender: &mpsc::Sender<Error>,
    error: Error,
    cancelled: &mut watch::Receiver<bool>,
) -> std::result::Result<(), ()> {
    tokio::select! {
        result = sender.send(error) => result.map_err(|_| ()),
        changed = cancelled.changed() => {
            let _ = changed;
            Err(())
        }
    }
}

/// Listener guard that removes only the socket inode it created.
pub struct Server {
    listener: UnixListener,
    socket_path: PathBuf,
    device: u64,
    inode: u64,
}

impl Server {
    pub fn bind() -> Result<Self> {
        Self::bind_path(runtime_socket_path()?)
    }

    pub fn bind_path(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        require_absolute(&path)?;
        let parent = path
            .parent()
            .ok_or_else(|| Error::RelativePath(path.clone()))?;
        secure_directory(parent)?;
        remove_stale_socket(&path)?;

        let listener = UnixListener::bind(&path)?;
        if let Err(error) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        {
            let _ = std::fs::remove_file(&path);
            return Err(error.into());
        }
        let metadata = std::fs::symlink_metadata(&path)?;
        Ok(Self {
            listener,
            socket_path: path,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub async fn accept(&self) -> Result<UnixStream> {
        let (stream, _) = self.listener.accept().await?;
        Ok(stream)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Ok(metadata) = std::fs::symlink_metadata(&self.socket_path)
            && metadata.file_type().is_socket()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }
}

/// Reads and validates one typed request using the standard limit and timeout.
pub async fn read_request<R>(reader: &mut R) -> Result<RequestEnvelope>
where
    R: AsyncBufRead + Unpin,
{
    read_request_with_limits(reader, DEFAULT_MAX_RECORD_SIZE, DEFAULT_IO_TIMEOUT).await
}

/// Reads and validates one typed request with explicit transport limits.
pub async fn read_request_with_limits<R>(
    reader: &mut R,
    max_record_size: usize,
    io_timeout: Duration,
) -> Result<RequestEnvelope>
where
    R: AsyncBufRead + Unpin,
{
    let request: RequestEnvelope = read_record(reader, max_record_size, io_timeout).await?;
    request.validate_version()?;
    Ok(request)
}

pub async fn write_response<W>(writer: &mut W, response: &ResponseEnvelope) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    write_record(
        writer,
        response,
        DEFAULT_MAX_RECORD_SIZE,
        DEFAULT_IO_TIMEOUT,
    )
    .await
}

pub async fn write_event<W>(writer: &mut W, event: &EventEnvelope) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    write_record(writer, event, DEFAULT_MAX_RECORD_SIZE, DEFAULT_IO_TIMEOUT).await
}

pub async fn read_record<R, T>(
    reader: &mut R,
    max_record_size: usize,
    io_timeout: Duration,
) -> Result<T>
where
    R: AsyncBufRead + Unpin,
    T: DeserializeOwned,
{
    match timeout(io_timeout, read_record_inner(reader, max_record_size)).await {
        Ok(result) => result,
        Err(_) => Err(Error::Timeout(io_timeout)),
    }
}

async fn read_record_inner<R, T>(reader: &mut R, max_record_size: usize) -> Result<T>
where
    R: AsyncBufRead + Unpin,
    T: DeserializeOwned,
{
    let mut record = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Err(Error::UnexpectedEof);
        }
        if let Some(position) = available.iter().position(|byte| *byte == b'\n') {
            if record.len().saturating_add(position) > max_record_size {
                return Err(Error::RecordTooLarge {
                    limit: max_record_size,
                });
            }
            record.extend_from_slice(&available[..position]);
            reader.consume(position + 1);
            break;
        }
        if record.len().saturating_add(available.len()) > max_record_size {
            return Err(Error::RecordTooLarge {
                limit: max_record_size,
            });
        }
        let length = available.len();
        record.extend_from_slice(available);
        reader.consume(length);
    }
    let record = std::str::from_utf8(&record).map_err(|error| {
        Error::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            error.to_string(),
        ))
    })?;
    Ok(from_ndjson(record)?)
}

pub async fn write_record<W, T>(
    writer: &mut W,
    value: &T,
    max_record_size: usize,
    io_timeout: Duration,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let encoded = to_ndjson(value)?;
    if encoded.len().saturating_sub(1) > max_record_size {
        return Err(Error::RecordTooLarge {
            limit: max_record_size,
        });
    }
    match timeout(io_timeout, async {
        writer.write_all(encoded.as_bytes()).await?;
        writer.flush().await
    })
    .await
    {
        Ok(result) => Ok(result?),
        Err(_) => Err(Error::Timeout(io_timeout)),
    }
}

fn require_absolute(path: &Path) -> Result<()> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(Error::RelativePath(path.to_path_buf()))
    }
}

fn reject_symlink(path: &Path) -> Result<Option<std::fs::Metadata>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(Error::Symlink(path.to_path_buf()))
        }
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn validate_existing_socket(path: &Path) -> Result<()> {
    require_absolute(path)?;
    match reject_symlink(path)? {
        Some(metadata) if metadata.file_type().is_socket() => Ok(()),
        Some(_) => Err(Error::NotSocket(path.to_path_buf())),
        None => Err(io::Error::new(io::ErrorKind::NotFound, "socket does not exist").into()),
    }
}

fn secure_directory(path: &Path) -> Result<()> {
    require_absolute(path)?;
    match reject_symlink(path)? {
        Some(metadata) if !metadata.is_dir() => Err(Error::NotDirectory(path.to_path_buf())),
        Some(_) => {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
            Ok(())
        }
        None => {
            std::fs::create_dir_all(path)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
            Ok(())
        }
    }
}

fn remove_stale_socket(path: &Path) -> Result<()> {
    match reject_symlink(path)? {
        Some(metadata) if metadata.file_type().is_socket() => {
            match std::os::unix::net::UnixStream::connect(path) {
                Ok(_) => Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "a daemon is already listening on the socket",
                )
                .into()),
                Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                    let current = std::fs::symlink_metadata(path)?;
                    if !current.file_type().is_socket()
                        || current.dev() != metadata.dev()
                        || current.ino() != metadata.ino()
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::AddrInUse,
                            "socket path changed while checking whether it was stale",
                        )
                        .into());
                    }
                    std::fs::remove_file(path)?;
                    Ok(())
                }
                Err(error) => Err(error.into()),
            }
        }
        Some(_) => Err(Error::NotSocket(path.to_path_buf())),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use lightsync_domain::{PROTOCOL_VERSION, ServiceState};
    use tokio::io::AsyncWriteExt;

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!(
                "lightsync-ipc-test-{}-{sequence}",
                std::process::id()
            ));
            std::fs::create_dir(&path).expect("create temporary directory");
            Self(path)
        }

        fn socket(&self) -> PathBuf {
            self.0.join("runtime").join(SOCKET_NAME)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn runtime_paths_require_a_present_absolute_directory() {
        assert!(matches!(
            socket_path_for_runtime_dir(None),
            Err(Error::RuntimeDirMissing)
        ));
        assert!(matches!(
            socket_path_for_runtime_dir(Some(OsStr::new("relative"))),
            Err(Error::RelativePath(_))
        ));
    }

    #[tokio::test]
    async fn repeated_one_shot_calls_use_typed_envelopes() {
        let temp = TempDir::new();
        let server = Server::bind_path(temp.socket()).expect("bind server");
        let path = server.socket_path().to_path_buf();
        let task = tokio::spawn(async move {
            for _ in 0..2 {
                let stream = server.accept().await.expect("accept");
                let mut reader = BufReader::new(stream);
                let request = read_request_with_limits(
                    &mut reader,
                    DEFAULT_MAX_RECORD_SIZE,
                    DEFAULT_IO_TIMEOUT,
                )
                .await
                .expect("request");
                assert_eq!(request.request, Request::GetStatus);
                let mut stream = reader.into_inner();
                write_response(
                    &mut stream,
                    &ResponseEnvelope::success(
                        request.id,
                        ResponsePayload::Status(StatusSnapshot::default()),
                    ),
                )
                .await
                .expect("response");
            }
        });

        let client = Client::connect_path(path).await.expect("connect client");
        for _ in 0..2 {
            assert!(matches!(
                client.request(Request::GetStatus).await.expect("status"),
                ResponsePayload::Status(_)
            ));
        }
        task.await.expect("server task");
    }

    #[tokio::test]
    async fn watch_accepts_response_then_status_events() {
        let temp = TempDir::new();
        let server = Server::bind_path(temp.socket()).expect("bind server");
        let path = server.socket_path().to_path_buf();
        let task = tokio::spawn(async move {
            let stream = server.accept().await.expect("accept");
            let mut reader = BufReader::new(stream);
            let request =
                read_request_with_limits(&mut reader, DEFAULT_MAX_RECORD_SIZE, DEFAULT_IO_TIMEOUT)
                    .await
                    .expect("watch request");
            assert_eq!(request.request, Request::WatchStatus { enabled: true });
            let mut stream = reader.into_inner();
            write_response(
                &mut stream,
                &ResponseEnvelope::success(request.id, ResponsePayload::Acknowledged),
            )
            .await
            .expect("watch response");
            let status = StatusSnapshot {
                service: ServiceState::Starting,
                ..StatusSnapshot::default()
            };
            write_event(
                &mut stream,
                &EventEnvelope::new(Event::StatusChanged(status)),
            )
            .await
            .expect("status event");
        });

        let client = Client::connect_path(path).await.expect("connect client");
        let mut subscription = client.watch_status(WatchOptions::default());
        let status = subscription
            .recv()
            .await
            .expect("subscription item")
            .expect("status");
        assert_eq!(status.service, ServiceState::Starting);
        subscription.cancel();
        task.await.expect("server task");
    }

    #[tokio::test]
    async fn watch_reconnects_after_connection_loss() {
        let temp = TempDir::new();
        let server = Server::bind_path(temp.socket()).expect("bind server");
        let path = server.socket_path().to_path_buf();
        let task = tokio::spawn(async move {
            for service in [ServiceState::Starting, ServiceState::Ready] {
                let stream = server.accept().await.expect("accept");
                let mut reader = BufReader::new(stream);
                let request = read_request(&mut reader).await.expect("watch request");
                let mut stream = reader.into_inner();
                write_response(
                    &mut stream,
                    &ResponseEnvelope::success(request.id, ResponsePayload::Acknowledged),
                )
                .await
                .expect("watch response");
                write_event(
                    &mut stream,
                    &EventEnvelope::new(Event::StatusChanged(StatusSnapshot {
                        service,
                        ..StatusSnapshot::default()
                    })),
                )
                .await
                .expect("status event");
            }
        });

        let client = Client::connect_path(path).await.expect("connect client");
        let mut subscription = client.watch_status(WatchOptions {
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(2),
            channel_capacity: 4,
        });
        let first = subscription
            .recv()
            .await
            .expect("first item")
            .expect("status");
        assert_eq!(first.service, ServiceState::Starting);
        assert!(subscription.recv().await.expect("disconnect item").is_err());
        let second = subscription
            .recv()
            .await
            .expect("reconnected item")
            .expect("status");
        assert_eq!(second.service, ServiceState::Ready);
        subscription.cancel();
        task.await.expect("server task");
    }

    #[tokio::test]
    async fn server_sets_permissions_and_cleans_up() {
        let temp = TempDir::new();
        let path = temp.socket();
        {
            let server = Server::bind_path(&path).expect("bind server");
            assert_eq!(
                std::fs::metadata(path.parent().expect("parent"))
                    .expect("directory metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(server.socket_path())
                    .expect("socket metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn second_server_never_unlinks_a_live_socket() {
        let temp = TempDir::new();
        let path = temp.socket();
        let server = Server::bind_path(&path).expect("bind first server");
        let error = match Server::bind_path(&path) {
            Ok(_) => panic!("live socket must be retained"),
            Err(error) => error,
        };
        assert!(matches!(error, Error::Io(ref error) if error.kind() == io::ErrorKind::AddrInUse));
        let client = Client::connect_path(&path)
            .await
            .expect("socket remains live");
        drop(client);
        drop(server);
    }

    #[tokio::test]
    async fn oversized_records_are_rejected_before_newline() {
        let (mut writer, reader) = UnixStream::pair().expect("socket pair");
        let read = tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            read_record::<_, serde_json::Value>(&mut reader, 8, DEFAULT_IO_TIMEOUT).await
        });
        writer.write_all(b"123456789").await.expect("write bytes");
        assert!(matches!(
            read.await.expect("reader task"),
            Err(Error::RecordTooLarge { limit: 8 })
        ));
    }

    #[tokio::test]
    async fn status_subscription_discards_queued_older_values() {
        let temp = TempDir::new();
        let server = Server::bind_path(temp.socket()).expect("bind server");
        let path = server.socket_path().to_path_buf();
        let task = tokio::spawn(async move {
            let stream = server.accept().await.expect("accept");
            let mut reader = BufReader::new(stream);
            let request = read_request(&mut reader).await.expect("watch request");
            let mut stream = reader.into_inner();
            write_response(
                &mut stream,
                &ResponseEnvelope::success(request.id, ResponsePayload::Acknowledged),
            )
            .await
            .expect("response");
            for service in [
                ServiceState::Starting,
                ServiceState::Stopping,
                ServiceState::Ready,
            ] {
                write_event(
                    &mut stream,
                    &EventEnvelope::new(Event::StatusChanged(StatusSnapshot {
                        service,
                        ..StatusSnapshot::default()
                    })),
                )
                .await
                .expect("event");
            }
            sleep(Duration::from_millis(100)).await;
        });

        let client = Client::connect_path(path).await.expect("connect client");
        let mut subscription = client.watch_status(WatchOptions::default());
        sleep(Duration::from_millis(30)).await;
        let status = subscription
            .recv()
            .await
            .expect("subscription item")
            .expect("status");
        assert_eq!(status.service, ServiceState::Ready);
        subscription.cancel();
        task.await.expect("server task");
    }

    #[tokio::test]
    async fn client_rejects_response_id_mismatch() {
        let temp = TempDir::new();
        let server = Server::bind_path(temp.socket()).expect("bind server");
        let path = server.socket_path().to_path_buf();
        let task = tokio::spawn(async move {
            let stream = server.accept().await.expect("accept");
            let mut reader = BufReader::new(stream);
            let request =
                read_request_with_limits(&mut reader, DEFAULT_MAX_RECORD_SIZE, DEFAULT_IO_TIMEOUT)
                    .await
                    .expect("request");
            let mut response = ResponseEnvelope::success(
                request.id,
                ResponsePayload::Status(StatusSnapshot::default()),
            );
            response.id = RequestEnvelope::new(Request::GetStatus).id;
            write_response(&mut reader.into_inner(), &response)
                .await
                .expect("response");
        });
        let client = Client::connect_path(path).await.expect("connect client");
        assert!(matches!(
            client.request(Request::GetStatus).await,
            Err(Error::IdMismatch { .. })
        ));
        task.await.expect("server task");
    }

    #[tokio::test]
    async fn client_rejects_response_version_mismatch() {
        let temp = TempDir::new();
        let server = Server::bind_path(temp.socket()).expect("bind server");
        let path = server.socket_path().to_path_buf();
        let task = tokio::spawn(async move {
            let stream = server.accept().await.expect("accept");
            let mut reader = BufReader::new(stream);
            let request = read_request(&mut reader).await.expect("request");
            let mut response = ResponseEnvelope::success(request.id, ResponsePayload::Acknowledged);
            response.version = PROTOCOL_VERSION + 1;
            write_response(&mut reader.into_inner(), &response)
                .await
                .expect("response");
        });
        let client = Client::connect_path(path).await.expect("connect client");
        assert!(matches!(
            client.request(Request::GetStatus).await,
            Err(Error::Protocol(ProtocolError::VersionMismatch { .. }))
        ));
        task.await.expect("server task");
    }

    #[tokio::test]
    async fn request_versions_are_checked() {
        let (mut writer, reader) = UnixStream::pair().expect("socket pair");
        let mut request = RequestEnvelope::new(Request::GetStatus);
        request.version = PROTOCOL_VERSION + 1;
        write_record(
            &mut writer,
            &request,
            DEFAULT_MAX_RECORD_SIZE,
            DEFAULT_IO_TIMEOUT,
        )
        .await
        .expect("write request");
        let mut reader = BufReader::new(reader);
        assert!(matches!(
            read_request_with_limits(&mut reader, DEFAULT_MAX_RECORD_SIZE, DEFAULT_IO_TIMEOUT)
                .await,
            Err(Error::Protocol(ProtocolError::VersionMismatch { .. }))
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn clients_reject_symlink_socket_paths() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new();
        let server = Server::bind_path(temp.socket()).expect("bind server");
        let link = temp.0.join("linked.sock");
        symlink(server.socket_path(), &link).expect("create symlink");
        assert!(matches!(
            Client::connect_path(link).await,
            Err(Error::Symlink(_))
        ));
    }
}
