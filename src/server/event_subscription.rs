use super::{
    ControlCommandError, PUBLISHED_DETECTION_EVENT_TYPES, ServerState, millis_timestamp,
    proto_camera_source_session, validate_client_id,
};
use crate::{
    access::CameraAccess,
    api::proto,
    webrtc::{DataChannelTarget, EventDeliveryGuard, SessionId},
};
use prost::Message as _;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

const MAXIMUM_EVENT_SUBSCRIPTIONS: usize = 256;
const MAXIMUM_EVENT_SUBSCRIPTIONS_PER_SESSION: usize = 16;
const MAXIMUM_SOURCE_FILTERS: usize = 64;
const MAXIMUM_EVENT_TYPE_FILTERS: usize = 16;
const MAXIMUM_ATTACHMENT_ROUTES: usize = 8;

#[derive(Clone, Default)]
pub(super) struct Registry {
    inner: Arc<Mutex<HashMap<(SessionId, String), Subscription>>>,
    native: Arc<Mutex<NativeCapabilities>>,
    starts: Arc<AtomicU64>,
    rejections: Arc<AtomicU64>,
    deliveries: Arc<AtomicU64>,
    sheds: Arc<AtomicU64>,
}

#[derive(Default)]
struct NativeCapabilities {
    revision: u64,
    known: HashMap<SessionId, KnownCapabilities>,
}

struct KnownCapabilities {
    revision: u64,
    event_types: HashMap<String, HashSet<String>>,
}

impl NativeCapabilities {
    fn remember(&mut self, session_id: SessionId, snapshot: &proto::ServerCapabilities) {
        if snapshot.encoded_len() > crate::webrtc::MAX_CONTROL_MESSAGE_BYTES {
            self.known.remove(&session_id);
            return;
        }
        if self.known.len() >= MAXIMUM_EVENT_SUBSCRIPTIONS && !self.known.contains_key(&session_id)
        {
            return;
        }
        let event_types = snapshot
            .source_sessions
            .iter()
            .filter(|source| !source.source_id.is_empty())
            .map(|source| {
                (
                    source.source_session_id.clone(),
                    source
                        .event_types
                        .iter()
                        .map(|kind| kind.event_type.clone())
                        .collect(),
                )
            })
            .collect();
        self.known.insert(
            session_id,
            KnownCapabilities {
                revision: self.revision,
                event_types,
            },
        );
    }

