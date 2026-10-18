use super::*;
use crate::api::proto;

impl ApiMediaRuntime {
    pub(super) fn subscribe(
        &mut self,
        session: &ApiSessionControl,
        request: &proto::SubscribeMedia,
        plan: MediaSubscriptionPlan,
    ) -> Result<proto::SubscriptionResult, ControlHandlerError> {
        if plan.media_kind == proto::MediaKind::Audio {
            return self.subscribe_audio(session, request, plan);
        }
        let track_id = TrackId::parse(request.subscription_id.clone()).map_err(|error| {
            ControlHandlerError::new(ErrorCode::InvalidRequest, error.to_string())
        })?;
        if let Some(index) = self
            .tracks
            .iter()
            .position(|track| track.runtime.track_id == track_id)
        {
            self.replace_video_subscription(session, request, plan, index)
        } else {
            self.add_video_subscription(session, request, plan, track_id)
        }
    }

    fn replace_video_subscription(
        &mut self,
        session: &ApiSessionControl,
        request: &proto::SubscribeMedia,
        plan: MediaSubscriptionPlan,
        index: usize,
    ) -> Result<proto::SubscriptionResult, ControlHandlerError> {
        validate_replacement(&self.tracks[index], &plan)?;
        let track_id = self.tracks[index].runtime.track_id.clone();
        self.discard_queued_stream_states(&track_id);
        self.tracks[index].runtime.update_subscription(&plan);
        self.bandwidth.dirty = true;
        session
            .media_camera_ips
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(plan.camera_ip);
        Ok(self.video_subscription_result(
            index,
            request.subscription_id.clone(),
            plan.selected_variant_id,
        ))
    }

    fn add_video_subscription(
        &mut self,
        session: &ApiSessionControl,
        request: &proto::SubscribeMedia,
        mut plan: MediaSubscriptionPlan,
        track_id: TrackId,
    ) -> Result<proto::SubscriptionResult, ControlHandlerError> {
        let selected_stream = native_variant_stream(&plan.selected_variant_id)?;
        let delivery = self.take_delivery(&request.subscription_id, &mut plan)?;
        let initial_stream = if matches!(delivery, TrackDelivery::ReliableData(_)) {
            selected_stream
        } else {
            initial_stream(plan.has_sub_stream)
        };
        let runtime = TrackRuntime::new(
            TrackPlan {
                track_id: track_id.clone(),
                camera_ip: plan.camera_ip,
                has_main_stream: plan.has_main_stream,
                has_sub_stream: plan.has_sub_stream,
                selected_stream,
                recording_label: plan.recording_label,
            },
            delivery,
            TrackDeps {
                inner: session.inner.clone(),
                session_id: session.session_id,
                control: Arc::new(SessionControl::new(plan.quality, initial_stream)),
                poller: session.poller.clone(),
                shutdown: session.shutdown.clone(),
                recording_demand: session.recording_demand.clone(),
            },
        );
        self.tracks.push(ApiMediaTrack {
            source_session_id: plan.source_session_id,
            runtime,
        });
        self.bandwidth.dirty = true;
        session
            .media_camera_ips
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(plan.camera_ip);
        self.enqueue_stream_state(&track_id, initial_stream);
        Ok(self.video_subscription_result(
            self.tracks.len() - 1,
            request.subscription_id.clone(),
            plan.selected_variant_id,
        ))
    }

    fn video_subscription_result(
        &self,
        index: usize,
        subscription_id: String,
        selected_variant_id: String,
    ) -> proto::SubscriptionResult {
        let runtime = &self.tracks[index].runtime;
        let mut result =
            subscription_result(subscription_id, &runtime.delivery, selected_variant_id);
        result.live_quality = self.live_quality(&runtime.track_id);
        result
    }

