use super::super::tests::fixture as test_root;
use super::*;

const HISTORY_TEST_KEY: &str = "exports-12345678-1234-4234-8234-123456789abc.json";

#[test]
fn history_replacement_preserves_old_open_file_and_reopens_complete_new_bytes() -> anyhow::Result<()>
{
    let (path, root) = test_root()?;
    let original = b"{\"version\":1,\"jobs\":[]}\n";
    let replacement = b"{\n  \"jobs\": [],\n  \"version\": 1\n}\n";
    std::fs::write(path.join(HISTORY_TEST_KEY), original)?;
    assert_eq!(root.read_history(HISTORY_TEST_KEY)?, original);
    let mut prior = std::fs::File::open(path.join(HISTORY_TEST_KEY))?;
    root.replace_history(HISTORY_TEST_KEY, replacement)?;
    let mut retained = Vec::new();
    std::io::Read::read_to_end(&mut prior, &mut retained)?;
    assert_eq!(retained, original);
    assert_eq!(root.read_history(HISTORY_TEST_KEY)?, replacement);
    drop(prior);
    drop(root);
    let reopened = Root::open(&path)?;
    assert_eq!(reopened.read_history(HISTORY_TEST_KEY)?, replacement);
    assert_eq!(std::fs::read(path.join(HISTORY_TEST_KEY))?, replacement);
    Ok(())
}

#[test]
fn history_operations_refuse_missing_leaf_and_replaced_root_without_recreating_them()
-> anyhow::Result<()> {
    let (path, root) = test_root()?;
    assert!(root.read_history(HISTORY_TEST_KEY).is_err());
    assert!(root.replace_history(HISTORY_TEST_KEY, b"new").is_err());
    assert!(!path.join(HISTORY_TEST_KEY).exists());
    std::fs::write(path.join(HISTORY_TEST_KEY), b"original")?;
    let retained = path.with_extension("retained");
    #[cfg(windows)]
    let identity = root.identity().clone();
    #[cfg(windows)]
    drop(root);
    std::fs::rename(&path, &retained)?;
    // Windows prevents the rename while the directory is pinned. Restore the stale observation.
    #[cfg(windows)]
    let root = Root {
        path: path.clone(),
        directory: cap_std::fs::Dir::open_ambient_dir(&retained, cap_std::ambient_authority())?,
        identity,
    };
    assert!(root.read_history(HISTORY_TEST_KEY).is_err());
    assert!(root.replace_history(HISTORY_TEST_KEY, b"new").is_err());
    assert!(!path.exists());
    std::fs::create_dir(&path)?;
    std::fs::write(path.join(HISTORY_TEST_KEY), b"unrelated replacement")?;
    assert!(root.read_history(HISTORY_TEST_KEY).is_err());
    assert!(root.replace_history(HISTORY_TEST_KEY, b"new").is_err());
    assert_eq!(
        std::fs::read(path.join(HISTORY_TEST_KEY))?,
        b"unrelated replacement"
    );
    assert_eq!(std::fs::read(retained.join(HISTORY_TEST_KEY))?, b"original");
    Ok(())
}

#[test]
fn history_operations_reject_traversal_noncanonical_names_and_hardlinks() -> anyhow::Result<()> {
    let (path, root) = test_root()?;
    let outside_key = format!("exports-{}.json", uuid::Uuid::new_v4());
    let outside = path.parent().unwrap().join(&outside_key);
    std::fs::write(&outside, b"unrelated outside history")?;
    for key in [
        format!("../{outside_key}"),
        format!("..\\{outside_key}"),
        "exports-12345678123442348234123456789abc.json".into(),
        "exports-12345678-1234-4234-8234-123456789ABC.json".into(),
        "history.json".into(),
    ] {
        assert!(root.read_history(&key).is_err());
        assert!(root.replace_history(&key, b"new").is_err());
    }
    assert_eq!(std::fs::read(&outside)?, b"unrelated outside history");
    std::fs::hard_link(&outside, path.join(HISTORY_TEST_KEY))?;
    assert!(root.read_history(HISTORY_TEST_KEY).is_err());
    assert!(root.replace_history(HISTORY_TEST_KEY, b"new").is_err());
    assert_eq!(std::fs::read(&outside)?, b"unrelated outside history");
    assert_eq!(
        std::fs::read(path.join(HISTORY_TEST_KEY))?,
        b"unrelated outside history"
    );
    Ok(())
}

#[test]
fn history_read_and_replacement_reject_more_than_eight_mebibytes_without_mutation()
-> anyhow::Result<()> {
    let (path, root) = test_root()?;
    let history = path.join(HISTORY_TEST_KEY);
    std::fs::write(&history, b"original")?;
    let oversized = vec![b' '; 8 * 1024 * 1024 + 1];
    assert!(root.replace_history(HISTORY_TEST_KEY, &oversized).is_err());
    assert_eq!(std::fs::read(&history)?, b"original");
    assert_eq!(root.read_history(HISTORY_TEST_KEY)?, b"original");
    std::fs::write(&history, &oversized)?;
    assert!(root.read_history(HISTORY_TEST_KEY).is_err());
    assert_eq!(std::fs::read(&history)?, oversized);
    Ok(())
}