    fn has_snapshot(&self, session_id: SessionId, event: &proto::Event, live: bool) -> bool {
        self.known.get(&session_id).is_some_and(|known| {
            known.revision == self.revision
                && (!live
                    || event
                        .source_session_id
                        .as_ref()
                        .and_then(|source| known.event_types.get(source))
                        .is_some_and(|kinds| kinds.contains(&event.event_type)))
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct MetricsSnapshot {
    pub(super) active: u64,
    pub(super) starts: u64,
    pub(super) rejections: u64,
    pub(super) deliveries: u64,
    pub(super) sheds: u64,
}

#[derive(Clone, Debug)]
struct Subscription {
    source_ids: HashSet<String>,
    allowed_source_ids: Option<HashSet<String>>,
    event_types: HashSet<String>,
    media_kinds: HashSet<proto::MediaKind>,
    attachment_routes: Vec<proto::EventAttachmentRoute>,
    guard: EventDeliveryGuard,
}

pub(super) struct Delivery {
    pub(super) session_id: SessionId,
    pub(super) subscription_id: String,
    pub(super) attachment_target: Option<DataChannelTarget>,
    pub(super) guard: EventDeliveryGuard,
}

impl Registry {
    pub(super) fn capabilities(
        &self,
        session_id: SessionId,
        snapshot: impl FnOnce() -> proto::ServerCapabilities,
    ) -> proto::ServerCapabilities {
        let mut native = self
            .native
            .lock()
            .expect("native capability revisions are not poisoned");
        let snapshot = snapshot();
        native.remember(session_id, &snapshot);
        snapshot
    }

    pub(super) fn subscribe(
        &self,
        state: &ServerState,
        session_id: SessionId,
        request: proto::SubscribeEvents,
    ) -> Result<proto::SubscriptionResult, ControlCommandError> {
        let result = super::camera_access::for_session(state, session_id).and_then(|policy| {
            self.subscribe_scoped_with_clock(
                state,
                session_id,
                request,
                &policy,
                super::unix_time_ms,
            )
        });
        if result.is_err() {
            self.rejections.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    fn subscribe_scoped_with_clock(
        &self,
        state: &ServerState,
        session_id: SessionId,
        request: proto::SubscribeEvents,
        policy: &CameraAccess,
        clock: impl FnOnce() -> u64,
    ) -> Result<proto::SubscriptionResult, ControlCommandError> {
        validate_client_id(&request.subscription_id, "event subscription ID")?;
        let subscription = scoped_subscription(state, &request, policy)?;
        let key = (session_id, request.subscription_id.clone());
        let mut subscriptions = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let is_new = !subscriptions.contains_key(&key);
        if is_new {
            let session_count = subscriptions
                .keys()
                .filter(|(owner, _)| *owner == session_id)
                .count();
            if subscriptions.len() >= MAXIMUM_EVENT_SUBSCRIPTIONS
                || session_count >= MAXIMUM_EVENT_SUBSCRIPTIONS_PER_SESSION
            {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::Rejected,
                    429,
                    "event subscription limit reached",
                ));
            }
        }
        let attachment_routes = subscription.attachment_routes.clone();
        if let Some(replaced) = subscriptions.insert(key, subscription) {
            replaced.guard.cancel();
        }
        drop(subscriptions);
        if is_new {
            self.starts.fetch_add(1, Ordering::Relaxed);
        }
        let backfill_end_ms = clock();
        Ok(proto::SubscriptionResult {
            subscription_id: request.subscription_id,
            delivery: Some(proto::subscription_result::Delivery::Events(
                proto::EventSubscriptionDelivery {
                    attachment_routes,
                    backfill_end_time: Some(millis_timestamp(
                        i64::try_from(backfill_end_ms).unwrap_or(i64::MAX),
                    )),
                },
            )),
            selected_variant_id: String::new(),
            selected_lineage: Vec::new(),
        })
    }

    pub(super) fn unsubscribe(&self, session_id: SessionId, subscription_ids: &[String]) {
        let subscription_ids = subscription_ids.iter().collect::<HashSet<_>>();
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(owner, subscription_id), subscription| {
                let remove = *owner == session_id && subscription_ids.contains(subscription_id);
                if remove {
                    subscription.guard.cancel();
                }
                !remove
            });
    }

    pub(super) fn close_session(&self, session_id: SessionId) {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(owner, _), subscription| {
                let remove = *owner == session_id;
                if remove {
                    subscription.guard.cancel();
                }
                !remove
            });
        self.native
            .lock()
            .expect("native capability revisions are not poisoned")
            .known
            .remove(&session_id);
    }

    pub(super) fn invalidate_source(&self, source_id: &str) -> Vec<SessionId> {
        let mut affected_sessions = HashSet::new();
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(session_id, _), subscription| {
                let remove = subscription.source_ids.contains(source_id);
                if remove {
                    subscription.guard.cancel();
                    affected_sessions.insert(*session_id);
                }
                !remove
            });
        let mut affected_sessions = affected_sessions.into_iter().collect::<Vec<_>>();
        affected_sessions.sort_unstable_by_key(|session_id| session_id.as_u64());
        affected_sessions
    }

    pub(super) fn deliveries(&self, event: &proto::Event) -> Vec<Delivery> {
        let subscriptions = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let event_media_kind = event
            .media_kind
            .and_then(|kind| proto::MediaKind::try_from(kind).ok());
        let canonical_attachment = event.canonical_attachment_id.as_deref().and_then(|id| {
            event
                .attachments
                .iter()
                .find(|attachment| attachment.attachment_id == id)
        });
        let mut deliveries = subscriptions
            .iter()
            .filter(|(_, subscription)| {
                subscription
                    .allowed_source_ids
                    .as_ref()
                    .is_none_or(|allowed| allowed.contains(&event.source_id))
                    && (subscription.source_ids.is_empty()
                        || subscription.source_ids.contains(&event.source_id))
                    && (subscription.event_types.is_empty()
                        || subscription.event_types.contains(&event.event_type))
                    && (subscription.media_kinds.is_empty()
                        || event_media_kind
                            .is_some_and(|kind| subscription.media_kinds.contains(&kind)))
            })
            .map(|((session_id, subscription_id), subscription)| {
                let attachment_target = canonical_attachment.and_then(|attachment| {
                    subscription
                        .attachment_routes
                        .iter()
                        .find(|route| {
                            route.attachment_type == attachment.attachment_type
                                && route.content_type == attachment.content_type
                        })
                        .and_then(
                            |route| match proto::DataChannelKind::try_from(route.channel) {
                                Ok(proto::DataChannelKind::ReliableData) => {
                                    Some(DataChannelTarget::Reliable)
                                }
                                Ok(proto::DataChannelKind::UnreliableData) => {
                                    Some(DataChannelTarget::Unreliable)
                                }
                                Ok(proto::DataChannelKind::Unspecified) | Err(_) => None,
                            },
                        )
                });
                Delivery {
                    session_id: *session_id,
                    subscription_id: subscription_id.clone(),
                    attachment_target,
                    guard: subscription.guard.clone(),
                }
            })
            .collect::<Vec<_>>();
        deliveries.sort_unstable_by(|left, right| {
            left.session_id
                .as_u64()
                .cmp(&right.session_id.as_u64())
                .then(left.subscription_id.cmp(&right.subscription_id))
        });
        self.deliveries
            .fetch_add(deliveries.len() as u64, Ordering::Relaxed);
        deliveries
    }

    pub(super) fn shed(&self, session_id: SessionId, subscription_id: &str) {
        let removed = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&(session_id, subscription_id.to_owned()));
        if let Some(removed) = removed {
            removed.guard.cancel();
            self.sheds.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(super) fn contains(&self, session_id: SessionId, subscription_id: &str) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&(session_id, subscription_id.to_owned()))
    }

