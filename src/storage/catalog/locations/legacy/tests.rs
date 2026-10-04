use super::*;

fn paths() -> LegacyPaths {
    let root =
        std::env::temp_dir().join(format!("keeppeek-legacy-offline-{}", uuid::Uuid::new_v4()));
    LegacyPaths {
        active_root: root.join("media"),
        archive_root: root.join("media"),
        export_root: root.join("media/.exports"),
        thumbnail_root: root.join("thumbnails"),
        catalog_path: root.join("metadata/custom.db"),
        export_history_path: root.join("media/.exports/history.json"),
    }
}

#[test]
fn first_snapshot_survives_changed_defaults_without_creating_offline_roots() -> anyhow::Result<()> {
    pollster::block_on(async {
        let database = turso::Builder::new_local(":memory:").build().await?;
        let connection = database.connect()?;
        initialize(&connection).await?;
        assert_eq!(load(&connection).await?, None);
        let original = paths();
        assert_eq!(register(&connection, &original).await?, original);
        let changed = paths();
        assert_eq!(register(&connection, &changed).await?, original);
        assert!(!original.active_root.exists());
        assert!(!original.catalog_path.exists());
        assert!(!changed.active_root.exists());
        drop(connection);
        let connection = database.connect()?;
        initialize(&connection).await?;
        assert_eq!(load(&connection).await?, Some(original));
        assert!(
            connection
                .execute("DELETE FROM storage_legacy_paths", ())
                .await
                .is_err()
        );
        assert!(
            connection
                .execute("UPDATE storage_legacy_paths SET snapshot='{}'", ())
                .await
                .is_err()
        );
        anyhow::Ok(())
    })
}

#[test]
fn invalid_paths_cannot_register_and_unknown_snapshot_fields_are_rejected() -> anyhow::Result<()> {
    pollster::block_on(async {
        let database = turso::Builder::new_local(":memory:").build().await?;
        let connection = database.connect()?;
        initialize(&connection).await?;
        for invalid in [
            PathBuf::from("relative"),
            std::env::temp_dir().join("../escape"),
            std::env::temp_dir().join("x".repeat(4097)),
        ] {
            let mut snapshot = paths();
            snapshot.archive_root = invalid;
            assert!(register(&connection, &snapshot).await.is_err());
        }
        assert_eq!(load(&connection).await?, None);
        let mut value = serde_json::to_value(paths())?;
        value
            .as_object_mut()
            .unwrap()
            .insert("unknown".into(), true.into());
        connection
            .execute(
                "INSERT INTO storage_legacy_paths VALUES(1,?1)",
                [value.to_string()],
            )
            .await?;
        assert!(load(&connection).await.is_err());
        anyhow::Ok(())
    })
}
