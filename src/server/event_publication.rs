use super::{
    ControlCommandError, PUBLISHED_DETECTION_EVENT_TYPES, ServerState, millis_timestamp,
    proto_camera_source_session, required_timestamp_ms, validate_client_id,
};
use crate::{
    api::proto::{self, event_publication_command},
    storage::events::{PublishedImageCommit, PublishedImageCommitError},
    storage::metadata::{EventAttachment, EventSource, TimelineEvent},
    webrtc::SessionId,
};
use prost::Message as _;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

const MAXIMUM_ACTIVE_PUBLICATIONS: usize = 64;
const MAXIMUM_PUBLICATION_IDS: usize = 256;
const MAXIMUM_PUBLICATION_IDS_PER_SESSION: usize = 64;
const MAXIMUM_PUBLICATIONS_PER_SESSION: usize = 4;
pub(super) const MAXIMUM_ATTACHMENT_BYTES: u64 = 8 * 1024 * 1024;
const MAXIMUM_EVENT_ATTACHMENT_BYTES: u64 = 32 * 1024 * 1024;
const MAXIMUM_STAGED_BYTES: u64 = 64 * 1024 * 1024;
const MAXIMUM_ATTACHMENT_CHUNKS: u32 = 256;
const MAXIMUM_ATTACHMENT_TEXT_CHARS: usize = 4_096;
const MAXIMUM_EVENT_TEXT_CHARS: usize = 4_096;
const MAXIMUM_EVENT_PAYLOAD_BYTES: usize = 16 * 1_024;
const MAXIMUM_EVENT_PAYLOAD_NODES: usize = 256;
const MAXIMUM_EVENT_PAYLOAD_DEPTH: usize = 8;
const MAXIMUM_EVENT_PAYLOAD_COLLECTION_ITEMS: usize = 64;
const MAXIMUM_EVENT_PAYLOAD_KEY_BYTES: usize = 128;
const PUBLICATION_TTL_MS: u64 = 30_000;
const DEFAULT_COMMIT_WAIT_MS: u64 = 1_000;
const MAXIMUM_COMMIT_LATENCY_SAMPLES: usize = 256;

#[derive(Clone, Default)]
pub(super) struct Registry {
    inner: Arc<RegistryInner>,
}

#[derive(Default)]
struct RegistryInner {
    publications: Mutex<HashMap<(SessionId, String), StagedPublication>>,
    changed: Condvar,
    commit: Mutex<()>,
    metrics: PublicationMetrics,
}

#[derive(Default)]
struct PublicationMetrics {
    starts: AtomicU64,
    commits: AtomicU64,
    aborts: AtomicU64,
    expirations: AtomicU64,
    rejections: AtomicU64,
    storage_failures: AtomicU64,
    commit_latencies_ms: Mutex<VecDeque<u64>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PublicationStatus {
    Accepting,
    Waiting,
    Committing,
    Committed,
    Aborted,
    Expired,
}

struct StagedPublication {
    event: proto::Event,
    attachment_channel: proto::DataChannelKind,
    expires_at_ms: u64,
    status: PublicationStatus,
    expiry_notification_pending: bool,
    attachment_bytes: Vec<u8>,
    reserved_bytes: u64,
    chunk_count: Option<u32>,
    next_chunk_index: u32,
    started_at: Instant,
}

struct PendingCommit {
    event: proto::Event,
    attachment_channel: proto::DataChannelKind,
    expires_at_ms: u64,
    attachment_bytes: Vec<u8>,
    started_at: Instant,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct MetricsSnapshot {
    pub(super) active: u64,
    pub(super) staged_bytes: u64,
    pub(super) starts: u64,
    pub(super) commits: u64,
    pub(super) aborts: u64,
    pub(super) expirations: u64,
    pub(super) rejections: u64,
    pub(super) storage_failures: u64,
    pub(super) commit_latency_ms_p50: u64,
    pub(super) commit_latency_ms_p95: u64,
}

pub(super) struct CommittedPublication {
    pub(super) event: proto::Event,
    pub(super) timeline_event: TimelineEvent,
    pub(super) attachment_bytes: Arc<[u8]>,
}

pub(super) struct Dispatch {
    pub(super) result: proto::ok::Result,
    pub(super) committed: Option<CommittedPublication>,
    pub(super) mqtt_retry: Option<TimelineEvent>,
}

enum CommitPreparation {
    Committed(Box<proto::EventPublicationState>),
    Pending(Box<PendingCommit>),
}

impl Registry {
    fn start(
        &self,
        state: &ServerState,
        session_id: SessionId,
        request: proto::StartEventPublication,
        now_ms: u64,
    ) -> Result<proto::EventPublicationState, ControlCommandError> {
        validate_path_id(&request.publication_id, "event publication ID")?;
        let event = request.event.ok_or_else(|| {
            publication_error(
                &request.publication_id,
                "",
                proto::EventPublicationErrorCode::EventInvalid,
                None,
                "event publication is missing its event",
            )
        })?;
        let key = (session_id, request.publication_id.clone());
        {
            let mut publications = self
                .inner
                .publications
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.record_expirations(expire_publications(&mut publications, now_ms));
            if let Some(existing) = publications.get(&key) {
                return retry_start(
                    &request.publication_id,
                    &event,
                    request.attachment_channel,
                    existing,
                );
            }
        }
        let channel = validate_start(
            state,
            &request.publication_id,
            &event,
            request.attachment_channel,
        )?;
        let expires_at_ms = now_ms.saturating_add(PUBLICATION_TTL_MS);
        let mut publications = self
            .inner
            .publications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.record_expirations(expire_publications(&mut publications, now_ms));
        if let Some(existing) = publications.get(&key) {
            return retry_start(&request.publication_id, &event, channel as i32, existing);
        }
        if publication_limit_reached(&publications, session_id) {
            return Err(publication_error(
                &request.publication_id,
                &event.event_id,
                proto::EventPublicationErrorCode::SizeLimitExceeded,
                None,
                "event publication limit reached",
            ));
        }
        let publication = StagedPublication {
            event,
            attachment_channel: channel,
            expires_at_ms,
            status: PublicationStatus::Accepting,
            expiry_notification_pending: false,
            attachment_bytes: Vec::new(),
            reserved_bytes: 0,
            chunk_count: None,
            next_chunk_index: 0,
            started_at: Instant::now(),
        };
        let response = publication_state(&request.publication_id, &publication);
        publications.insert(key, publication);
        self.inner.metrics.starts.fetch_add(1, Ordering::Relaxed);
        Ok(response)
    }

    fn ingest(
        &self,
        state: &ServerState,
        session_id: SessionId,
        channel: proto::DataChannelKind,
        chunk: proto::EventAttachmentChunk,
        now_ms: u64,
    ) -> Result<(), ControlCommandError> {
        let publication_id = match chunk.context.as_ref() {
            Some(proto::event_attachment_chunk::Context::PublicationId(id)) => id,
            _ => {
                return Err(publication_error(
                    "",
                    &chunk.event_id,
                    proto::EventPublicationErrorCode::AttachmentInvalid,
                    None,
                    "event attachment has no publication context",
                ));
            }
        };
        let key = (session_id, publication_id.to_owned());
        let mut publications = self
            .inner
            .publications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.record_expirations(expire_publications(&mut publications, now_ms));
        let total_staged_bytes = publications
            .values()
            .try_fold(0_u64, |total, publication| {
                total.checked_add(publication.reserved_bytes)
            })
            .ok_or_else(|| {
                publication_error(
                    publication_id,
                    &chunk.event_id,
                    proto::EventPublicationErrorCode::SizeLimitExceeded,
                    None,
                    "event publication staging size overflow",
                )
            })?;
        let publication = publications.get_mut(&key).ok_or_else(|| {
            publication_error(
                publication_id,
                &chunk.event_id,
                proto::EventPublicationErrorCode::StateInvalid,
                None,
                "event publication was not found",
            )
        })?;
        if !matches!(
            publication.status,
            PublicationStatus::Accepting | PublicationStatus::Waiting
        ) {
            return Err(publication_state_error(publication_id, publication));
        }
        validate_event_identity(state, publication_id, &publication.event)?;
        validate_chunk(
            publication_id,
            publication,
            channel,
            &chunk,
            total_staged_bytes,
        )?;
        let chunk_bytes = u64::try_from(chunk.payload.len()).map_err(|_| {
            publication_error(
                publication_id,
                &chunk.event_id,
                proto::EventPublicationErrorCode::SizeLimitExceeded,
                None,
                "event publication staging size overflow",
            )
        })?;
        let reserved_bytes = publication
            .reserved_bytes
            .checked_add(chunk_bytes)
            .ok_or_else(|| {
                publication_error(
                    publication_id,
                    &chunk.event_id,
                    proto::EventPublicationErrorCode::SizeLimitExceeded,
                    None,
                    "event publication staging size overflow",
                )
            })?;
        publication
            .attachment_bytes
            .extend_from_slice(&chunk.payload);
        publication.reserved_bytes = reserved_bytes;
        publication.chunk_count = Some(chunk.chunk_count);
        publication.next_chunk_index = publication.next_chunk_index.saturating_add(1);
        self.inner.changed.notify_all();
        Ok(())
    }

