use super::*;
use crate::storage::catalog::{CatalogRecording, RecordingCatalog};
use std::{
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
};

struct Fixture {
    root: PathBuf,
    catalog: Option<RecordingCatalog>,
    state: ServerState,
    principal: ApiPrincipal,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-preservation-{:032x}",
            rand::random::<u128>()
        ));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("media.mp4");
        std::fs::write(&path, [42; 64]).unwrap();
        let catalog = RecordingCatalog::open(&root.join("catalog.db")).unwrap();
        let handle = catalog.handle();
        handle
            .upsert_recording(CatalogRecording {
                id: "media".into(),
                stream_id: "camera/sub".into(),
                source_id: Some("camera".into()),
                logical_stream_id: Some("sub".into()),
                started_at_ms: 1000,
                ended_at_ms: Some(2000),
                path: path.to_str().unwrap().into(),
                init_offset: 0,
                init_len: 8,
                finalized: true,
            })
            .unwrap();
        handle.update_recording_path("media", &path, true).unwrap();
        let mut state = ServerState::empty();
        state.catalog = Some(handle);
        state.storage_config.long_term_path = root.clone();
        Self {
            root,
            catalog: Some(catalog),
            state,
            principal: ApiPrincipal::local(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        }
    }

    fn request(
        &self,
        action: proto::preservation_command::Action,
    ) -> Result<proto::PreservationState, ControlCommandError> {
        let result = dispatch(
            &self.state,
            &self.principal,
            proto::PreservationCommand {
                target: Some(proto::PreservationTarget {
                    target: Some(proto::preservation_target::Target::RecordingId(
                        "media".into(),
                    )),
                }),
                action: Some(action),
            },
        )?;
        let proto::ok::Result::PreservationState(state) = result else {
            panic!("wrong result")
        };
        Ok(state)
    }

    fn get(&self) -> proto::PreservationState {
        self.request(proto::preservation_command::Action::Get(
            proto::GetPreservation {},
        ))
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.catalog.take());
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn change(revision: Option<u64>) -> proto::MutatePreservation {
    proto::MutatePreservation {
        expected_revision: revision,
        reason: "Keep evidence".into(),
    }
}

#[test]
fn save_release_and_stale_retry_preserve_other_holds() {
    use proto::preservation_command::Action;
    let fixture = Fixture::new();
    assert_eq!(fixture.get().revision, 0);
    let saved = fixture
        .request(Action::SaveForever(change(Some(0))))
        .unwrap();
    assert!(saved.marker_active);
    assert_eq!(saved.actor, fixture.principal.id());
    assert_eq!(saved.protected_objects, 1);
    assert_eq!(saved.protected_bytes, 64);
    assert_eq!(
        saved.coverage,
        proto::PreservationCoverage::Protected as i32
    );
    assert!(!saved.independently_protected);
    assert!(fixture.request(Action::Release(change(Some(0)))).is_err());
    fixture
        .state
        .catalog
        .as_ref()
        .unwrap()
        .update_recording_hold(
            "media",
            "event",
            holds::Update {
                expected_revision: None,
                active: true,
                actor: "event owner".into(),
                reason: "other evidence".into(),
            },
        )
        .unwrap();
    let released = fixture
        .request(Action::Release(change(Some(saved.revision))))
        .unwrap();
    assert!(!released.marker_active);
    assert!(released.independently_protected);
    assert_eq!(
        released.coverage,
        proto::PreservationCoverage::Protected as i32
    );
    assert_eq!(released.protected_objects, 1);
    assert!(
        fixture
            .request(Action::SaveForever(change(Some(saved.revision))))
            .is_err()
    );
    assert!(fixture.root.join("media.mp4").is_file());
}

#[test]
fn missing_media_does_not_hide_or_prevent_releasing_a_marker() {
    use proto::preservation_command::Action;
    let fixture = Fixture::new();
    let saved = fixture
        .request(Action::SaveForever(change(Some(0))))
        .unwrap();
    std::fs::remove_file(fixture.root.join("media.mp4")).unwrap();
    let missing = fixture.get();
    assert!(missing.marker_active);
    assert_eq!(missing.revision, saved.revision);
    assert_eq!(
        missing.coverage,
        proto::PreservationCoverage::Unavailable as i32
    );
    assert_eq!(missing.protected_objects, 0);
    assert_eq!(missing.protected_bytes, 0);
    assert!(!missing.gaps.is_empty());
    assert!(
        !fixture
            .request(Action::Release(change(Some(saved.revision))))
            .unwrap()
            .marker_active
    );
}

#[test]
fn malformed_mutations_and_non_administrators_cannot_change_protection() {
    use proto::preservation_command::Action;
    let mut fixture = Fixture::new();
    assert!(fixture.request(Action::SaveForever(change(None))).is_err());
    for reason in [
        String::new(),
        " ".into(),
        "é".repeat(129),
        "bad\0reason".into(),
    ] {
        assert!(
            fixture
                .request(Action::SaveForever(proto::MutatePreservation {
                    expected_revision: Some(0),
                    reason,
                }))
                .is_err()
        );
    }
    assert_eq!(fixture.get().revision, 0);
    fixture.principal.role = crate::access::AccessRole::User;
    assert!(
        fixture
            .request(Action::SaveForever(change(Some(0))))
            .is_err()
    );
    assert!(
        fixture
            .request(Action::Get(proto::GetPreservation {}))
            .is_err()
    );
}

#[test]
fn event_targets_never_fall_through_to_recording_identity() {
    let fixture = Fixture::new();
    let result = dispatch(
        &fixture.state,
        &fixture.principal,
        proto::PreservationCommand {
            target: Some(proto::PreservationTarget {
                target: Some(proto::preservation_target::Target::EventId("media".into())),
            }),
            action: Some(proto::preservation_command::Action::SaveForever(change(
                Some(0),
            ))),
        },
    );
    assert!(result.is_err());
    assert_eq!(fixture.get().revision, 0);
}

#[test]
fn protobuf_keeps_explicit_initial_revision_distinct_from_omission() {
    use prost::Message as _;
    for expected in [None, Some(0), Some(17)] {
        let encoded = change(expected).encode_to_vec();
        assert_eq!(
            proto::MutatePreservation::decode(encoded.as_slice())
                .unwrap()
                .expected_revision,
            expected
        );
    }
}

#[test]
fn saving_already_missing_media_reports_a_gap_instead_of_complete_coverage() {
    let fixture = Fixture::new();
    std::fs::remove_file(fixture.root.join("media.mp4")).unwrap();
    let state = fixture
        .request(proto::preservation_command::Action::SaveForever(change(
            Some(0),
        )))
        .unwrap();
    assert!(state.marker_active);
    assert_eq!(
        state.coverage,
        proto::PreservationCoverage::Unavailable as i32
    );
    assert_eq!(state.protected_objects, 0);
    assert_eq!(state.protected_bytes, 0);
}

#[test]
fn capabilities_advertise_only_the_available_target_workflow() {
    let mut fixture = Fixture::new();
    let capabilities = super::super::server_capabilities(&fixture.state, &[]);
    assert!(
        capabilities
            .capability_ids
            .iter()
            .any(|id| id == CAPABILITY)
    );
    assert!(
        !capabilities
            .capability_ids
            .iter()
            .any(|id| id == "keeppeek.event-preservation.v1")
    );
    fixture.state.catalog = None;
    let capabilities = super::super::server_capabilities(&fixture.state, &[]);
    assert!(
        !capabilities
            .capability_ids
            .iter()
            .any(|id| id == CAPABILITY)
    );
}
