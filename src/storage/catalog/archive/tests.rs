use super::*;

async fn fixture() -> anyhow::Result<turso::Connection> {
    let database = turso::Builder::new_local(":memory:")
        .experimental_generated_columns(true)
        .build()
        .await?;
    let connection = database.connect()?;
    super::super::initialize_schema(&connection).await?;
    connection.execute_batch(
        "INSERT INTO storage_volume_bindings(id,generation,root,filesystem,root_identity,writable,minimum_free_bytes)
         VALUES('hot',1,'/hot','hotfs','hotroot',1,0);
         INSERT INTO recording_files(id,stream_id,started_at_ms,ended_at_ms,path,init_offset,init_len,
             finalized,finalized_at_ms,file_identity,file_bytes)
         VALUES('recording','camera/main',1,2,'/hot/aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa.mp4',0,1,
             1,2,'source-version',4);
         INSERT INTO storage_volume_allocations(operation,kind,object_id,volume_id,generation,
             relative_key,destination_path,bytes,intent_bytes,materialized_bytes,state,file_identity,digest,location_revision)
         VALUES('source-operation','recording','recording','hot',1,
             'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa.mp4','/hot/aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa.mp4',
             4,4,4,'published','source-version',zeroblob(32),1);",
    ).await?;
    connection.execute_batch(
        "INSERT INTO recording_fragments(recording_id,sequence,start_ms,duration_ms,byte_offset,byte_len,random_access)
         VALUES('recording',1,1,1,1,3,1);",
    ).await?;
    Ok(connection)
}

fn binding() -> Binding {
    Binding {
        id: "cold".into(),
        root: PathBuf::from("/cold"),
        identity: Identity {
            filesystem: "coldfs".into(),
            directory: "coldroot".into(),
        },
    }
}

fn admission() -> Admission {
    Admission {
        job_id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into(),
        recording_id: "recording".into(),
        kind: "recording".into(),
        target: "cold".into(),
        object_limit: 16,
        count_limit: 1,
        bytes_limit: 16,
        export_job_id: None,
        export_metadata: None,
    }
}

async fn ready_for_eviction(connection: &turso::Connection) -> anyhow::Result<()> {
    bind(connection, &binding()).await?;
    let id = admission().job_id;
    enqueue(connection, &admission()).await?;
    start(connection, &id, 2).await?;
    let version = Version::new(
        crate::storage::archive::target::ObjectId::parse(&id)?,
        "archive-version".into(),
        4,
    )?;
    stage(connection, &id, &version).await?;
    ready(connection, &id, &version, [0; 32], &[0; 32]).await?;
    connection
        .execute(
            "UPDATE archive_replicas SET action='evict' WHERE job_id=?1",
            [id.as_str()],
        )
        .await?;
    Ok(())
}

async fn begin_eviction(connection: &turso::Connection) -> anyhow::Result<()> {
    super::super::archive_playback::begin_eviction(
        connection,
        &super::super::readers::Registry::default(),
        "recording",
        "retirement",
    )
    .await?;
    Ok(())
}

async fn assert_preintent_holds(connection: &turso::Connection) -> anyhow::Result<()> {
    super::super::set_recording_protected(connection, "recording", true).await?;
    assert!(begin_eviction(connection).await.is_err());
    super::super::set_recording_protected(connection, "recording", false).await?;
    connection
        .execute(
            "UPDATE recording_retention_runtime SET request_pending=1 WHERE singleton=1",
            (),
        )
        .await?;
    assert!(begin_eviction(connection).await.is_err());
    assert_eq!(
        required(connection, &admission().job_id)
            .await?
            .local_evicted,
        0
    );
    connection
        .execute(
            "UPDATE recording_retention_runtime SET request_pending=0 WHERE singleton=1",
            (),
        )
        .await?;
    Ok(())
}

