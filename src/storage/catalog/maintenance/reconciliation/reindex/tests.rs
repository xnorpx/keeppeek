use super::*;

#[test]
fn queued_reindex_rejects_replaced_media_before_catalog_commit() {
    pollster::block_on(async {
        let root =
            std::env::temp_dir().join(format!("keeppeek-reindex-{:032x}", rand::random::<u128>()));
        std::fs::create_dir(&root).unwrap();
        let file = root.join("recording.mp4");
        std::fs::write(&file, [42; 64]).unwrap();
        let archive = Archive::open(&root).unwrap();
        let observation = archive.inspect(&file, 64).unwrap();
        let database = turso::Builder::new_local(":memory:").build().await.unwrap();
        let connection = database.connect().unwrap();
        catalog::initialize_schema(&connection).await.unwrap();
        let identity = catalog::recording_file_identity(&file, &std::fs::metadata(&file).unwrap());
        connection.execute(
            "INSERT INTO recording_files (id, stream_id, source_id, logical_stream_id, started_at_ms, ended_at_ms,
                path, init_offset, init_len, finalized, file_bytes, file_identity)
             VALUES ('recording', 'front/sub', 'front', 'sub', 1000, 2000, ?1, 0, 8, 1, 64, ?2)",
            turso::params![file.to_str().unwrap(), identity],
        ).await.unwrap();
        let deadline = Instant::now() + BUSY_TIMEOUT;
        let mut inputs = super::super::read_rows(&connection, deadline)
            .await
            .unwrap();
        let expected = inputs.rows.remove(0);
        let index = Index {
            initialization: mp4::Mp4ByteRange {
                offset: 0,
                size: 64,
            },
            fragments: Vec::new(),
        };
        std::fs::rename(&file, root.join("original.mp4")).unwrap();
        std::fs::write(&file, [24; 64]).unwrap();
        let evidence = Evidence {
            archive: archive.try_clone().unwrap(),
            observation,
        };
        let result = apply(
            &connection,
            &expected,
            inputs.revision,
            index,
            evidence,
            deadline,
        )
        .await;
        let mut rows = connection
            .query(
                "SELECT init_len FROM recording_files WHERE id = 'recording'",
                (),
            )
            .await
            .unwrap();
        let init_len = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
        drop(rows);
        drop(archive);
        let remaining = std::fs::read(&file).unwrap();
        std::fs::remove_dir_all(root).unwrap();

        assert!(result.is_err());
        assert_eq!(init_len, 8);
        assert_eq!(remaining, [24; 64]);
    });
}