    fn commit(
        &self,
        state: &ServerState,
        session_id: SessionId,
        request: proto::CommitEventPublication,
        now_ms: u64,
    ) -> Result<
        (
            proto::EventPublicationState,
            Option<CommittedPublication>,
            Option<TimelineEvent>,
        ),
        ControlCommandError,
    > {
        validate_path_id(&request.publication_id, "event publication ID")?;
        let wait_ms =
            commit_wait_timeout_ms(&request.publication_id, request.wait_timeout.as_ref())?;
        let key = (session_id, request.publication_id.clone());
        let pending = match self.prepare_commit(&key, &request.publication_id, now_ms, wait_ms)? {
            CommitPreparation::Committed(publication) => {
                let mqtt_retry = state
                    .events
                    .as_ref()
                    .and_then(|store| store.event_by_id(&publication.event_id).ok().flatten());
                return Ok((*publication, None, mqtt_retry));
            }
            CommitPreparation::Pending(pending) => *pending,
        };
        let _commit_guard = self
            .inner
            .commit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let PendingCommit {
            event,
            attachment_channel,
            expires_at_ms,
            attachment_bytes,
            started_at,
        } = pending;
        if super::unix_time_ms() >= expires_at_ms {
            self.finish_failed_commit(&key, attachment_bytes, false, super::unix_time_ms());
            return Err(publication_error(
                &request.publication_id,
                &event.event_id,
                proto::EventPublicationErrorCode::Expired,
                None,
                "event publication expired",
            ));
        }
        let commit_result =
            (|| -> Result<(TimelineEvent, PublishedImageCommit), (ControlCommandError, bool)> {
                validate_event_identity(state, &request.publication_id, &event)
                    .map_err(|error| (error, false))?;
                validate_revision(state, &request.publication_id, &event)
                    .map_err(|error| (error, false))?;
                let timeline_event = timeline_event(&event).map_err(|error| (error, false))?;
                let store = state.events.as_ref().ok_or_else(|| {
                    (
                        publication_error(
                            &request.publication_id,
                            &event.event_id,
                            proto::EventPublicationErrorCode::StorageUnavailable,
                            None,
                            "event storage is unavailable",
                        ),
                        true,
                    )
                })?;
                match store.commit_published_image(
                    &request.publication_id,
                    timeline_event.clone(),
                    &attachment_bytes,
                ) {
                    Ok(outcome) => Ok((timeline_event, outcome)),
                    Err(PublishedImageCommitError::Invalid(_)) => Err((
                        publication_error(
                            &request.publication_id,
                            &event.event_id,
                            proto::EventPublicationErrorCode::AttachmentInvalid,
                            None,
                            "event attachment is not a valid JPEG",
                        ),
                        false,
                    )),
                    Err(PublishedImageCommitError::Conflict(current_revision)) => Err((
                        publication_error(
                            &request.publication_id,
                            &event.event_id,
                            proto::EventPublicationErrorCode::RevisionConflict,
                            current_revision,
                            "event publication revision conflicts with durable state",
                        ),
                        false,
                    )),
                    Err(PublishedImageCommitError::Storage(_)) => {
                        if let Err(error) =
                            validate_revision(state, &request.publication_id, &event)
                        {
                            return Err((error, false));
                        }
                        Err((
                            publication_error(
                                &request.publication_id,
                                &event.event_id,
                                proto::EventPublicationErrorCode::StorageUnavailable,
                                None,
                                "event publication could not be stored",
                            ),
                            true,
                        ))
                    }
                }
            })();
        let (timeline_event, outcome) = match commit_result {
            Ok(result) => result,
            Err((error, retryable)) => {
                self.finish_failed_commit(&key, attachment_bytes, retryable, super::unix_time_ms());
                return Err(error);
            }
        };
        let publication = self.finish_successful_commit(
            &key,
            &request.publication_id,
            &event,
            attachment_channel,
            expires_at_ms,
        );
        let (committed, mqtt_retry) = match outcome {
            PublishedImageCommit::Stored => {
                self.inner.metrics.record_commit(started_at.elapsed());
                (
                    Some(CommittedPublication {
                        event,
                        timeline_event,
                        attachment_bytes: Arc::from(attachment_bytes),
                    }),
                    None,
                )
            }
            PublishedImageCommit::Existing => (None, Some(timeline_event)),
        };
        Ok((publication, committed, mqtt_retry))
    }

    fn prepare_commit(
        &self,
        key: &(SessionId, String),
        publication_id: &str,
        now_ms: u64,
        wait_ms: u64,
    ) -> Result<CommitPreparation, ControlCommandError> {
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(wait_ms))
            .expect("publication wait is capped to thirty seconds");
        let mut publications = self
            .inner
            .publications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut current_ms = now_ms;
        loop {
            self.record_expirations(expire_publications(&mut publications, current_ms));
            let publication = publications.get_mut(key).ok_or_else(|| {
                publication_error(
                    publication_id,
                    "",
                    proto::EventPublicationErrorCode::StateInvalid,
                    None,
                    "event publication was not found",
                )
            })?;
            match publication.status {
                PublicationStatus::Committed => {
                    return Ok(CommitPreparation::Committed(Box::new(publication_state(
                        publication_id,
                        publication,
                    ))));
                }
                PublicationStatus::Accepting | PublicationStatus::Waiting => {}
                PublicationStatus::Committing
                | PublicationStatus::Aborted
                | PublicationStatus::Expired => {
                    return Err(publication_state_error(publication_id, publication));
                }
            }
            if publication_complete(publication) {
                publication.status = PublicationStatus::Committing;
                return Ok(CommitPreparation::Pending(Box::new(PendingCommit {
                    event: publication.event.clone(),
                    attachment_channel: publication.attachment_channel,
                    expires_at_ms: publication.expires_at_ms,
                    attachment_bytes: std::mem::take(&mut publication.attachment_bytes),
                    started_at: publication.started_at,
                })));
            }
            let wait =
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(
                        publication.expires_at_ms.saturating_sub(current_ms),
                    ));
            if wait.is_zero() {
                publication.status = PublicationStatus::Accepting;
                return Err(publication_error(
                    publication_id,
                    &publication.event.event_id,
                    proto::EventPublicationErrorCode::AttachmentsIncomplete,
                    None,
                    "event publication attachments are incomplete",
                ));
            }
            publication.status = PublicationStatus::Waiting;
            self.inner.changed.notify_all();
            publications = match self.inner.changed.wait_timeout(publications, wait) {
                Ok((publications, _)) => publications,
                Err(poisoned) => poisoned.into_inner().0,
            };
            current_ms = super::unix_time_ms();
        }
    }

