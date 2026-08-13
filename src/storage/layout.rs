use std::path::{Path, PathBuf};
use time::OffsetDateTime;

pub fn camera_dir(root: &Path, camera_id: &str) -> PathBuf {
    root.join(camera_id)
}

pub fn date_dir(root: &Path, camera_id: &str, ts: OffsetDateTime) -> PathBuf {
    let date = format!("{:04}-{:02}-{:02}", ts.year(), ts.month() as u8, ts.day());
    camera_dir(root, camera_id).join(date)
}

pub fn hour_dir(root: &Path, camera_id: &str, ts: OffsetDateTime) -> PathBuf {
    let hour = format!("{:02}", ts.hour());
    date_dir(root, camera_id, ts).join(hour)
}

pub fn segment_path(root: &Path, camera_id: &str, ts: OffsetDateTime, extension: &str) -> PathBuf {
    let filename = format!(
        "{:02}{:02}{:03}.{}",
        ts.minute(),
        ts.second(),
        ts.millisecond(),
        extension
    );
    hour_dir(root, camera_id, ts).join(filename)
}

pub fn active_segment_path(
    root: &Path,
    camera_id: &str,
    ts: OffsetDateTime,
    extension: &str,
) -> PathBuf {
    let filename = format!(
        "{:02}{:02}{:03}.{}.active",
        ts.minute(),
        ts.second(),
        ts.millisecond(),
        extension
    );
    hour_dir(root, camera_id, ts).join(filename)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn segment_path_format() {
        let root = Path::new("/recordings");
        let ts = datetime!(2026-02-17 14:35:09.123 UTC);
        let path = segment_path(root, "front_door", ts, "mp4");
        assert_eq!(
            path,
            PathBuf::from("/recordings/front_door/2026-02-17/14/3509123.mp4")
        );
    }

    #[test]
    fn active_segment_path_format() {
        let root = Path::new("/recordings");
        let ts = datetime!(2026-02-17 14:35:09.123 UTC);
        let path = active_segment_path(root, "front_door", ts, "mp4");
        assert_eq!(
            path,
            PathBuf::from("/recordings/front_door/2026-02-17/14/3509123.mp4.active")
        );
    }
}