    fn subscribe_audio(
        &mut self,
        session: &ApiSessionControl,
        request: &proto::SubscribeMedia,
        mut plan: MediaSubscriptionPlan,
    ) -> Result<proto::SubscriptionResult, ControlHandlerError> {
        let track_id = TrackId::parse(request.subscription_id.clone()).map_err(|error| {
            ControlHandlerError::new(ErrorCode::InvalidRequest, error.to_string())
        })?;
        if self
            .audio_tracks
            .iter()
            .any(|track| track.runtime.track_id == track_id)
        {
            return Err(ControlHandlerError::new(
                ErrorCode::InvalidRequest,
                "audio subscription replacement is not supported",
            ));
        }
        let selected_stream = native_variant_stream(&plan.selected_variant_id)?;
        let reserved_bitrate_bps = bandwidth::audio_reservation_bps(&plan.format);
        let delivery = self.take_delivery(&request.subscription_id, &mut plan)?;
        let runtime = AudioTrackRuntime::new(
            session,
            track_id,
            Source {
                camera_ip: plan.camera_ip,
                stream: selected_stream,
            },
            delivery,
        );
        let result = subscription_result(
            request.subscription_id.clone(),
            &runtime.delivery,
            plan.selected_variant_id,
        );
        self.audio_tracks.push(ApiAudioTrack {
            runtime,
            reserved_bitrate_bps,
        });
        self.bandwidth.dirty = true;
        session
            .media_camera_ips
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(plan.camera_ip);
        Ok(result)
    }

    fn take_delivery(
        &mut self,
        subscription_id: &str,
        plan: &mut MediaSubscriptionPlan,
    ) -> Result<TrackDelivery, ControlHandlerError> {
        let audio = plan.media_kind == proto::MediaKind::Audio;
        match plan.delivery_transport {
            proto::DeliveryTransport::Rtp => {
                let (available_mids, kind) = if audio {
                    (&mut self.available_audio_mids, "audio")
                } else {
                    (&mut self.available_video_mids, "video")
                };
                available_mids.pop().map(TrackDelivery::Rtp).ok_or_else(|| {
                    ControlHandlerError::new(
                        ErrorCode::Unavailable,
                        format!("no negotiated {kind} MID is available"),
                    )
                })
            }
            proto::DeliveryTransport::ReliableData => {
                Ok(TrackDelivery::ReliableData(Box::new(DataMediaBinding {
                    stream_binding_id: format!("media:{subscription_id}"),
                    codec: std::mem::take(&mut plan.codec),
                    format: std::mem::take(&mut plan.format),
                    configuration_revision: 1,
                    next_frame_id: 1,
                    source_clock: None,
                    pending_configuration: None,
                })))
            }
            _ => {
                let kind = if audio { "audio" } else { "media" };
                Err(ControlHandlerError::new(
                    ErrorCode::InvalidRequest,
                    format!("{kind} subscription plan has an unsupported delivery transport"),
                ))
            }
        }
    }
}

fn validate_replacement(
    track: &ApiMediaTrack,
    plan: &MediaSubscriptionPlan,
) -> Result<(), ControlHandlerError> {
    if track.source_session_id != plan.source_session_id {
        return Err(ControlHandlerError::new(
            ErrorCode::InvalidRequest,
            "a media subscription replacement must keep the same source session",
        ));
    }
    let transport_matches = matches!(
        (&track.runtime.delivery, plan.delivery_transport),
        (TrackDelivery::Rtp(_), proto::DeliveryTransport::Rtp)
            | (
                TrackDelivery::ReliableData(_),
                proto::DeliveryTransport::ReliableData
            )
    );
    if !transport_matches {
        return Err(ControlHandlerError::new(
            ErrorCode::InvalidRequest,
            "a media subscription replacement must keep its delivery transport",
        ));
    }
    if matches!(track.runtime.delivery, TrackDelivery::ReliableData(_))
        && native_variant_stream(&plan.selected_variant_id)?
            != track.runtime.subscription.active_source.stream
    {
        return Err(ControlHandlerError::new(
            ErrorCode::Unavailable,
            "fixed delivery requires a new subscription for another variant",
        ));
    }
    Ok(())
}

fn native_variant_stream(variant_id: &str) -> Result<StreamKind, ControlHandlerError> {
    match variant_id {
        "main" => Ok(StreamKind::Main),
        "sub" => Ok(StreamKind::Sub),
        _ => Err(ControlHandlerError::new(
            ErrorCode::Internal,
            "media subscription plan selected an invalid variant",
        )),
    }
}