    fn finish_failed_commit(
        &self,
        key: &(SessionId, String),
        attachment_bytes: Vec<u8>,
        retryable: bool,
        now_ms: u64,
    ) {
        let mut publications = self
            .inner
            .publications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut aborted = false;
        let mut expired = false;
        if let Some(publication) = publications.get_mut(key)
            && publication.status == PublicationStatus::Committing
        {
            if now_ms >= publication.expires_at_ms {
                publication.status = PublicationStatus::Expired;
                publication.expiry_notification_pending = true;
                publication.reserved_bytes = 0;
                expired = true;
            } else if retryable {
                publication.status = PublicationStatus::Accepting;
                publication.attachment_bytes = attachment_bytes;
            } else {
                publication.status = PublicationStatus::Aborted;
                publication.reserved_bytes = 0;
                aborted = true;
            }
        }
        drop(publications);
        self.record_aborts(u64::from(aborted));
        self.record_expirations(u64::from(expired));
        self.inner.changed.notify_all();
    }

    fn finish_successful_commit(
        &self,
        key: &(SessionId, String),
        publication_id: &str,
        event: &proto::Event,
        attachment_channel: proto::DataChannelKind,
        expires_at_ms: u64,
    ) -> proto::EventPublicationState {
        let mut publications = self
            .inner
            .publications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = if let Some(publication) = publications.get_mut(key) {
            publication.status = PublicationStatus::Committed;
            publication.reserved_bytes = 0;
            publication_state(publication_id, publication)
        } else {
            publication_state(
                publication_id,
                &StagedPublication {
                    event: event.clone(),
                    attachment_channel,
                    expires_at_ms,
                    status: PublicationStatus::Committed,
                    expiry_notification_pending: false,
                    attachment_bytes: Vec::new(),
                    reserved_bytes: 0,
                    chunk_count: None,
                    next_chunk_index: 0,
                    started_at: Instant::now(),
                },
            )
        };
        self.inner.changed.notify_all();
        state
    }

    fn abort(
        &self,
        session_id: SessionId,
        request: proto::AbortEventPublication,
        now_ms: u64,
    ) -> Result<proto::EventPublicationState, ControlCommandError> {
        validate_path_id(&request.publication_id, "event publication ID")?;
        let key = (session_id, request.publication_id.clone());
        let mut publications = self
            .inner
            .publications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.record_expirations(expire_publications(&mut publications, now_ms));
        let publication = publications.get_mut(&key).ok_or_else(|| {
            publication_error(
                &request.publication_id,
                "",
                proto::EventPublicationErrorCode::StateInvalid,
                None,
                "event publication was not found",
            )
        })?;
        if matches!(
            publication.status,
            PublicationStatus::Committing | PublicationStatus::Committed
        ) {
            return Err(publication_state_error(
                &request.publication_id,
                publication,
            ));
        }
        let transitioned = publication.status != PublicationStatus::Aborted;
        publication.status = PublicationStatus::Aborted;
        publication.attachment_bytes.clear();
        publication.reserved_bytes = 0;
        self.record_aborts(u64::from(transitioned));
        self.inner.changed.notify_all();
        Ok(publication_state(&request.publication_id, publication))
    }

    pub(super) fn close_session(&self, session_id: SessionId) {
        let _commit_guard = self
            .inner
            .commit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut aborted = 0_u64;
        self.inner
            .publications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(owner, _), publication| {
                let remove = *owner == session_id;
                if remove && publication.status.is_active() {
                    aborted = aborted.saturating_add(1);
                }
                !remove
            });
        self.record_aborts(aborted);
        self.inner.changed.notify_all();
    }

    pub(super) fn invalidate_source(&self, source_id: &str) {
        let _commit_guard = self
            .inner
            .commit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut publications = self
            .inner
            .publications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut aborted = 0_u64;
        for publication in publications.values_mut() {
            if publication.event.source_id == source_id && publication.status.is_active() {
                publication.status = PublicationStatus::Aborted;
                publication.attachment_bytes.clear();
                publication.reserved_bytes = 0;
                aborted = aborted.saturating_add(1);
            }
        }
        drop(publications);
        self.record_aborts(aborted);
        self.inner.changed.notify_all();
    }

    pub(super) fn expire(&self, now_ms: u64) -> Vec<(SessionId, proto::EventPublicationState)> {
        let mut publications = self
            .inner
            .publications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.record_expirations(expire_publications(&mut publications, now_ms));
        let mut expired = Vec::new();
        for ((session_id, publication_id), publication) in publications.iter_mut() {
            if publication.expiry_notification_pending {
                expired.push((*session_id, publication_state(publication_id, publication)));
                publication.expiry_notification_pending = false;
            }
        }
        if !expired.is_empty() {
            self.inner.changed.notify_all();
        }
        expired
    }

    pub(super) fn metrics_snapshot(&self) -> MetricsSnapshot {
        let publications = self
            .inner
            .publications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let active = publications
            .values()
            .filter(|publication| publication.status.is_active())
            .count() as u64;
        let staged_bytes = publications
            .values()
            .map(|publication| publication.reserved_bytes)
            .sum();
        let latencies = self
            .inner
            .metrics
            .commit_latencies_ms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        MetricsSnapshot {
            active,
            staged_bytes,
            starts: self.inner.metrics.starts.load(Ordering::Relaxed),
            commits: self.inner.metrics.commits.load(Ordering::Relaxed),
            aborts: self.inner.metrics.aborts.load(Ordering::Relaxed),
            expirations: self.inner.metrics.expirations.load(Ordering::Relaxed),
            rejections: self.inner.metrics.rejections.load(Ordering::Relaxed),
            storage_failures: self.inner.metrics.storage_failures.load(Ordering::Relaxed),
            commit_latency_ms_p50: latency_percentile(&latencies, 50),
            commit_latency_ms_p95: latency_percentile(&latencies, 95),
        }
    }

    fn record_error(&self, error: &ControlCommandError) {
        self.inner
            .metrics
            .rejections
            .fetch_add(1, Ordering::Relaxed);
        let storage_failure = error.details.iter().any(|detail| {
            proto::EventPublicationError::decode(detail.value.as_slice()).is_ok_and(|detail| {
                detail.code == proto::EventPublicationErrorCode::StorageUnavailable as i32
            })
        });
        if storage_failure {
            self.inner
                .metrics
                .storage_failures
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    fn record_aborts(&self, count: u64) {
        self.inner
            .metrics
            .aborts
            .fetch_add(count, Ordering::Relaxed);
    }

    fn record_expirations(&self, count: u64) {
        self.inner
            .metrics
            .expirations
            .fetch_add(count, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(super) fn wait_until_commit_waiting(&self, session_id: SessionId, publication_id: &str) {
        let key = (session_id, publication_id.to_owned());
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut publications = self.inner.publications.lock().unwrap();
        loop {
            if publications
                .get(&key)
                .is_some_and(|publication| publication.status == PublicationStatus::Waiting)
            {
                return;
            }
            let wait = deadline.saturating_duration_since(Instant::now());
            assert!(!wait.is_zero(), "commit did not enter the waiting state");
            publications = self
                .inner
                .changed
                .wait_timeout(publications, wait)
                .unwrap()
                .0;
        }
    }
}

impl PublicationStatus {
    const fn is_active(self) -> bool {
        matches!(self, Self::Accepting | Self::Waiting | Self::Committing)
    }
}

fn publication_limit_reached(
    publications: &HashMap<(SessionId, String), StagedPublication>,
    session_id: SessionId,
) -> bool {
    let session_ids = publications
        .keys()
        .filter(|(owner, _)| *owner == session_id)
        .count();
    let active = publications
        .values()
        .filter(|publication| publication.status.is_active())
        .count();
    let session_active = publications
        .iter()
        .filter(|((owner, _), publication)| *owner == session_id && publication.status.is_active())
        .count();
    publications.len() >= MAXIMUM_PUBLICATION_IDS
        || session_ids >= MAXIMUM_PUBLICATION_IDS_PER_SESSION
        || active >= MAXIMUM_ACTIVE_PUBLICATIONS
        || session_active >= MAXIMUM_PUBLICATIONS_PER_SESSION
}

impl PublicationMetrics {
    fn record_commit(&self, elapsed: Duration) {
        self.commits.fetch_add(1, Ordering::Relaxed);
        let mut samples = self
            .commit_latencies_ms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if samples.len() == MAXIMUM_COMMIT_LATENCY_SAMPLES {
            samples.pop_front();
        }
        samples.push_back(elapsed.as_millis().try_into().unwrap_or(u64::MAX));
    }
}

fn latency_percentile(samples: &VecDeque<u64>, percentile: usize) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let mut ordered = samples.iter().copied().collect::<Vec<_>>();
    ordered.sort_unstable();
    let rank = ordered.len().saturating_mul(percentile).div_ceil(100);
    ordered[rank.saturating_sub(1).min(ordered.len() - 1)]
}

fn retry_start(
    publication_id: &str,
    event: &proto::Event,
    attachment_channel: i32,
    existing: &StagedPublication,
) -> Result<proto::EventPublicationState, ControlCommandError> {
    if existing.event == *event && existing.attachment_channel as i32 == attachment_channel {
        Ok(publication_state(publication_id, existing))
    } else {
        Err(publication_error(
            publication_id,
            &event.event_id,
            proto::EventPublicationErrorCode::StateInvalid,
            None,
            "event publication ID already has different content",
        ))
    }
}

pub(super) fn dispatch(
    state: &ServerState,
    session_id: SessionId,
    command: proto::EventPublicationCommand,
) -> Result<Dispatch, ControlCommandError> {
    let result = dispatch_inner(state, session_id, command);
    if let Err(error) = &result {
        state.event_publications.record_error(error);
    }
    result
}

fn dispatch_inner(
    state: &ServerState,
    session_id: SessionId,
    command: proto::EventPublicationCommand,
) -> Result<Dispatch, ControlCommandError> {
    let mut committed = None;
    let mut mqtt_retry = None;
    let publication =
        match command.action {
            Some(event_publication_command::Action::Start(request)) => state
                .event_publications
                .start(state, session_id, request, super::unix_time_ms())?,
            Some(event_publication_command::Action::Commit(request)) => {
                let (publication, committed_publication, retry) = state.event_publications.commit(
                    state,
                    session_id,
                    request,
                    super::unix_time_ms(),
                )?;
                committed = committed_publication;
                mqtt_retry = retry;
                publication
            }
            Some(event_publication_command::Action::Abort(request)) => state
                .event_publications
                .abort(session_id, request, super::unix_time_ms())?,
            None => {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "event publication command has no action",
                ));
            }
        };
    Ok(Dispatch {
        result: proto::ok::Result::EventPublicationState(publication),
        committed,
        mqtt_retry,
    })
}

