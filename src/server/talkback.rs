use crate::{api::proto, webrtc::SessionId};
use std::sync::{Arc, Mutex, PoisonError};

const MAX_TALKBACK_TARGETS: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CameraTarget {
    pub(crate) source_id: String,
    pub(crate) groups: Vec<String>,
    pub(crate) enabled: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Error {
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
}

#[derive(Clone)]
pub(crate) struct Registry {
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
    ) -> Result<proto::TalkbackState, Error> {
        let active_source_ids = resolve_target(&target, cameras)?;
        let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        if active.is_some() {
            return Err(Error::AlreadyOwned);
        }
        *active = Some(ActiveTalkback {
            owner,
            target: target.clone(),
        });
        Ok(state(owner, target, active_source_ids))
    }

    pub(crate) fn stop(&self, owner: SessionId) -> Result<proto::TalkbackState, Error> {
        let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        let current = active.as_ref().ok_or(Error::NotOwner)?;
        if current.owner != owner {
            return Err(Error::NotOwner);
        }
        let current = active.take().expect("active talkback was checked above");
        Ok(state(owner, current.target, Vec::new()))
    }

    pub(crate) fn stop_for_session(&self, owner: SessionId) {
        let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        if active
            .as_ref()
            .is_some_and(|current| current.owner == owner)
        {
            *active = None;
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
        let cameras = [camera("front", &[], true)];
        let target = target(proto::talkback_target::Selection::All(
            proto::AllTalkbackTargets {},
        ));
        assert!(
            registry
                .start(SessionId::from_u64(1), target.clone(), &cameras)
                .is_ok()
        );
        assert_eq!(
            registry.start(SessionId::from_u64(2), target, &cameras),
            Err(Error::AlreadyOwned)
        );
        assert_eq!(registry.stop(SessionId::from_u64(2)), Err(Error::NotOwner));
        assert!(registry.stop(SessionId::from_u64(1)).is_ok());
    }
}