    pub(super) fn metrics_snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            active: self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len() as u64,
            starts: self.starts.load(Ordering::Relaxed),
            rejections: self.rejections.load(Ordering::Relaxed),
            deliveries: self.deliveries.load(Ordering::Relaxed),
            sheds: self.sheds.load(Ordering::Relaxed),
        }
    }

    #[cfg(test)]
    fn subscribe_with_clock(
        &self,
        state: &ServerState,
        session_id: SessionId,
        request: proto::SubscribeEvents,
        clock: impl FnOnce() -> u64,
    ) -> Result<proto::SubscriptionResult, ControlCommandError> {
        self.subscribe_scoped_with_clock(
            state,
            session_id,
            request,
            &CameraAccess::unrestricted(),
            clock,
        )
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }
}

fn scoped_subscription(
    state: &ServerState,
    request: &proto::SubscribeEvents,
    policy: &CameraAccess,
) -> Result<Subscription, ControlCommandError> {
    super::camera_access::require_cameras(policy, &request.source_ids)?;
    let mut subscription = validate_subscription(state, request)?;
    subscription.allowed_source_ids =
        (!policy.all_cameras).then(|| policy.camera_ids.iter().cloned().collect());
    Ok(subscription)
}

fn validate_subscription(
    state: &ServerState,
    request: &proto::SubscribeEvents,
) -> Result<Subscription, ControlCommandError> {
    let source_ids = bounded_unique(
        &request.source_ids,
        MAXIMUM_SOURCE_FILTERS,
        "event subscription sources are invalid",
    )?;
    if !source_ids.is_empty() {
        let available = state
            .camera_entries()
            .into_iter()
            .filter(|camera| proto_camera_source_session(&camera.info, &state.webrtc).is_some())
            .map(|camera| camera.info.id)
            .collect::<HashSet<_>>();
        if source_ids
            .iter()
            .any(|source_id| !available.contains(source_id))
        {
            return Err(subscription_error(
                &request.subscription_id,
                proto::SubscriptionErrorCode::SourceNotFound,
                "event subscription source is unavailable",
            ));
        }
    }
    let event_types = bounded_unique(
        &request.event_types,
        MAXIMUM_EVENT_TYPE_FILTERS,
        "event subscription event types are invalid",
    )?;
    let mut available_types = PUBLISHED_DETECTION_EVENT_TYPES
        .iter()
        .map(|kind| (*kind).to_owned())
        .collect::<HashSet<_>>();
    for camera in state.camera_entries() {
        if source_ids.is_empty() || source_ids.contains(&camera.info.id) {
            available_types.extend(
                super::native_events::reported_types(&camera.info, &state.health.events)
                    .into_iter()
                    .map(|kind| kind.event_type),
            );
        }
    }
    if event_types
        .iter()
        .any(|event_type| !available_types.contains(event_type))
    {
        return Err(subscription_error(
            &request.subscription_id,
            proto::SubscriptionErrorCode::EventTypeUnavailable,
            "event subscription type is unavailable",
        ));
    }
    let mut media_kinds = HashSet::with_capacity(request.media_kinds.len());
    for value in &request.media_kinds {
        let kind = proto::MediaKind::try_from(*value).map_err(|_| {
            subscription_error(
                &request.subscription_id,
                proto::SubscriptionErrorCode::MediaNotFound,
                "event subscription media kind is unavailable",
            )
        })?;
        if kind != proto::MediaKind::Video || !media_kinds.insert(kind) {
            return Err(subscription_error(
                &request.subscription_id,
                proto::SubscriptionErrorCode::MediaNotFound,
                "event subscription media kind is unavailable",
            ));
        }
    }
    if request.attachment_routes.len() > MAXIMUM_ATTACHMENT_ROUTES {
        return Err(subscription_error(
            &request.subscription_id,
            proto::SubscriptionErrorCode::EventAttachmentUnavailable,
            "event subscription has too many attachment routes",
        ));
    }
    let mut route_keys = HashSet::with_capacity(request.attachment_routes.len());
    for route in &request.attachment_routes {
        if route.attachment_type != "snapshot"
            || route.content_type != "image/jpeg"
            || !route_keys.insert((route.attachment_type.as_str(), route.content_type.as_str()))
        {
            return Err(subscription_error(
                &request.subscription_id,
                proto::SubscriptionErrorCode::EventAttachmentUnavailable,
                "event subscription attachment route is unavailable",
            ));
        }
        if !matches!(
            proto::DataChannelKind::try_from(route.channel),
            Ok(proto::DataChannelKind::ReliableData | proto::DataChannelKind::UnreliableData)
        ) {
            return Err(subscription_error(
                &request.subscription_id,
                proto::SubscriptionErrorCode::DeliveryTransportUnavailable,
                "event subscription attachment channel is unavailable",
            ));
        }
    }
    Ok(Subscription {
        source_ids,
        allowed_source_ids: None,
        event_types,
        media_kinds,
        attachment_routes: request.attachment_routes.clone(),
        guard: EventDeliveryGuard::default(),
    })
}

