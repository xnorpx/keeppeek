use crate::api::{CameraId, CameraStatus};
use polling::{Events, Poller};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    time::Duration,
};

#[derive(Debug)]
pub enum RouterQuery {
    ListCameras,
    GetCamera(CameraId),
}

#[derive(Debug, PartialEq, Eq)]
pub enum RouterResponse {
    Cameras(Vec<CameraStatus>),
    Camera(CameraStatus),
}

#[derive(Debug, PartialEq, Eq)]
pub enum RouterError {
    CameraNotFound(CameraId),
}

#[derive(Debug)]
pub enum WorkerEvent {
    StatusChanged(CameraStatus),
}

pub type RouterReply = SyncSender<Result<RouterResponse, RouterError>>;

#[derive(Debug)]
pub enum RouterMessage {
    Query {
        query: RouterQuery,
        reply: RouterReply,
    },
    WorkerEvent(WorkerEvent),
    Shutdown,
}

#[derive(Debug)]
pub enum FacadeSendError<T> {
    Disconnected(T),
    Notify(std::io::Error),
}

pub struct FacadeSender<T> {
    tx: mpsc::Sender<T>,
    poller: Arc<Poller>,
}

impl<T> Clone for FacadeSender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            poller: self.poller.clone(),
        }
    }
}

impl<T> FacadeSender<T> {
    pub fn send(&self, message: T) -> Result<(), FacadeSendError<T>> {
        self.tx
            .send(message)
            .map_err(|error| FacadeSendError::Disconnected(error.0))?;
        self.poller.notify().map_err(FacadeSendError::Notify)
    }
}

pub struct Router {
    rx: Receiver<RouterMessage>,
    poller: Arc<Poller>,
    events: Events,
    cameras: HashMap<CameraId, CameraStatus>,
    shutting_down: bool,
}

impl Router {
    pub fn new() -> std::io::Result<(Self, FacadeSender<RouterMessage>)> {
        let poller = Arc::new(Poller::new()?);
        let (tx, rx) = mpsc::channel();
        let sender = FacadeSender {
            tx,
            poller: poller.clone(),
        };
        Ok((
            Self {
                rx,
                poller,
                events: Events::new(),
                cameras: HashMap::new(),
                shutting_down: false,
            },
            sender,
        ))
    }

    pub const fn is_shutting_down(&self) -> bool {
        self.shutting_down
    }

    pub fn wait_and_drain(&mut self, timeout: Option<Duration>) -> std::io::Result<usize> {
        self.events.clear();
        self.poller.wait(&mut self.events, timeout)?;
        Ok(self.drain_pending())
    }