pub(super) fn ingest(
    state: &ServerState,
    session_id: SessionId,
    channel: proto::DataChannelKind,
    message: proto::Message,
) -> Result<(), ControlCommandError> {
    let result = ingest_inner(state, session_id, channel, message);
    if let Err(error) = &result {
        state.event_publications.record_error(error);
    }
    result
}

fn ingest_inner(
    state: &ServerState,
    session_id: SessionId,
    channel: proto::DataChannelKind,
    message: proto::Message,
) -> Result<(), ControlCommandError> {
    let Some(proto::message::Message::Event(proto::EventMessage {
        message: Some(proto::event_message::Message::Attachment(chunk)),
    })) = message.message
    else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "event data channel requires an attachment chunk",
        ));
    };
    state
        .event_publications
        .ingest(state, session_id, channel, chunk, super::unix_time_ms())
}

fn publication_complete(publication: &StagedPublication) -> bool {
    let expected_bytes = publication.event.attachments[0]
        .byte_len
        .unwrap_or_default();
    publication.chunk_count.is_some()
        && publication.chunk_count == Some(publication.next_chunk_index)
        && u64::try_from(publication.attachment_bytes.len()) == Ok(expected_bytes)
        && publication.reserved_bytes == expected_bytes
}

fn commit_wait_timeout_ms(
    publication_id: &str,
    duration: Option<&prost_types::Duration>,
) -> Result<u64, ControlCommandError> {
    let Some(duration) = duration else {
        return Ok(DEFAULT_COMMIT_WAIT_MS);
    };
    if duration.seconds < 0 || !(0..1_000_000_000).contains(&duration.nanos) {
        return Err(publication_error(
            publication_id,
            "",
            proto::EventPublicationErrorCode::StateInvalid,
            None,
            "event publication commit wait timeout is invalid",
        ));
    }
    if duration.seconds == 0 && duration.nanos == 0 {
        return Ok(DEFAULT_COMMIT_WAIT_MS);
    }
    let seconds = u64::try_from(duration.seconds).map_err(|_| {
        publication_error(
            publication_id,
            "",
            proto::EventPublicationErrorCode::StateInvalid,
            None,
            "event publication commit wait timeout is invalid",
        )
    })?;
    let nanos = u64::try_from(duration.nanos).map_err(|_| {
        publication_error(
            publication_id,
            "",
            proto::EventPublicationErrorCode::StateInvalid,
            None,
            "event publication commit wait timeout is invalid",
        )
    })?;
    let wait_ms = seconds
        .checked_mul(1_000)
        .and_then(|milliseconds| milliseconds.checked_add(nanos.div_ceil(1_000_000)))
        .ok_or_else(|| {
            publication_error(
                publication_id,
                "",
                proto::EventPublicationErrorCode::StateInvalid,
                None,
                "event publication commit wait timeout is too large",
            )
        })?;
    Ok(wait_ms.min(PUBLICATION_TTL_MS))
}

fn validate_start(
    state: &ServerState,
    publication_id: &str,
    event: &proto::Event,
    attachment_channel: i32,
) -> Result<proto::DataChannelKind, ControlCommandError> {
    validate_event_identity(state, publication_id, event)?;
    let channel = proto::DataChannelKind::try_from(attachment_channel).map_err(|_| {
        publication_error(
            publication_id,
            &event.event_id,
            proto::EventPublicationErrorCode::AttachmentInvalid,
            None,
            "event attachment channel is invalid",
        )
    })?;
    if channel != proto::DataChannelKind::ReliableData {
        return Err(publication_error(
            publication_id,
            &event.event_id,
            proto::EventPublicationErrorCode::AttachmentInvalid,
            None,
            "event attachments require reliable data",
        ));
    }
    validate_event_values(publication_id, event)?;
    validate_attachments(publication_id, event)?;
    validate_revision(state, publication_id, event)?;
    Ok(channel)
}

fn validate_chunk(
    publication_id: &str,
    publication: &StagedPublication,
    channel: proto::DataChannelKind,
    chunk: &proto::EventAttachmentChunk,
    total_staged_bytes: u64,
) -> Result<(), ControlCommandError> {
    let descriptor = &publication.event.attachments[0];
    let expected_timestamp = descriptor.timestamp.as_ref();
    let chunk_bytes = u64::try_from(chunk.payload.len()).map_err(|_| {
        publication_error(
            publication_id,
            &chunk.event_id,
            proto::EventPublicationErrorCode::SizeLimitExceeded,
            None,
            "event attachment size is invalid",
        )
    })?;
    let metadata_matches = channel == publication.attachment_channel
        && chunk.event_id == publication.event.event_id
        && chunk.revision == publication.event.revision
        && chunk.attachment_id == descriptor.attachment_id
        && chunk.attachment_type == descriptor.attachment_type
        && chunk.content_type == descriptor.content_type
        && chunk.ordinal == descriptor.ordinal
        && chunk.timestamp.as_ref() == expected_timestamp
        && chunk.sequence == 1
        && chunk.chunk_count > 0
        && chunk.chunk_count <= MAXIMUM_ATTACHMENT_CHUNKS
        && publication
            .chunk_count
            .is_none_or(|count| count == chunk.chunk_count)
        && chunk.chunk_index == publication.next_chunk_index
        && chunk.chunk_index < chunk.chunk_count;
    let expected_bytes = descriptor.byte_len.unwrap_or_default();
    let publication_bytes = (publication.attachment_bytes.len() as u64).saturating_add(chunk_bytes);
    if !metadata_matches || chunk.payload.is_empty() || publication_bytes > expected_bytes {
        return Err(publication_error(
            publication_id,
            &chunk.event_id,
            proto::EventPublicationErrorCode::AttachmentInvalid,
            None,
            "event attachment chunk is invalid",
        ));
    }
    if total_staged_bytes.saturating_add(chunk_bytes) > MAXIMUM_STAGED_BYTES {
        return Err(publication_error(
            publication_id,
            &chunk.event_id,
            proto::EventPublicationErrorCode::SizeLimitExceeded,
            None,
            "event publication staging limit reached",
        ));
    }
    Ok(())
}