async fn assert_late_holds_rejected(connection: &turso::Connection) -> anyhow::Result<()> {
    let protection = super::super::set_recording_protected(connection, "recording", true)
        .await
        .unwrap_err();
    assert!(
        protection
            .to_string()
            .contains("archive eviction is in progress")
    );
    let retention = connection.execute("UPDATE recording_retention_decisions SET deadline_ms=9223372036854775807 WHERE recording_id='recording'", ()).await.unwrap_err();
    assert!(
        retention
            .to_string()
            .contains("archive eviction is in progress")
    );
    let insertion = connection.execute_batch("INSERT INTO recording_retention_decisions
        (recording_id,policy_revision,policy_fingerprint,event_revision,deadline_ms,matching_rules_json,reason_json)
        VALUES('recording',2,zeroblob(32),0,NULL,'[]','\"no-matching-evidence\"')").await.unwrap_err();
    assert!(
        insertion
            .to_string()
            .contains("archive eviction is in progress")
    );
    let mut rows = connection
        .query(
            "SELECT protected FROM recording_files WHERE id='recording'",
            (),
        )
        .await?;
    assert_eq!(rows.next().await?.expect("recording").get::<i64>(0)?, 0);
    Ok(())
}

#[test]
fn eviction_intent_fences_late_holds_and_pending_retention() -> anyhow::Result<()> {
    pollster::block_on(async {
        let connection = fixture().await?;
        ready_for_eviction(&connection).await?;
        assert_preintent_holds(&connection).await?;
        connection.execute_batch("INSERT INTO recording_retention_decisions
            (recording_id,policy_revision,policy_fingerprint,event_revision,deadline_ms,matching_rules_json,reason_json)
            VALUES('recording',1,zeroblob(32),0,NULL,'[]','\"no-matching-evidence\"')").await?;
        begin_eviction(&connection).await?;
        assert_eq!(
            required(&connection, &admission().job_id)
                .await?
                .local_evicted,
            2
        );
        assert_late_holds_rejected(&connection).await
    })
}

#[test]
fn archive_binding_is_immutable_and_enqueue_is_idempotent() -> anyhow::Result<()> {
    pollster::block_on(async {
        let connection = fixture().await?;
        bind(&connection, &binding()).await?;
        bind(&connection, &binding()).await?;
        let mut changed = binding();
        changed.identity.directory = "other-root".into();
        assert!(bind(&connection, &changed).await.is_err());
        let original = enqueue(&connection, &admission()).await?;
        let duplicate = enqueue(&connection, &admission()).await?;
        assert!(matches!(
            (original, duplicate),
            (Reply::Job(Some(_)), Reply::Job(Some(_)))
        ));
        let mut wrong = admission();
        wrong.target = "other".into();
        assert!(enqueue(&connection, &wrong).await.is_err());
        Ok(())
    })
}

#[test]
fn ready_requires_matching_staged_version_and_chunk_evidence() -> anyhow::Result<()> {
    pollster::block_on(async {
        let connection = fixture().await?;
        bind(&connection, &binding()).await?;
        enqueue(&connection, &admission()).await?;
        let id = admission().job_id;
        start(&connection, &id, 2).await?;
        let version = Version::new(
            crate::storage::archive::target::ObjectId::parse(&id)?,
            "archive-version".into(),
            4,
        )?;
        assert!(
            ready(&connection, &id, &version, [0; 32], &[0; 32])
                .await
                .is_err()
        );
        stage(&connection, &id, &version).await?;
        assert!(
            ready(&connection, &id, &version, [1; 32], &[0; 32])
                .await
                .is_err()
        );
        assert!(
            ready(&connection, &id, &version, [0; 32], &[0; 31])
                .await
                .is_err()
        );
        assert!(matches!(
            ready(&connection, &id, &version, [0; 32], &[0; 32]).await?,
            Reply::Job(Some(_))
        ));
        assert!(super::super::archive_playback::preserves_index(&connection, "recording").await?);
        let primary =
            super::super::media_fragments_in_range(&connection, "camera/main", 1, 2).await?;
        assert_eq!(primary.len(), 1);
        assert!(primary[0].archive.is_none());
        connection
            .execute(
                "UPDATE archive_replicas SET local_evicted=2 WHERE job_id=?1",
                [id.as_str()],
            )
            .await?;
        let archived =
            super::super::media_fragments_in_range(&connection, "camera/main", 1, 2).await?;
        assert_eq!(
            archived[0].archive.as_ref().expect("archive").version,
            "archive-version"
        );
        assert_eq!(archived[0].path, primary[0].path);
        Ok(())
    })
}

#[test]
fn ready_rejects_a_republished_source_identity() -> anyhow::Result<()> {
    pollster::block_on(async {
        let connection = fixture().await?;
        bind(&connection, &binding()).await?;
        let id = admission().job_id;
        enqueue(&connection, &admission()).await?;
        start(&connection, &id, 2).await?;
        let version = Version::new(
            crate::storage::archive::target::ObjectId::parse(&id)?,
            "archive-version".into(),
            4,
        )?;
        stage(&connection, &id, &version).await?;
        connection.execute("UPDATE storage_volume_allocations SET file_identity='replacement' WHERE operation='source-operation'", ()).await?;
        assert!(
            ready(&connection, &id, &version, [0; 32], &[0; 32])
                .await
                .is_err()
        );
        assert_eq!(required(&connection, &id).await?.state, "copying");
        Ok(())
    })
}

#[test]
fn restore_reservation_preserves_its_recording_index() -> anyhow::Result<()> {
    pollster::block_on(async {
        let connection = fixture().await?;
        bind(&connection, &binding()).await?;
        let id = admission().job_id;
        enqueue(&connection, &admission()).await?;
        start(&connection, &id, 2).await?;
        let version = Version::new(
            crate::storage::archive::target::ObjectId::parse(&id)?,
            "archive-version".into(),
            4,
        )?;
        stage(&connection, &id, &version).await?;
        ready(&connection, &id, &version, [0; 32], &[0; 32]).await?;
        connection
            .execute(
                "UPDATE archive_replicas SET local_evicted=1,action='restore' WHERE job_id=?1",
                [id.as_str()],
            )
            .await?;
        connection.execute("UPDATE storage_volume_allocations SET state='reserved' WHERE operation='source-operation'", ()).await?;
        assert!(
            connection
                .execute("DELETE FROM recording_files WHERE id='recording'", ())
                .await
                .is_err()
        );
        let mut rows = connection
            .query("SELECT id FROM recording_files WHERE id='recording'", ())
            .await?;
        assert!(rows.next().await?.is_some());
        assert_eq!(required(&connection, &id).await?.local_evicted, 1);
        Ok(())
    })
}