pub(super) fn publish(state: &ServerState, event: &proto::Event, image: Option<Arc<[u8]>>) {
    publish_images(state, event, image, &[]);
}

pub(super) fn publish_images(
    state: &ServerState,
    event: &proto::Event,
    image: Option<Arc<[u8]>>,
    additional: &[(String, Arc<[u8]>)],
) {
    for delivery in state.event_subscriptions.deliveries(event) {
        publish_delivery(state, event, image.as_ref(), additional, delivery);
    }
}

pub(super) fn publish_native_images(
    state: &ServerState,
    camera: &super::CameraInfo,
    event: &proto::Event,
    image: Option<Arc<[u8]>>,
    additional: &[(String, Arc<[u8]>)],
) {
    let Ok(ip) = camera.ip.parse() else {
        return;
    };
    if !state.health.events.enabled(ip) {
        return;
    }
    let mut native = state
        .event_subscriptions
        .native
        .lock()
        .expect("native capability revisions are not poisoned");
    if state.health.events.record_kind(ip, &event.event_type) {
        native.revision = native
            .revision
            .checked_add(1)
            .expect("native capability revision exhausted");
    }
    if !super::native_events::reported_types(camera, &state.health.events)
        .iter()
        .any(|kind| kind.event_type == event.event_type)
    {
        return;
    }
    let live = !state.webrtc.live_video_sources(ip).is_empty()
        && event.source_session_id.as_deref()
            == Some(
                super::camera_source_session_id(&camera.id, state.webrtc.camera_generation(ip))
                    .as_str(),
            );
    for delivery in state.event_subscriptions.deliveries(event) {
        let Some(policy) = native_delivery_access(state, event, &delivery) else {
            continue;
        };
        let mut snapshot = None;
        if !native.has_snapshot(delivery.session_id, event, live) {
            let Ok(cameras) = super::camera_access::query_cameras(state, &policy, &[]) else {
                state
                    .event_subscriptions
                    .shed(delivery.session_id, &delivery.subscription_id);
                continue;
            };
            let snapshot = snapshot.insert(super::server_capabilities(state, &cameras));
            if !queue_capabilities(state, delivery.session_id, snapshot) {
                state
                    .event_subscriptions
                    .shed(delivery.session_id, &delivery.subscription_id);
                continue;
            }
            native.remember(delivery.session_id, snapshot);
        }
        if live
            && snapshot
                .as_ref()
                .is_none_or(|snapshot| snapshot_has_event(snapshot, event))
        {
            publish_delivery(state, event, image.as_ref(), additional, delivery);
        }
    }
}