fn expire_publications(
    publications: &mut HashMap<(SessionId, String), StagedPublication>,
    now_ms: u64,
) -> u64 {
    let mut expired = 0_u64;
    for publication in publications.values_mut() {
        if matches!(
            publication.status,
            PublicationStatus::Accepting | PublicationStatus::Waiting
        ) && now_ms >= publication.expires_at_ms
        {
            publication.status = PublicationStatus::Expired;
            publication.expiry_notification_pending = true;
            publication.attachment_bytes.clear();
            publication.reserved_bytes = 0;
            expired = expired.saturating_add(1);
        }
    }
    expired
}

fn publication_state_error(
    publication_id: &str,
    publication: &StagedPublication,
) -> ControlCommandError {
    let (code, message) = match publication.status {
        PublicationStatus::Expired => (
            proto::EventPublicationErrorCode::Expired,
            "event publication expired",
        ),
        PublicationStatus::Committing
        | PublicationStatus::Committed
        | PublicationStatus::Aborted => (
            proto::EventPublicationErrorCode::StateInvalid,
            "event publication state does not allow this transition",
        ),
        PublicationStatus::Accepting | PublicationStatus::Waiting => (
            proto::EventPublicationErrorCode::StateInvalid,
            "event publication state is invalid",
        ),
    };
    publication_error(
        publication_id,
        &publication.event.event_id,
        code,
        None,
        message,
    )
}

fn timeline_event(event: &proto::Event) -> Result<TimelineEvent, ControlCommandError> {
    let stream = event_stream(event).map(str::to_owned);
    let attachments = event
        .attachments
        .iter()
        .map(|attachment| {
            Ok(EventAttachment {
                id: attachment.attachment_id.clone(),
                attachment_type: attachment.attachment_type.clone(),
                content_type: attachment.content_type.clone(),
                byte_len: attachment.byte_len,
                ordinal: attachment.ordinal,
                timestamp_ms: attachment
                    .timestamp
                    .as_ref()
                    .map(|timestamp| required_timestamp_ms(Some(timestamp), "attachment timestamp"))
                    .transpose()?,
                text: attachment.text.clone(),
            })
        })
        .collect::<Result<Vec<_>, ControlCommandError>>()?;
    let icon = crate::storage::metadata::event_icon(event.icon_key.as_deref(), &event.event_type);
    Ok(TimelineEvent {
        id: event.event_id.clone(),
        revision: event.revision,
        camera_id: event.source_id.clone(),
        stream,
        source: EventSource::KeepPeek,
        kind: event.event_type.clone(),
        start_time_ms: required_timestamp_ms(event.start_time.as_ref(), "event start time")?,
        end_time_ms: event
            .end_time
            .as_ref()
            .map(|timestamp| required_timestamp_ms(Some(timestamp), "event end time"))
            .transpose()?,
        confidence: event.confidence,
        bbox: event
            .bounding_box
            .as_ref()
            .map(|bbox| [bbox.x, bbox.y, bbox.width, bbox.height]),
        bbox_attachment_id: event.bounding_box_attachment_id.clone(),
        zone: event.zone.clone(),
        text: event.text.clone(),
        payload: json_payload(event.payload.as_ref()).map_err(|_| {
            publication_error(
                "",
                &event.event_id,
                proto::EventPublicationErrorCode::EventInvalid,
                None,
                "event publication payload is invalid",
            )
        })?,
        attachments,
        canonical_attachment_id: event.canonical_attachment_id.clone(),
        icon_key: icon.key.to_owned(),
        rejected_icon_key: icon.rejected,
        thumbnail_filename: None,
    })
}

fn event_stream(event: &proto::Event) -> Option<&str> {
    event
        .payload
        .as_ref()
        .and_then(|payload| payload.fields.get("stream_id"))
        .and_then(|value| value.kind.as_ref())
        .and_then(|kind| match kind {
            prost_types::value::Kind::StringValue(stream) => Some(stream.as_str()),
            _ => None,
        })
        .filter(|stream| matches!(*stream, "main" | "sub"))
}

fn validate_path_id(value: &str, name: &str) -> Result<(), ControlCommandError> {
    validate_client_id(value, name)?;
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            format!("{name} may contain only letters, digits, hyphens, and underscores"),
        ));
    }
    Ok(())
}

fn validate_event_identity(
    state: &ServerState,
    publication_id: &str,
    event: &proto::Event,
) -> Result<(), ControlCommandError> {
    if validate_path_id(&event.event_id, "event ID").is_err()
        || validate_client_id(&event.source_id, "event source ID").is_err()
        || event.subscription_id.is_some()
        || !PUBLISHED_DETECTION_EVENT_TYPES.contains(&event.event_type.as_str())
        || proto::MediaKind::try_from(event.media_kind.unwrap_or_default())
            != Ok(proto::MediaKind::Video)
    {
        return Err(publication_error(
            publication_id,
            &event.event_id,
            proto::EventPublicationErrorCode::EventInvalid,
            None,
            "event publication identity or type is invalid",
        ));
    }
    let source_session_id = event.source_session_id.as_deref().ok_or_else(|| {
        publication_error(
            publication_id,
            &event.event_id,
            proto::EventPublicationErrorCode::SourceNotFound,
            None,
            "event source session is missing",
        )
    })?;
    let camera = state
        .camera_entries()
        .into_iter()
        .find(|camera| camera.info.id == event.source_id)
        .ok_or_else(|| {
            publication_error(
                publication_id,
                &event.event_id,
                proto::EventPublicationErrorCode::SourceNotFound,
                None,
                "event source was not found",
            )
        })?;
    if proto_camera_source_session(&camera.info, &state.webrtc)
        .is_none_or(|source| source.source_session_id != source_session_id)
    {
        return Err(publication_error(
            publication_id,
            &event.event_id,
            proto::EventPublicationErrorCode::SourceNotFound,
            None,
            "event source session is not active",
        ));
    }
    Ok(())
}

fn validate_event_values(
    publication_id: &str,
    event: &proto::Event,
) -> Result<(), ControlCommandError> {
    let invalid_event = || {
        publication_error(
            publication_id,
            &event.event_id,
            proto::EventPublicationErrorCode::EventInvalid,
            None,
            "event publication values are invalid",
        )
    };
    let start_time_ms = required_timestamp_ms(event.start_time.as_ref(), "event start time")
        .map_err(|_| invalid_event())?;
    let end_time_ms = event
        .end_time
        .as_ref()
        .map(|timestamp| required_timestamp_ms(Some(timestamp), "event end time"))
        .transpose()
        .map_err(|_| invalid_event())?;
    let invalid_confidence = event
        .confidence
        .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value));
    let invalid_box = event.bounding_box.as_ref().is_some_and(|bbox| {
        [bbox.x, bbox.y, bbox.width, bbox.height]
            .into_iter()
            .any(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
            || bbox.x + bbox.width > 1.0
            || bbox.y + bbox.height > 1.0
    });
    if end_time_ms.is_some_and(|end| end < start_time_ms)
        || invalid_confidence
        || invalid_box
        || !text_and_payload_valid(event)
    {
        return Err(invalid_event());
    }
    Ok(())
}

pub(super) fn text_and_payload_valid(event: &proto::Event) -> bool {
    !event
        .text
        .as_ref()
        .is_some_and(|text| text.chars().count() > MAXIMUM_EVENT_TEXT_CHARS)
        && !event
            .payload
            .as_ref()
            .is_some_and(|payload| !valid_structured_payload(payload))
}

pub(super) fn json_payload(
    payload: Option<&prost_types::Struct>,
) -> anyhow::Result<Option<serde_json::Map<String, serde_json::Value>>> {
    payload
        .map(|payload| {
            payload
                .fields
                .iter()
                .map(|(key, value)| Ok((key.clone(), json_value(value)?)))
                .collect::<anyhow::Result<serde_json::Map<_, _>>>()
        })
        .transpose()
}