    fn drain_pending(&mut self) -> usize {
        let mut handled = 0;
        loop {
            match self.rx.try_recv() {
                Ok(message) => {
                    handled += 1;
                    self.handle(message);
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return handled,
            }
        }
    }

    fn handle(&mut self, message: RouterMessage) {
        match message {
            RouterMessage::Query { query, reply } => {
                let _ = reply.send(self.query(query));
            }
            RouterMessage::WorkerEvent(WorkerEvent::StatusChanged(status)) => {
                self.cameras.insert(status.id.clone(), status);
            }
            RouterMessage::Shutdown => self.shutting_down = true,
        }
    }

    pub fn query(&self, query: RouterQuery) -> Result<RouterResponse, RouterError> {
        match query {
            RouterQuery::ListCameras => {
                let mut cameras = self.cameras.values().cloned().collect::<Vec<_>>();
                cameras.sort_unstable_by_key(|status| status.id.clone());
                Ok(RouterResponse::Cameras(cameras))
            }
            RouterQuery::GetCamera(id) => self
                .cameras
                .get(&id)
                .cloned()
                .map(RouterResponse::Camera)
                .ok_or(RouterError::CameraNotFound(id)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::CameraLifecycle;
    use std::time::Instant;

    fn status(id: &str, lifecycle: CameraLifecycle) -> CameraStatus {
        CameraStatus {
            id: CameraId::new(id),
            lifecycle,
            last_error: None,
        }
    }

    #[test]
    fn facade_wakes_blocked_router() {
        let (mut router, sender) = Router::new().unwrap();
        let thread = std::thread::spawn(move || {
            let started = Instant::now();
            let handled = router.wait_and_drain(Some(Duration::from_secs(5))).unwrap();
            (handled, started.elapsed())
        });

        sender.send(RouterMessage::Shutdown).unwrap();
        let (handled, elapsed) = thread.join().unwrap();

        assert_eq!(handled, 1);
        assert!(elapsed < Duration::from_secs(1));
    }

    #[test]
    fn one_wakeup_drains_all_pending_messages() {
        let (mut router, sender) = Router::new().unwrap();
        sender
            .send(RouterMessage::WorkerEvent(WorkerEvent::StatusChanged(
                status("garage", CameraLifecycle::Starting),
            )))
            .unwrap();
        sender
            .send(RouterMessage::WorkerEvent(WorkerEvent::StatusChanged(
                status("garage", CameraLifecycle::Connected),
            )))
            .unwrap();

        assert_eq!(router.wait_and_drain(Some(Duration::ZERO)).unwrap(), 2);

        let response = router.query(RouterQuery::GetCamera(CameraId::new("garage")));
        assert_eq!(
            response,
            Ok(RouterResponse::Camera(status(
                "garage",
                CameraLifecycle::Connected
            )))
        );
    }

    #[test]
    fn repeated_waits_reuse_the_event_buffer() {
        let (mut router, sender) = Router::new().unwrap();

        for _ in 0..2_048 {
            sender
                .send(RouterMessage::WorkerEvent(WorkerEvent::StatusChanged(
                    status("garage", CameraLifecycle::Connected),
                )))
                .unwrap();
            assert_eq!(router.wait_and_drain(Some(Duration::ZERO)).unwrap(), 1);
        }
    }

    #[test]
    fn query_replies_and_missing_receivers_do_not_stop_router() {
        let (mut router, sender) = Router::new().unwrap();
        let (dropped_reply, dropped_rx) = mpsc::sync_channel(1);
        drop(dropped_rx);
        sender
            .send(RouterMessage::Query {
                query: RouterQuery::ListCameras,
                reply: dropped_reply,
            })
            .unwrap();

        let (reply, rx) = mpsc::sync_channel(1);
        sender
            .send(RouterMessage::Query {
                query: RouterQuery::GetCamera(CameraId::new("missing")),
                reply,
            })
            .unwrap();

        assert_eq!(router.wait_and_drain(Some(Duration::ZERO)).unwrap(), 2);
        assert_eq!(
            rx.recv().unwrap(),
            Err(RouterError::CameraNotFound(CameraId::new("missing")))
        );
        assert!(!router.is_shutting_down());
    }

    #[test]
    fn shutdown_is_observed_after_prior_events() {
        let (mut router, sender) = Router::new().unwrap();
        sender
            .send(RouterMessage::WorkerEvent(WorkerEvent::StatusChanged(
                status("door", CameraLifecycle::ShuttingDown),
            )))
            .unwrap();
        sender.send(RouterMessage::Shutdown).unwrap();

        assert_eq!(router.wait_and_drain(Some(Duration::ZERO)).unwrap(), 2);
        assert!(router.is_shutting_down());
        assert_eq!(
            router.query(RouterQuery::GetCamera(CameraId::new("door"))),
            Ok(RouterResponse::Camera(status(
                "door",
                CameraLifecycle::ShuttingDown
            )))
        );
    }

    fn assert_send<T: Send>() {}

    #[test]
    fn cross_thread_messages_are_send() {
        assert_send::<RouterMessage>();
        assert_send::<WorkerEvent>();
        assert_send::<CameraStatus>();
    }
}
