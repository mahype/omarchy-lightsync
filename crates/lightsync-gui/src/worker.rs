use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};

use lightsync_domain::{Request, ResponsePayload, StatusSnapshot};
use lightsync_ipc::{Client, Error, WatchOptions};
use tokio::sync::mpsc as tokio_mpsc;

#[derive(Debug)]
pub enum WorkerEvent {
    Status(StatusSnapshot),
    Disconnected,
    Response {
        id: u64,
        request: Request,
        result: Box<Result<ResponsePayload, Error>>,
    },
}

struct Command {
    id: u64,
    request: Request,
}

#[derive(Clone)]
pub struct Worker {
    sender: tokio_mpsc::UnboundedSender<Command>,
    next_id: Arc<AtomicU64>,
}

fn is_transport_failure(error: &Error) -> bool {
    !matches!(error, Error::Remote(_))
}

impl Worker {
    pub fn start(events: mpsc::Sender<WorkerEvent>) -> Self {
        let (sender, receiver) = tokio_mpsc::unbounded_channel();
        std::thread::Builder::new()
            .name("lightsync-ipc".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("create IPC runtime");
                runtime.block_on(run(receiver, events));
            })
            .expect("start IPC worker");
        Self {
            sender,
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn send(&self, request: Request) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let _ = self.sender.send(Command { id, request });
        id
    }
}

async fn run(
    mut commands: tokio_mpsc::UnboundedReceiver<Command>,
    events: mpsc::Sender<WorkerEvent>,
) {
    let Ok(client) = Client::from_runtime() else {
        let _ = events.send(WorkerEvent::Disconnected);
        return;
    };
    let mut watch = client.watch_status(WatchOptions::default());
    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(Command { id, request }) = command else { return; };
                let client = client.clone();
                let events = events.clone();
                tokio::spawn(async move {
                    let result = client.request(request.clone()).await;
                    let transport_failed = result.as_ref().is_err_and(is_transport_failure);
                    if transport_failed {
                        let _ = events.send(WorkerEvent::Disconnected);
                    }
                    let _ = events.send(WorkerEvent::Response { id, request, result: Box::new(result) });
                });
            }
            update = watch.recv() => {
                match update {
                    Some(Ok(status)) => {
                        if events.send(WorkerEvent::Status(status)).is_err() { return; }
                    }
                    Some(Err(_)) => {
                        if events.send(WorkerEvent::Disconnected).is_err() { return; }
                    }
                    None => return,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use lightsync_domain::{ActionableError, ErrorCode};

    use super::*;

    #[test]
    fn remote_errors_do_not_mark_the_transport_disconnected() {
        let remote = Error::Remote(ActionableError::new(
            ErrorCode::LinkButtonRequired,
            "press link button",
        ));
        assert!(!is_transport_failure(&remote));
        assert!(is_transport_failure(&Error::RuntimeDirMissing));
    }
}