fn json_value(value: &prost_types::Value) -> anyhow::Result<serde_json::Value> {
    match value
        .kind
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("structured payload value has no kind"))?
    {
        prost_types::value::Kind::NullValue(_) => Ok(serde_json::Value::Null),
        prost_types::value::Kind::NumberValue(number) => serde_json::Number::from_f64(*number)
            .map(serde_json::Value::Number)
            .ok_or_else(|| anyhow::anyhow!("structured payload number is not finite")),
        prost_types::value::Kind::StringValue(value) => {
            Ok(serde_json::Value::String(value.clone()))
        }
        prost_types::value::Kind::BoolValue(value) => Ok(serde_json::Value::Bool(*value)),
        prost_types::value::Kind::StructValue(value) => {
            json_payload(Some(value)).and_then(|value| {
                value
                    .map(serde_json::Value::Object)
                    .ok_or_else(|| anyhow::anyhow!("structured payload object is missing"))
            })
        }
        prost_types::value::Kind::ListValue(value) => value
            .values
            .iter()
            .map(json_value)
            .collect::<Result<Vec<_>, _>>()
            .map(serde_json::Value::Array),
    }
}

fn valid_structured_payload(payload: &prost_types::Struct) -> bool {
    if payload.encoded_len() > MAXIMUM_EVENT_PAYLOAD_BYTES
        || payload.fields.len() > MAXIMUM_EVENT_PAYLOAD_COLLECTION_ITEMS
    {
        return false;
    }
    let mut nodes = 0;
    payload.fields.iter().all(|(key, value)| {
        !key.is_empty()
            && key.len() <= MAXIMUM_EVENT_PAYLOAD_KEY_BYTES
            && !key.chars().any(char::is_control)
            && valid_structured_value(value, 1, &mut nodes)
    })
}

fn valid_structured_value(value: &prost_types::Value, depth: usize, nodes: &mut usize) -> bool {
    *nodes = nodes.saturating_add(1);
    if depth > MAXIMUM_EVENT_PAYLOAD_DEPTH || *nodes > MAXIMUM_EVENT_PAYLOAD_NODES {
        return false;
    }
    match value.kind.as_ref() {
        Some(prost_types::value::Kind::NullValue(_))
        | Some(prost_types::value::Kind::BoolValue(_))
        | Some(prost_types::value::Kind::StringValue(_)) => true,
        Some(prost_types::value::Kind::NumberValue(number)) => number.is_finite(),
        Some(prost_types::value::Kind::StructValue(structure)) => {
            structure.fields.len() <= MAXIMUM_EVENT_PAYLOAD_COLLECTION_ITEMS
                && structure.fields.iter().all(|(key, value)| {
                    !key.is_empty()
                        && key.len() <= MAXIMUM_EVENT_PAYLOAD_KEY_BYTES
                        && !key.chars().any(char::is_control)
                        && valid_structured_value(value, depth.saturating_add(1), nodes)
                })
        }
        Some(prost_types::value::Kind::ListValue(list)) => {
            list.values.len() <= MAXIMUM_EVENT_PAYLOAD_COLLECTION_ITEMS
                && list
                    .values
                    .iter()
                    .all(|value| valid_structured_value(value, depth.saturating_add(1), nodes))
        }
        None => false,
    }
}

fn validate_attachments(
    publication_id: &str,
    event: &proto::Event,
) -> Result<(), ControlCommandError> {
    if event.attachments.len() != 1 {
        return Err(publication_error(
            publication_id,
            &event.event_id,
            proto::EventPublicationErrorCode::AttachmentCountMismatch,
            None,
            "person and vehicle events require exactly one snapshot",
        ));
    }
    let descriptor = &event.attachments[0];
    let bytes = descriptor.byte_len.unwrap_or_default();
    if bytes > MAXIMUM_ATTACHMENT_BYTES || bytes > MAXIMUM_EVENT_ATTACHMENT_BYTES {
        return Err(publication_error(
            publication_id,
            &event.event_id,
            proto::EventPublicationErrorCode::SizeLimitExceeded,
            None,
            "event snapshot exceeds publication byte limits",
        ));
    }
    let referenced = |candidate: &Option<String>| {
        candidate
            .as_deref()
            .is_none_or(|id| id == descriptor.attachment_id)
    };
    let invalid_timestamp = descriptor.timestamp.as_ref().is_some_and(|timestamp| {
        required_timestamp_ms(Some(timestamp), "attachment timestamp").is_err()
    });
    if validate_client_id(&descriptor.attachment_id, "event attachment ID").is_err()
        || descriptor.attachment_type != "snapshot"
        || descriptor.content_type != "image/jpeg"
        || descriptor.ordinal != 0
        || bytes == 0
        || descriptor
            .text
            .as_ref()
            .is_some_and(|text| text.chars().count() > MAXIMUM_ATTACHMENT_TEXT_CHARS)
        || invalid_timestamp
        || !referenced(&event.canonical_attachment_id)
        || !referenced(&event.bounding_box_attachment_id)
        || proto::EventImageAvailability::try_from(event.image_availability)
            != Ok(proto::EventImageAvailability::Available)
    {
        return Err(publication_error(
            publication_id,
            &event.event_id,
            proto::EventPublicationErrorCode::AttachmentInvalid,
            None,
            "event snapshot descriptor is invalid",
        ));
    }
    let mut ids = HashSet::with_capacity(event.attachments.len());
    if !ids.insert(descriptor.attachment_id.as_str()) {
        return Err(publication_error(
            publication_id,
            &event.event_id,
            proto::EventPublicationErrorCode::AttachmentInvalid,
            None,
            "event attachment IDs must be unique",
        ));
    }
    Ok(())
}

fn validate_revision(
    state: &ServerState,
    publication_id: &str,
    event: &proto::Event,
) -> Result<(), ControlCommandError> {
    let store = state.events.as_ref().ok_or_else(|| {
        publication_error(
            publication_id,
            &event.event_id,
            proto::EventPublicationErrorCode::StorageUnavailable,
            None,
            "event storage is unavailable",
        )
    })?;
    let existing = store.event_by_id(&event.event_id).map_err(|_| {
        publication_error(
            publication_id,
            &event.event_id,
            proto::EventPublicationErrorCode::StorageUnavailable,
            None,
            "event storage is unavailable",
        )
    })?;
    let current_revision = existing.as_ref().map(|stored| stored.revision);
    let valid = match existing {
        Some(stored) => {
            let same_external_source =
                event.source_id == stored.camera_id && stored.source == EventSource::KeepPeek;
            if event.revision > stored.revision {
                same_external_source
            } else if event.revision == stored.revision && same_external_source {
                store
                    .event_publication_identity(&event.event_id)
                    .map_err(|_| {
                        publication_error(
                            publication_id,
                            &event.event_id,
                            proto::EventPublicationErrorCode::StorageUnavailable,
                            current_revision,
                            "event storage is unavailable",
                        )
                    })?
                    .is_some_and(|identity| identity.publication_id == publication_id)
            } else {
                false
            }
        }
        None => event.revision == 1,
    };
    if !valid {
        return Err(publication_error(
            publication_id,
            &event.event_id,
            proto::EventPublicationErrorCode::RevisionConflict,
            current_revision,
            "event publication revision conflicts with durable state",
        ));
    }
    Ok(())
}

fn publication_state(
    publication_id: &str,
    publication: &StagedPublication,
) -> proto::EventPublicationState {
    proto::EventPublicationState {
        publication_id: publication_id.to_owned(),
        status: match publication.status {
            PublicationStatus::Accepting
            | PublicationStatus::Waiting
            | PublicationStatus::Committing => {
                proto::EventPublicationStatus::AcceptingAttachments as i32
            }
            PublicationStatus::Committed => proto::EventPublicationStatus::Committed as i32,
            PublicationStatus::Aborted => proto::EventPublicationStatus::Aborted as i32,
            PublicationStatus::Expired => proto::EventPublicationStatus::Expired as i32,
        },
        event_id: publication.event.event_id.clone(),
        revision: publication.event.revision,
        attachment_channel: publication.attachment_channel as i32,
        max_attachment_bytes: MAXIMUM_ATTACHMENT_BYTES,
        max_event_attachment_bytes: MAXIMUM_EVENT_ATTACHMENT_BYTES,
        expires_at: Some(millis_timestamp(
            i64::try_from(publication.expires_at_ms).unwrap_or(i64::MAX),
        )),
    }
}

