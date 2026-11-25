use crate::{
    api::proto,
    webrtc::{SessionId, WebRtc},
};
use std::sync::{Arc, Mutex, PoisonError};

const MAX_TALKBACK_TARGETS: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraTarget {
    pub(crate) source_id: String,
    pub(crate) groups: Vec<String>,
    pub(crate) enabled: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    AlreadyOwned,
    NotOwner,
    MissingTarget,
    TargetNotFound,
    NoEnabledTargets,
    TooManyTargets,
}

struct ActiveTalkback {
    owner: SessionId,
    target: proto::TalkbackTarget,
    sources: Vec<String>,
}

#[derive(Clone)]
pub struct Registry {
    active: Arc<Mutex<Option<ActiveTalkback>>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            active: Arc::new(Mutex::new(None)),
        }
    }
}

impl Registry {
    pub(crate) fn start(
        &self,
        owner: SessionId,
        target: proto::TalkbackTarget,
        cameras: &[CameraTarget],
        webrtc: &WebRtc,
    ) -> Result<proto::TalkbackState, Error> {
        let active_source_ids = resolve_target(&target, cameras)?;
        let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        if active.is_some() || !webrtc.arm_talkback(owner, &active_source_ids) {
            return Err(Error::AlreadyOwned);
        }
        *active = Some(ActiveTalkback {
            owner,
            target: target.clone(),
            sources: active_source_ids.clone(),
        });
        Ok(state(owner, target, active_source_ids))
    }

    pub(crate) fn stop(
        &self,
        owner: SessionId,
        webrtc: &WebRtc,
    ) -> Result<proto::TalkbackState, Error> {
        let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        let current = active.as_ref().ok_or(Error::NotOwner)?;
        if current.owner != owner {
            return Err(Error::NotOwner);
        }
        webrtc.disarm_talkback(owner);
        let current = active.take().expect("active talkback was checked above");
        Ok(state(owner, current.target, Vec::new()))
    }

    pub(crate) fn stop_for_session(&self, owner: SessionId, webrtc: &WebRtc) {
        let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        webrtc.disarm_talkback(owner);
        if active
            .as_ref()
            .is_some_and(|current| current.owner == owner)
        {
            *active = None;
        }
    }

    pub(super) fn retire_source(&self, source: &str, webrtc: &WebRtc) {
        self.retire_source_inner(source, None, webrtc);
    }

    pub(super) fn retire_source_for_owner(
        &self,
        source: &str,
        expected_owner: SessionId,
        webrtc: &WebRtc,
    ) {
        self.retire_source_inner(source, Some(expected_owner), webrtc);
    }

    fn retire_source_inner(
        &self,
        source: &str,
        expected_owner: Option<SessionId>,
        webrtc: &WebRtc,
    ) {
        let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(talk) = active.as_mut() else { return };
        if expected_owner.is_some_and(|owner| talk.owner != owner) {
            return;
        }
        let Some(index) = talk.sources.iter().position(|id| id == source) else {
            return;
        };
        let owner = talk.owner;
        webrtc.live().retire_talkback_source(source, owner);
        talk.sources.remove(index);
        let mut remaining = state(owner, talk.target.clone(), talk.sources.clone());
        remaining.failed_source_ids.push(source.to_owned());
        if talk.sources.is_empty() {
            *active = None;
        }
        let notice = proto::Notification {
            event: Some(proto::notification::Event::TalkbackState(remaining)),
        };
        if !matches!(webrtc.try_enqueue_api_notification(owner, notice), Ok(true)) {
            webrtc.request_api_session_close(owner);
        }
    }
}

fn resolve_target(
    target: &proto::TalkbackTarget,
    cameras: &[CameraTarget],
) -> Result<Vec<String>, Error> {
    let selection = target.selection.as_ref().ok_or(Error::MissingTarget)?;
    let matches = cameras.iter().filter(|camera| match selection {
        proto::talkback_target::Selection::SourceId(source_id) => camera.source_id == *source_id,
        proto::talkback_target::Selection::GroupId(group_id) => {
            camera.groups.iter().any(|group| group == group_id)
        }
        proto::talkback_target::Selection::All(_) => true,
    });
    let mut active = Vec::new();
    let mut found = false;
    for camera in matches {
        found = true;
        if camera.enabled {
            if active.len() == MAX_TALKBACK_TARGETS {
                return Err(Error::TooManyTargets);
            }
            active.push(camera.source_id.clone());
        }
    }
    if !found {
        return Err(Error::TargetNotFound);
    }
    if active.is_empty() {
        return Err(Error::NoEnabledTargets);
    }
    Ok(active)
}

