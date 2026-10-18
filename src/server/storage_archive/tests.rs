use super::*;

#[test]
fn archive_controls_reject_users_before_reading_configuration_or_runtime() {
    use proto::archive_command::Action;
    let state = ServerState::empty();
    let mut principal = ApiPrincipal::local("127.0.0.1".parse().unwrap());
    principal.role = crate::access::AccessRole::User;
    for action in [
        Action::Probe(Default::default()),
        Action::List(Default::default()),
        Action::Get(Default::default()),
        Action::Copy(Default::default()),
        Action::Cancel(Default::default()),
        Action::Retry(Default::default()),
        Action::Restore(Default::default()),
        Action::Evict(Default::default()),
    ] {
        let command = proto::ArchiveCommand {
            action: Some(action),
        };
        let request = proto::Request {
            request_id: 130,
            command: Some(proto::request::Command::ArchiveCommand(command.clone())),
        };
        assert_eq!(
            proto::Request::decode(request.encode_to_vec().as_slice()).unwrap(),
            request
        );
        assert_eq!(
            dispatch(&state, &principal, command)
                .unwrap_err()
                ._http_status,
            403
        );
    }
}

#[test]
fn archive_list_reports_unavailable_runtime_and_rejects_unbounded_pages() {
    let state = ServerState::empty();
    let principal = ApiPrincipal::local("127.0.0.1".parse().unwrap());
    let request = |limit| proto::ArchiveCommand {
        action: Some(proto::archive_command::Action::List(
            proto::ListArchiveJobs {
                limit,
                after_job_id: String::new(),
            },
        )),
    };
    let proto::ok::Result::ArchiveResult(result) =
        dispatch(&state, &principal, request(16)).unwrap()
    else {
        panic!("missing archive response")
    };
    let Some(proto::archive_result::Result::Jobs(jobs)) = result.result else {
        panic!("missing archive jobs")
    };
    assert!(jobs.jobs.is_empty());
    assert!(!jobs.status.unwrap().runtime_available);
    assert_eq!(
        dispatch(&state, &principal, request(65))
            .unwrap_err()
            ._http_status,
        400
    );
}

#[test]
fn archive_wire_configuration_keeps_private_references_and_rejects_invalid_limits() {
    let directory = std::env::temp_dir().join(format!("archive-wire-{}", uuid::Uuid::new_v4()));
    let path = directory.join("config.toml");
    let secrets = toml::Table::from_iter([
        (
            "ROOT".into(),
            directory.join("archive").to_string_lossy().as_ref().into(),
        ),
        ("TARGET".into(), "local-archive".into()),
        ("SOURCE".into(), "192.0.2.10".into()),
    ]);
    crate::config::write_private_file(
        &crate::config::secrets_path(&path),
        toml::to_string(&secrets).unwrap().as_bytes(),
    )
    .unwrap();
    crate::config::write_private_file(&path, b"[storage.archive]\ntarget_id='{secret:TARGET}'\nroot='{secret:ROOT}'\nsource_ids=['{secret:SOURCE}']\n").unwrap();
    let config = crate::config::load_config(&path).unwrap();
    let wire = to_wire(sanitized(&config).unwrap());
    assert_eq!(wire.root, "{secret:ROOT}");
    assert_eq!(wire.target_id, "{secret:TARGET}");
    assert_eq!(wire.source_ids, ["{secret:SOURCE}"]);
    assert_eq!(
        from_wire(&path, wire.clone()).unwrap().root,
        Path::new("{secret:ROOT}")
    );
    let invalid = proto::ArchiveSettings {
        maximum_object_bytes: 0,
        ..wire
    };
    assert!(from_wire(&path, invalid).is_err());
    assert!(!directory.join("archive").exists());
    std::fs::remove_dir_all(directory).unwrap();
}