fn native_delivery_access(
    state: &ServerState,
    event: &proto::Event,
    delivery: &Delivery,
) -> Option<crate::access::CameraAccess> {
    if !delivery.guard.is_active() {
        return None;
    }
    let policy = super::camera_access::for_session(state, delivery.session_id)
        .ok()
        .filter(|policy| policy.allows(&event.source_id));
    if policy.is_none() {
        state
            .event_subscriptions
            .shed(delivery.session_id, &delivery.subscription_id);
    }
    policy
}

fn snapshot_has_event(snapshot: &proto::ServerCapabilities, event: &proto::Event) -> bool {
    snapshot.source_sessions.iter().any(|source| {
        event.source_session_id.as_deref() == Some(source.source_session_id.as_str())
            && source.source_id == event.source_id
            && source
                .event_types
                .iter()
                .any(|kind| kind.event_type == event.event_type)
    })
}

fn queue_capabilities(
    state: &ServerState,
    session_id: SessionId,
    snapshot: &proto::ServerCapabilities,
) -> bool {
    if snapshot.encoded_len() > crate::webrtc::MAX_CONTROL_MESSAGE_BYTES {
        return false;
    }
    let access_session = state
        .api_session_owners
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&session_id)
        .map(|session| super::proto_access_session(session_id, session));
    let notification = proto::Notification {
        event: Some(proto::notification::Event::InitialCapabilities(
            super::connection_capabilities(snapshot.clone(), session_id, access_session),
        )),
    };
    crate::webrtc::control_notification_encoded_len(&notification)
        <= crate::webrtc::MAX_CONTROL_MESSAGE_BYTES
        && matches!(
            state
                .webrtc
                .try_enqueue_api_notification(session_id, notification),
            Ok(true)
        )
}