fn state(
    owner: SessionId,
    target: proto::TalkbackTarget,
    active_source_ids: Vec<String>,
) -> proto::TalkbackState {
    proto::TalkbackState {
        owner_session_id: owner.to_string(),
        target: Some(target),
        active_source_ids,
        failed_source_ids: Vec::new(),
        microphone_active: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera(source_id: &str, groups: &[&str], enabled: bool) -> CameraTarget {
        CameraTarget {
            source_id: source_id.to_owned(),
            groups: groups.iter().map(|group| (*group).to_owned()).collect(),
            enabled,
        }
    }

    fn target(selection: proto::talkback_target::Selection) -> proto::TalkbackTarget {
        proto::TalkbackTarget {
            selection: Some(selection),
        }
    }

    #[test]
    fn resolves_one_camera_and_rejects_disabled_camera() {
        let cameras = [
            camera("front", &["doors"], true),
            camera("back", &[], false),
        ];
        let resolved = resolve_target(
            &target(proto::talkback_target::Selection::SourceId(
                "front".to_owned(),
            )),
            &cameras,
        );
        assert_eq!(resolved, Ok(vec!["front".to_owned()]));
        assert_eq!(
            resolve_target(
                &target(proto::talkback_target::Selection::SourceId(
                    "back".to_owned()
                )),
                &cameras,
            ),
            Err(Error::NoEnabledTargets)
        );
    }

    #[test]
    fn resolves_group_and_all_targets() {
        let cameras = [
            camera("front", &["doors"], true),
            camera("side", &["doors"], true),
            camera("yard", &["outdoor"], true),
        ];
        assert_eq!(
            resolve_target(
                &target(proto::talkback_target::Selection::GroupId(
                    "doors".to_owned()
                )),
                &cameras,
            ),
            Ok(vec!["front".to_owned(), "side".to_owned()])
        );
        assert_eq!(
            resolve_target(
                &target(proto::talkback_target::Selection::All(
                    proto::AllTalkbackTargets {},
                )),
                &cameras,
            ),
            Ok(vec![
                "front".to_owned(),
                "side".to_owned(),
                "yard".to_owned()
            ])
        );
    }

    #[test]
    fn only_one_session_owns_talkback() {
        let registry = Registry::default();
        let webrtc = WebRtc::new();
        let cameras = [camera("front", &[], true)];
        let target = target(proto::talkback_target::Selection::All(
            proto::AllTalkbackTargets {},
        ));
        assert!(
            registry
                .start(SessionId::from_u64(1), target.clone(), &cameras, &webrtc)
                .is_ok()
        );
        assert_eq!(
            registry.start(SessionId::from_u64(2), target.clone(), &cameras, &webrtc),
            Err(Error::AlreadyOwned)
        );
        assert_eq!(
            registry.stop(SessionId::from_u64(2), &webrtc),
            Err(Error::NotOwner)
        );
        registry.stop_for_session(SessionId::from_u64(1), &webrtc);
        assert!(
            registry
                .start(SessionId::from_u64(2), target, &cameras, &webrtc)
                .is_ok()
        );
        assert!(registry.stop(SessionId::from_u64(2), &webrtc).is_ok());
    }

    #[test]
    fn retire_source_for_owner_ignores_stale_owner_and_retires_current_owner() {
        let registry = Registry::default();
        let webrtc = WebRtc::new();
        let cameras = [camera("front", &[], true), camera("side", &[], true)];
        let target = target(proto::talkback_target::Selection::All(
            proto::AllTalkbackTargets {},
        ));
        let stale_owner = SessionId::from_u64(1);
        let current_owner = SessionId::from_u64(2);
        registry
            .start(current_owner, target, &cameras, &webrtc)
            .unwrap();

        registry.retire_source_for_owner("front", stale_owner, &webrtc);
        assert!(webrtc.live().talkback_is_armed("front"));
        assert!(webrtc.live().talkback_is_armed("side"));

        registry.retire_source_for_owner("front", current_owner, &webrtc);
        assert!(!webrtc.live().talkback_is_armed("front"));
        assert!(webrtc.live().talkback_is_armed("side"));
        registry.stop_for_session(current_owner, &webrtc);
    }
}
