mod config_store;
mod daemon;

use std::sync::Arc;

use anyhow::Result;
use daemon::Daemon;
use lightsync_domain::{Event, EventEnvelope, Request, ResponseEnvelope};
use lightsync_ipc::{Server, read_request, write_event, write_response};
use tokio::io::BufReader;
use tokio::net::UnixStream;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    initialize_tracing();
    let config_path = config_store::config_path()?;
    let daemon = Daemon::load(config_path).await?;
    daemon.initialize_bridge().await;
    daemon.auto_start().await;
    let server = Arc::new(Server::bind()?);
    tracing::info!(socket = %server.socket_path().display(), "lightsyncd is ready");

    loop {
        tokio::select! {
            result = server.accept() => {
                match result {
                    Ok(stream) => {
                        let daemon = daemon.clone();
                        tokio::spawn(async move {
                            if let Err(error) = serve_client(daemon, stream).await {
                                tracing::debug!(error = %error, "IPC client disconnected");
                            }
                        });
                    }
                    Err(error) => tracing::warn!(error = %error, "IPC accept failed"),
                }
            }
            signal = shutdown_signal() => {
                signal?;
                tracing::info!("shutdown requested");
                daemon.shutdown().await;
                return Ok(());
            }
        }
    }
}

async fn serve_client(daemon: Daemon, stream: UnixStream) -> lightsync_ipc::Result<()> {
    let mut reader = BufReader::new(stream);
    let request = read_request(&mut reader).await?;
    let watch = matches!(request.request, Request::WatchStatus { enabled: true });
    let mut statuses = daemon.subscribe_status();
    let mut events = daemon.subscribe();
    let response = match daemon.dispatch(request.request).await {
        Ok(payload) => ResponseEnvelope::success(request.id, payload),
        Err(error) => ResponseEnvelope::error(request.id, error),
    };
    let mut stream = reader.into_inner();
    write_response(&mut stream, &response).await?;
    if !watch
        || !matches!(
            response.response,
            lightsync_domain::Response::Success { .. }
        )
    {
        return Ok(());
    }
    let status = statuses.borrow_and_update().clone();
    write_event(
        &mut stream,
        &EventEnvelope::new(Event::StatusChanged(status)),
    )
    .await?;
    loop {
        tokio::select! {
            changed = statuses.changed() => {
                changed.map_err(|_| std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "status publisher closed",
                ))?;
                let status = statuses.borrow_and_update().clone();
                write_event(
                    &mut stream,
                    &EventEnvelope::new(Event::StatusChanged(status)),
                )
                .await?;
            }
            event = events.recv() => match event {
                Ok(event) if !matches!(event.event, Event::StatusChanged(_)) => {
                    write_event(&mut stream, &event).await?;
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
            }
        }
    }
}

async fn shutdown_signal() -> std::io::Result<()> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = terminate.recv() => Ok(()),
    }
}

fn initialize_tracing() {
    use tracing_subscriber::prelude::*;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_target(true))
        .with(tracing_journald::layer().ok())
        .init();
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use lightsync_domain::{RequestEnvelope, Response, ResponsePayload};
    use lightsync_ipc::{DEFAULT_IO_TIMEOUT, DEFAULT_MAX_RECORD_SIZE, read_record, write_record};

    use super::*;

    static NEXT: AtomicU64 = AtomicU64::new(0);

    #[tokio::test]
    async fn ipc_dispatches_a_typed_request() {
        let path = std::env::temp_dir().join(format!(
            "lightsync-daemon-ipc-{}-{}/config.toml",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let daemon = Daemon::load(path).await.expect("daemon");
        let (mut client, server) = UnixStream::pair().expect("socket pair");
        let task = tokio::spawn(serve_client(daemon, server));
        let request = RequestEnvelope::new(Request::GetCapabilities);
        write_record(
            &mut client,
            &request,
            DEFAULT_MAX_RECORD_SIZE,
            DEFAULT_IO_TIMEOUT,
        )
        .await
        .expect("request");
        let mut reader = BufReader::new(client);
        let response: ResponseEnvelope =
            read_record(&mut reader, DEFAULT_MAX_RECORD_SIZE, DEFAULT_IO_TIMEOUT)
                .await
                .expect("response");
        assert_eq!(response.id, request.id);
        assert!(matches!(
            response.response,
            Response::Success {
                payload: ResponsePayload::Capabilities(_)
            }
        ));
        task.await.expect("server task").expect("server result");
    }

    #[tokio::test]
    async fn status_snapshot_consumes_all_older_watch_versions() {
        let path = std::env::temp_dir().join(format!(
            "lightsync-daemon-watch-{}-{}/config.toml",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let daemon = Daemon::load(path).await.expect("daemon");
        let mut statuses = daemon.subscribe_status();
        daemon
            .set_bridge_state(lightsync_domain::BridgeState::Connecting, None)
            .await;
        daemon
            .set_bridge_state(lightsync_domain::BridgeState::Connected, None)
            .await;
        assert_eq!(
            statuses.borrow_and_update().bridge,
            lightsync_domain::BridgeState::Connected
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), statuses.changed())
                .await
                .is_err()
        );
    }
}