fn publish_delivery(
    state: &ServerState,
    event: &proto::Event,
    image: Option<&Arc<[u8]>>,
    additional: &[(String, Arc<[u8]>)],
    delivery: Delivery,
) {
    let mut delivered_event = event.clone();
    delivered_event.subscription_id = Some(delivery.subscription_id.clone());
    let attachment_bytes = delivery
        .attachment_target
        .and_then(|_| image.map(Arc::clone));
    let queued = state.webrtc.try_enqueue_api_event(
        delivery.session_id,
        crate::webrtc::OutboundEventDelivery {
            event: delivered_event,
            attachment_target: delivery
                .attachment_target
                .filter(|_| image.is_some() || !additional.is_empty()),
            attachment_bytes,
            additional_attachments: if delivery.attachment_target.is_some() {
                additional.to_vec()
            } else {
                Vec::new()
            },
            guard: delivery.guard,
        },
    );
    if !matches!(queued, Ok(true)) {
        state
            .event_subscriptions
            .shed(delivery.session_id, &delivery.subscription_id);
        tracing::warn!(session_id = %delivery.session_id, subscription_id = %delivery.subscription_id,
            "shed event subscription after its delivery queue stopped accepting work");
    }
}

fn bounded_unique(
    values: &[String],
    maximum: usize,
    message: &'static str,
) -> Result<HashSet<String>, ControlCommandError> {
    let unique = values.iter().cloned().collect::<HashSet<_>>();
    if values.len() > maximum
        || unique.len() != values.len()
        || unique
            .iter()
            .any(|value| validate_client_id(value, "event subscription filter").is_err())
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            message,
        ));
    }
    Ok(unique)
}