fn publication_error(
    publication_id: &str,
    event_id: &str,
    code: proto::EventPublicationErrorCode,
    current_revision: Option<u64>,
    message: &'static str,
) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::Rejected, 409, message).with_detail(
        prost_types::Any {
            type_url: "type.googleapis.com/keeppeek.webrtc.v1.EventPublicationError".to_owned(),
            value: proto::EventPublicationError {
                publication_id: publication_id.to_owned(),
                event_id: event_id.to_owned(),
                code: code as i32,
                current_revision,
            }
            .encode_to_vec(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn staged_publication() -> StagedPublication {
        StagedPublication {
            event: proto::Event {
                event_id: "event-1".to_owned(),
                revision: 1,
                source_id: "front-door".to_owned(),
                attachments: vec![proto::EventAttachmentDescriptor {
                    attachment_id: "snapshot-1".to_owned(),
                    attachment_type: "snapshot".to_owned(),
                    content_type: "image/jpeg".to_owned(),
                    byte_len: Some(4),
                    ordinal: 0,
                    timestamp: Some(millis_timestamp(1_000)),
                    text: None,
                }],
                ..Default::default()
            },
            attachment_channel: proto::DataChannelKind::ReliableData,
            expires_at_ms: 100,
            status: PublicationStatus::Accepting,
            expiry_notification_pending: false,
            attachment_bytes: Vec::new(),
            reserved_bytes: 0,
            chunk_count: None,
            next_chunk_index: 0,
            started_at: Instant::now(),
        }
    }

    fn attachment_chunk(chunk_index: u32, chunk_count: u32) -> proto::EventAttachmentChunk {
        proto::EventAttachmentChunk {
            context: Some(proto::event_attachment_chunk::Context::PublicationId(
                "publication-1".to_owned(),
            )),
            event_id: "event-1".to_owned(),
            revision: 1,
            attachment_id: "snapshot-1".to_owned(),
            attachment_type: "snapshot".to_owned(),
            content_type: "image/jpeg".to_owned(),
            ordinal: 0,
            timestamp: Some(millis_timestamp(1_000)),
            sequence: 1,
            chunk_index,
            chunk_count,
            payload: vec![1, 2],
        }
    }

    fn publication_code(error: &ControlCommandError) -> proto::EventPublicationErrorCode {
        let detail = proto::EventPublicationError::decode(error.details[0].value.as_slice())
            .expect("publication errors must carry typed detail");
        proto::EventPublicationErrorCode::try_from(detail.code).unwrap()
    }

    #[test]
    fn commit_wait_timeout_defaults_rounds_and_caps() {
        assert_eq!(
            commit_wait_timeout_ms("publication-1", None).unwrap(),
            1_000
        );
        assert_eq!(
            commit_wait_timeout_ms(
                "publication-1",
                Some(&prost_types::Duration {
                    seconds: 0,
                    nanos: 1,
                }),
            )
            .unwrap(),
            1
        );
        assert_eq!(
            commit_wait_timeout_ms(
                "publication-1",
                Some(&prost_types::Duration {
                    seconds: 60,
                    nanos: 0,
                }),
            )
            .unwrap(),
            PUBLICATION_TTL_MS
        );
        let error = commit_wait_timeout_ms(
            "publication-1",
            Some(&prost_types::Duration {
                seconds: -1,
                nanos: 0,
            }),
        )
        .unwrap_err();
        assert_eq!(
            publication_code(&error),
            proto::EventPublicationErrorCode::StateInvalid
        );
    }

    #[test]
    fn attachment_descriptor_rejections_are_typed_at_start() {
        let mut event = staged_publication().event;

        let mut missing = event.clone();
        missing.attachments.clear();
        let error = validate_attachments("publication-1", &missing).unwrap_err();
        assert_eq!(
            publication_code(&error),
            proto::EventPublicationErrorCode::AttachmentCountMismatch
        );

        event.attachments[0].byte_len = Some(MAXIMUM_ATTACHMENT_BYTES + 1);
        let error = validate_attachments("publication-1", &event).unwrap_err();
        assert_eq!(
            publication_code(&error),
            proto::EventPublicationErrorCode::SizeLimitExceeded
        );

        event.attachments[0].byte_len = Some(4);
        event.attachments[0].text = Some("x".repeat(4_097));
        let error = validate_attachments("publication-1", &event).unwrap_err();
        assert_eq!(
            publication_code(&error),
            proto::EventPublicationErrorCode::AttachmentInvalid
        );

        event.attachments[0].text = None;
        event.attachments[0].timestamp = Some(prost_types::Timestamp {
            seconds: 1,
            nanos: 1_000_000_000,
        });
        let error = validate_attachments("publication-1", &event).unwrap_err();
        assert_eq!(
            publication_code(&error),
            proto::EventPublicationErrorCode::AttachmentInvalid
        );
    }

    #[test]
    fn event_text_and_structured_payload_are_bounded() {
        let mut event = staged_publication().event;
        event.start_time = Some(millis_timestamp(1_000));

        event.text = Some("x".repeat(MAXIMUM_EVENT_TEXT_CHARS + 1));
        let error = validate_event_values("publication-1", &event).unwrap_err();
        assert_eq!(
            publication_code(&error),
            proto::EventPublicationErrorCode::EventInvalid
        );

        event.text = None;
        event.payload = Some(prost_types::Struct {
            fields: std::collections::BTreeMap::from([(
                "confidence".to_owned(),
                prost_types::Value {
                    kind: Some(prost_types::value::Kind::NumberValue(f64::NAN)),
                },
            )]),
        });
        let error = validate_event_values("publication-1", &event).unwrap_err();
        assert_eq!(
            publication_code(&error),
            proto::EventPublicationErrorCode::EventInvalid
        );

        event.payload = Some(prost_types::Struct {
            fields: std::collections::BTreeMap::from([(
                "description".to_owned(),
                prost_types::Value {
                    kind: Some(prost_types::value::Kind::StringValue(
                        "x".repeat(MAXIMUM_EVENT_PAYLOAD_BYTES),
                    )),
                },
            )]),
        });
        let error = validate_event_values("publication-1", &event).unwrap_err();
        assert_eq!(
            publication_code(&error),
            proto::EventPublicationErrorCode::EventInvalid
        );

        let mut nested = prost_types::Value {
            kind: Some(prost_types::value::Kind::StringValue("leaf".to_owned())),
        };
        for _ in 0..MAXIMUM_EVENT_PAYLOAD_DEPTH {
            nested = prost_types::Value {
                kind: Some(prost_types::value::Kind::StructValue(prost_types::Struct {
                    fields: std::collections::BTreeMap::from([("nested".to_owned(), nested)]),
                })),
            };
        }
        event.payload = Some(prost_types::Struct {
            fields: std::collections::BTreeMap::from([("nested".to_owned(), nested)]),
        });
        let error = validate_event_values("publication-1", &event).unwrap_err();
        assert_eq!(
            publication_code(&error),
            proto::EventPublicationErrorCode::EventInvalid
        );

        let inner = prost_types::Value {
            kind: Some(prost_types::value::Kind::ListValue(
                prost_types::ListValue {
                    values: vec![prost_types::Value::default(); 4],
                },
            )),
        };
        event.payload = Some(prost_types::Struct {
            fields: std::collections::BTreeMap::from([(
                "nodes".to_owned(),
                prost_types::Value {
                    kind: Some(prost_types::value::Kind::ListValue(
                        prost_types::ListValue {
                            values: vec![inner; MAXIMUM_EVENT_PAYLOAD_COLLECTION_ITEMS],
                        },
                    )),
                },
            )]),
        });
        let error = validate_event_values("publication-1", &event).unwrap_err();
        assert_eq!(
            publication_code(&error),
            proto::EventPublicationErrorCode::EventInvalid
        );
    }

    #[test]
    fn chunk_validation_rejects_unbound_metadata_order_and_limits() {
        let publication = staged_publication();
        validate_chunk(
            "publication-1",
            &publication,
            proto::DataChannelKind::ReliableData,
            &attachment_chunk(0, 2),
            0,
        )
        .unwrap();

        let mut cases = Vec::new();
        cases.push((
            proto::DataChannelKind::UnreliableData,
            attachment_chunk(0, 2),
            0,
            proto::EventPublicationErrorCode::AttachmentInvalid,
        ));
        let mut inconsistent = attachment_chunk(0, 2);
        inconsistent.content_type = "image/png".to_owned();
        cases.push((
            proto::DataChannelKind::ReliableData,
            inconsistent,
            0,
            proto::EventPublicationErrorCode::AttachmentInvalid,
        ));
        cases.push((
            proto::DataChannelKind::ReliableData,
            attachment_chunk(1, 2),
            0,
            proto::EventPublicationErrorCode::AttachmentInvalid,
        ));
        cases.push((
            proto::DataChannelKind::ReliableData,
            attachment_chunk(0, MAXIMUM_ATTACHMENT_CHUNKS + 1),
            0,
            proto::EventPublicationErrorCode::AttachmentInvalid,
        ));
        cases.push((
            proto::DataChannelKind::ReliableData,
            attachment_chunk(0, 2),
            MAXIMUM_STAGED_BYTES,
            proto::EventPublicationErrorCode::SizeLimitExceeded,
        ));
        for (channel, chunk, staged_bytes, expected) in cases {
            let error =
                validate_chunk("publication-1", &publication, channel, &chunk, staged_bytes)
                    .unwrap_err();
            assert_eq!(publication_code(&error), expected);
        }

        let mut after_first = staged_publication();
        after_first.attachment_bytes.extend_from_slice(&[1, 2]);
        after_first.reserved_bytes = 2;
        after_first.chunk_count = Some(2);
        after_first.next_chunk_index = 1;
        for chunk_index in [0, 2] {
            let error = validate_chunk(
                "publication-1",
                &after_first,
                proto::DataChannelKind::ReliableData,
                &attachment_chunk(chunk_index, 2),
                2,
            )
            .unwrap_err();
            assert_eq!(
                publication_code(&error),
                proto::EventPublicationErrorCode::AttachmentInvalid
            );
        }
        validate_chunk(
            "publication-1",
            &after_first,
            proto::DataChannelKind::ReliableData,
            &attachment_chunk(1, 2),
            2,
        )
        .unwrap();
    }

    #[test]
    fn expiry_abort_and_session_close_release_staged_bytes() {
        let session_id = SessionId::from_u64(7);
        let key = (session_id, "publication-1".to_owned());
        let registry = Registry::default();
        registry
            .inner
            .publications
            .lock()
            .unwrap()
            .insert(key.clone(), staged_publication());

        let aborted = registry
            .abort(
                session_id,
                proto::AbortEventPublication {
                    publication_id: "publication-1".to_owned(),
                },
                50,
            )
            .unwrap();
        assert_eq!(
            aborted.status,
            proto::EventPublicationStatus::Aborted as i32
        );
        let retried = registry
            .abort(
                session_id,
                proto::AbortEventPublication {
                    publication_id: "publication-1".to_owned(),
                },
                50,
            )
            .unwrap();
        assert_eq!(retried.status, aborted.status);
        assert!(
            registry
                .inner
                .publications
                .lock()
                .unwrap()
                .get(&key)
                .unwrap()
                .attachment_bytes
                .is_empty()
        );

        registry
            .inner
            .publications
            .lock()
            .unwrap()
            .insert(key.clone(), staged_publication());
        let expired = registry.expire(100);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].0, session_id);
        assert_eq!(
            expired[0].1.status,
            proto::EventPublicationStatus::Expired as i32
        );
        assert!(registry.expire(100).is_empty());
        let metrics = registry.metrics_snapshot();
        assert_eq!(metrics.active, 0);
        assert_eq!(metrics.staged_bytes, 0);
        assert_eq!(metrics.aborts, 1);
        assert_eq!(metrics.expirations, 1);
        let publications = registry.inner.publications.lock().unwrap();
        assert!(publications.get(&key).unwrap().attachment_bytes.is_empty());
        drop(publications);
        registry.close_session(session_id);
        assert!(
            !registry
                .inner
                .publications
                .lock()
                .unwrap()
                .contains_key(&key)
        );
    }

    #[test]
    fn source_invalidation_aborts_staged_publication_bytes() {
        let registry = Registry::default();
        let key = (SessionId::from_u64(7), "publication-1".to_owned());
        let mut publication = staged_publication();
        publication.attachment_bytes = vec![1, 2];
        publication.reserved_bytes = 2;
        registry
            .inner
            .publications
            .lock()
            .unwrap()
            .insert(key.clone(), publication);

        registry.invalidate_source("front-door");

        let publications = registry.inner.publications.lock().unwrap();
        let invalidated = publications.get(&key).unwrap();
        assert_eq!(invalidated.status, PublicationStatus::Aborted);
        assert!(invalidated.attachment_bytes.is_empty());
        assert_eq!(invalidated.reserved_bytes, 0);
        drop(publications);
        assert_eq!(registry.metrics_snapshot().aborts, 1);
    }

    #[test]
    fn source_invalidation_waits_for_the_durable_commit_boundary() {
        let registry = Registry::default();
        let key = (SessionId::from_u64(7), "publication-1".to_owned());
        let mut publication = staged_publication();
        publication.status = PublicationStatus::Committing;
        registry
            .inner
            .publications
            .lock()
            .unwrap()
            .insert(key.clone(), publication);
        let commit_guard = registry.inner.commit.lock().unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let worker_registry = registry.clone();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            worker_registry.invalidate_source("front-door");
            finished_tx.send(()).unwrap();
        });
        started_rx.recv().unwrap();

        assert_eq!(
            finished_rx.recv_timeout(Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        );
        drop(commit_guard);
        finished_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        worker.join().unwrap();

        let publications = registry.inner.publications.lock().unwrap();
        assert_eq!(
            publications.get(&key).unwrap().status,
            PublicationStatus::Aborted
        );
    }

    #[test]
    fn commit_latency_metrics_retain_a_bounded_recent_window() {
        let metrics = PublicationMetrics::default();
        for latency_ms in 1..=300 {
            metrics.record_commit(Duration::from_millis(latency_ms));
        }

        let samples = metrics.commit_latencies_ms.lock().unwrap();

        assert_eq!(metrics.commits.load(Ordering::Relaxed), 300);
        assert_eq!(samples.len(), MAXIMUM_COMMIT_LATENCY_SAMPLES);
        assert_eq!(latency_percentile(&samples, 50), 172);
        assert_eq!(latency_percentile(&samples, 95), 288);
    }

    #[test]
    fn publication_id_tombstones_are_bounded_per_session_and_globally() {
        let mut publications = HashMap::new();
        for index in 0..MAXIMUM_PUBLICATION_IDS_PER_SESSION {
            let mut publication = staged_publication();
            publication.status = PublicationStatus::Aborted;
            publications.insert(
                (SessionId::from_u64(7), format!("publication-{index}")),
                publication,
            );
        }

        assert!(publication_limit_reached(
            &publications,
            SessionId::from_u64(7)
        ));
        assert!(!publication_limit_reached(
            &publications,
            SessionId::from_u64(8)
        ));

        for index in publications.len()..MAXIMUM_PUBLICATION_IDS {
            let mut publication = staged_publication();
            publication.status = PublicationStatus::Committed;
            publications.insert(
                (
                    SessionId::from_u64(8 + index as u64),
                    format!("publication-{index}"),
                ),
                publication,
            );
        }
        assert!(publication_limit_reached(
            &publications,
            SessionId::from_u64(u64::MAX)
        ));
    }
}