fn subscription_error(
    subscription_id: &str,
    code: proto::SubscriptionErrorCode,
    message: &'static str,
) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::Rejected, 409, message).with_detail(
        prost_types::Any {
            type_url: "type.googleapis.com/keeppeek.webrtc.v1.SubscriptionError".to_owned(),
            value: proto::SubscriptionError {
                subscription_id: subscription_id.to_owned(),
                code: code as i32,
            }
            .encode_to_vec(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_filters_activate_and_replace_one_bounded_subscription() {
        let state = ServerState::empty();
        let registry = Registry::default();
        let request = proto::SubscribeEvents {
            subscription_id: "events-1".to_owned(),
            ..Default::default()
        };

        let first = registry
            .subscribe_with_clock(&state, SessionId::from_u64(7), request.clone(), || 1_000)
            .unwrap();
        let Some(proto::subscription_result::Delivery::Events(first_delivery)) = first.delivery
        else {
            panic!("event subscription must return event delivery");
        };
        assert_eq!(
            first_delivery.backfill_end_time,
            Some(millis_timestamp(1_000))
        );
        let stale_delivery = registry.deliveries(&proto::Event::default()).remove(0);

        let replacement = registry
            .subscribe_with_clock(&state, SessionId::from_u64(7), request, || 2_000)
            .unwrap();
        let Some(proto::subscription_result::Delivery::Events(replacement_delivery)) =
            replacement.delivery
        else {
            panic!("event subscription replacement must return event delivery");
        };
        assert_eq!(
            replacement_delivery.backfill_end_time,
            Some(millis_timestamp(2_000))
        );
        assert!(!stale_delivery.guard.is_active());
        assert_eq!(registry.inner.lock().unwrap().len(), 1);
        assert_eq!(registry.metrics_snapshot().starts, 1);

        registry.unsubscribe(SessionId::from_u64(7), &["events-1".to_owned()]);
        assert!(registry.inner.lock().unwrap().is_empty());
        assert_eq!(registry.metrics_snapshot().active, 0);
    }

    fn subscription_code(error: &ControlCommandError) -> proto::SubscriptionErrorCode {
        let detail = proto::SubscriptionError::decode(error.details[0].value.as_slice()).unwrap();
        proto::SubscriptionErrorCode::try_from(detail.code).unwrap()
    }

    #[test]
    fn unavailable_event_filters_and_routes_return_typed_errors() {
        let state = ServerState::empty();
        let base = proto::SubscribeEvents {
            subscription_id: "events-1".to_owned(),
            ..Default::default()
        };
        let cases = [
            (
                proto::SubscribeEvents {
                    source_ids: vec!["missing-camera".to_owned()],
                    ..base.clone()
                },
                proto::SubscriptionErrorCode::SourceNotFound,
            ),
            (
                proto::SubscribeEvents {
                    event_types: vec!["motion".to_owned()],
                    ..base.clone()
                },
                proto::SubscriptionErrorCode::EventTypeUnavailable,
            ),
            (
                proto::SubscribeEvents {
                    media_kinds: vec![proto::MediaKind::Audio as i32],
                    ..base.clone()
                },
                proto::SubscriptionErrorCode::MediaNotFound,
            ),
            (
                proto::SubscribeEvents {
                    attachment_routes: vec![proto::EventAttachmentRoute {
                        attachment_type: "story-frame".to_owned(),
                        content_type: "image/jpeg".to_owned(),
                        channel: proto::DataChannelKind::ReliableData as i32,
                    }],
                    ..base.clone()
                },
                proto::SubscriptionErrorCode::EventAttachmentUnavailable,
            ),
            (
                proto::SubscribeEvents {
                    attachment_routes: vec![proto::EventAttachmentRoute {
                        attachment_type: "snapshot".to_owned(),
                        content_type: "image/jpeg".to_owned(),
                        channel: 99,
                    }],
                    ..base
                },
                proto::SubscriptionErrorCode::DeliveryTransportUnavailable,
            ),
        ];

        for (request, expected) in cases {
            let error = validate_subscription(&state, &request).unwrap_err();
            assert_eq!(subscription_code(&error), expected);
        }
        let registry = Registry::default();
        registry
            .subscribe(
                &state,
                SessionId::from_u64(7),
                proto::SubscribeEvents {
                    subscription_id: "events-1".to_owned(),
                    source_ids: vec!["missing-camera".to_owned()],
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert_eq!(registry.metrics_snapshot().rejections, 1);
    }

    #[test]
    fn subscription_count_limits_are_bounded_and_allow_replacement() {
        let state = ServerState::empty();
        let registry = Registry::default();
        for session in 0..(MAXIMUM_EVENT_SUBSCRIPTIONS / MAXIMUM_EVENT_SUBSCRIPTIONS_PER_SESSION) {
            for subscription in 0..MAXIMUM_EVENT_SUBSCRIPTIONS_PER_SESSION {
                registry
                    .subscribe_with_clock(
                        &state,
                        SessionId::from_u64(session as u64),
                        proto::SubscribeEvents {
                            subscription_id: format!("events-{subscription}"),
                            ..Default::default()
                        },
                        || 1_000,
                    )
                    .unwrap();
            }
            if session == 0 {
                let error = registry
                    .subscribe_with_clock(
                        &state,
                        SessionId::from_u64(0),
                        proto::SubscribeEvents {
                            subscription_id: "events-over-session-limit".to_owned(),
                            ..Default::default()
                        },
                        || 1_000,
                    )
                    .unwrap_err();
                assert_eq!(error.code, proto::ErrorCode::Rejected);
                assert_eq!(registry.len(), MAXIMUM_EVENT_SUBSCRIPTIONS_PER_SESSION);
            }
        }
        assert_eq!(registry.len(), MAXIMUM_EVENT_SUBSCRIPTIONS);

        registry
            .subscribe_with_clock(
                &state,
                SessionId::from_u64(0),
                proto::SubscribeEvents {
                    subscription_id: "events-0".to_owned(),
                    event_types: vec!["person".to_owned()],
                    ..Default::default()
                },
                || 2_000,
            )
            .unwrap();
        assert_eq!(registry.len(), MAXIMUM_EVENT_SUBSCRIPTIONS);

        let error = registry
            .subscribe_with_clock(
                &state,
                SessionId::from_u64(999),
                proto::SubscribeEvents {
                    subscription_id: "one-too-many".to_owned(),
                    ..Default::default()
                },
                || 3_000,
            )
            .unwrap_err();
        assert_eq!(error.code, proto::ErrorCode::Rejected);
        assert_eq!(registry.len(), MAXIMUM_EVENT_SUBSCRIPTIONS);
    }

    #[test]
    fn deliveries_match_filters_and_select_the_requested_attachment_channel() {
        let registry = Registry::default();
        registry.inner.lock().unwrap().extend([
            (
                (SessionId::from_u64(8), "all".to_owned()),
                Subscription {
                    source_ids: HashSet::new(),
                    allowed_source_ids: None,
                    event_types: HashSet::new(),
                    media_kinds: HashSet::new(),
                    attachment_routes: Vec::new(),
                    guard: EventDeliveryGuard::default(),
                },
            ),
            (
                (SessionId::from_u64(7), "person".to_owned()),
                Subscription {
                    source_ids: HashSet::from(["front-door".to_owned()]),
                    allowed_source_ids: None,
                    event_types: HashSet::from(["person".to_owned()]),
                    media_kinds: HashSet::from([proto::MediaKind::Video]),
                    attachment_routes: vec![proto::EventAttachmentRoute {
                        attachment_type: "snapshot".to_owned(),
                        content_type: "image/jpeg".to_owned(),
                        channel: proto::DataChannelKind::UnreliableData as i32,
                    }],
                    guard: EventDeliveryGuard::default(),
                },
            ),
            (
                (SessionId::from_u64(9), "vehicle".to_owned()),
                Subscription {
                    source_ids: HashSet::new(),
                    allowed_source_ids: None,
                    event_types: HashSet::from(["vehicle".to_owned()]),
                    media_kinds: HashSet::new(),
                    attachment_routes: Vec::new(),
                    guard: EventDeliveryGuard::default(),
                },
            ),
        ]);
        let event = proto::Event {
            source_id: "front-door".to_owned(),
            media_kind: Some(proto::MediaKind::Video as i32),
            event_type: "person".to_owned(),
            attachments: vec![proto::EventAttachmentDescriptor {
                attachment_id: "snapshot-1".to_owned(),
                attachment_type: "snapshot".to_owned(),
                content_type: "image/jpeg".to_owned(),
                ..Default::default()
            }],
            canonical_attachment_id: Some("snapshot-1".to_owned()),
            ..Default::default()
        };

        let deliveries = registry.deliveries(&event);

        assert_eq!(deliveries.len(), 2);
        assert_eq!(deliveries[0].session_id, SessionId::from_u64(7));
        assert_eq!(deliveries[0].subscription_id, "person");
        assert_eq!(
            deliveries[0].attachment_target,
            Some(DataChannelTarget::Unreliable)
        );
        assert_eq!(deliveries[1].session_id, SessionId::from_u64(8));
        assert_eq!(deliveries[1].subscription_id, "all");
        assert_eq!(deliveries[1].attachment_target, None);

        let explicit_guard = deliveries[0].guard.clone();
        assert_eq!(
            registry.invalidate_source("front-door"),
            vec![SessionId::from_u64(7)]
        );
        assert!(!explicit_guard.is_active());
        assert_eq!(registry.deliveries(&event).len(), 1);
        registry.shed(SessionId::from_u64(8), "all");
        assert!(registry.deliveries(&event).is_empty());
        let metrics = registry.metrics_snapshot();
        assert_eq!(metrics.active, 1);
        assert_eq!(metrics.deliveries, 3);
        assert_eq!(metrics.sheds, 1);
    }
}
