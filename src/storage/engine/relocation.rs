//! Recover split-root recording moves from the finalized source file itself.
use super::{RecordingCatalogHandle, RecordingHealthRegistry, StorageConfig, WriterWorker};
use crate::storage::catalog::relocation::Move;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::{
    collections::HashSet,
    fs::{self, File, ReadDir},
    io::{self, Read},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

// A failed storage mount must not cause I/O or a growing queue per incoming frame.
const RETRY_DELAY: Duration = Duration::from_secs(30);
const PAGE_DELAY: Duration = Duration::from_millis(25);
const DIRECTORY_ENTRIES_PER_PAGE: usize = 32;

#[derive(Default)]
pub(super) struct Scan {
    due: Option<Instant>,
    directories: Vec<ReadDir>,
    failed: HashSet<String>,
    incomplete: bool,
    journal_cursor: String,
    journal_done: bool,
    published: bool,
    pass_finished: bool,
}

impl Scan {
    pub(super) fn new(distinct_roots: bool) -> Self {
        Self {
            due: distinct_roots.then(Instant::now),
            ..Self::default()
        }
    }

    fn fail(&mut self, stream: &str, now: Instant) {
        self.failed.insert(stream.to_owned());
        self.due
            .get_or_insert(now.max(Instant::now()) + RETRY_DELAY);
    }

    fn next(&mut self, source: &Path, destination: &Path) -> io::Result<Next> {
        if self.directories.is_empty() {
            self.directories.push(fs::read_dir(source)?);
        }
        for _ in 0..DIRECTORY_ENTRIES_PER_PAGE {
            let Some(entries) = self.directories.last_mut() else {
                return Ok(Next::Done);
            };
            let Some(entry) = entries.next() else {
                self.directories.pop();
                if self.directories.is_empty() {
                    return Ok(Next::Done);
                }
                continue;
            };
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_dir() && path != destination {
                self.directories.push(fs::read_dir(path)?);
            } else if kind.is_file()
                && let Some(stream) = legacy_stream(source, &path)
            {
                return Ok(Next::File(stream, path));
            }
        }
        Ok(Next::More)
    }
}

enum Next {
    More,
    Done,
    File(String, PathBuf),
}

fn legacy_stream(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let name = relative.file_name()?.to_str()?.strip_suffix(".mp4")?;
    let hour = relative.parent()?;
    let date = hour.parent()?;
    let stream = date.parent()?;
    let digits = |text: &str, len| text.len() == len && text.bytes().all(|b| b.is_ascii_digit());
    let day = date.file_name()?.to_str()?;
    if !digits(name, 7)
        || !digits(hour.file_name()?.to_str()?, 2)
        || day.len() != 10
        || !day.is_ascii()
        || !digits(&day[..4], 4)
        || !digits(&day[5..7], 2)
        || !digits(&day[8..], 2)
        || &day[4..5] != "-"
        || &day[7..8] != "-"
        || stream.as_os_str().is_empty()
    {
        return None;
    }
    Some(
        stream
            .iter()
            .map(|s| s.to_str())
            .collect::<Option<Vec<_>>>()?
            .join("/"),
    )
}

impl WriterWorker {
    pub(super) fn finish_relocation(&mut self, stream: &str, path: &Path, id: &str) {
        if let Err(error) = self.move_to_long_term(stream, path, id) {
            if self.config.medium_term_path != self.config.long_term_path
                && legacy_stream(&self.config.medium_term_path, path).is_some()
            {
                self.health.relocation_failed(stream);
                self.relocation_scan.fail(stream, Instant::now());
            } else {
                self.health.note_failure(stream, &error.to_string());
            }
            tracing::error!(camera = stream, %id, %error, "finalized recording relocation pending");
        }
    }

    #[cfg(test)]
    pub(super) fn retry_relocations(&mut self, now: Instant) -> Option<Duration> {
        self.relocation_scan
            .step(&self.config, self.catalog.as_ref(), &self.health, now)
    }
}

pub(super) fn legacy_path(config: &StorageConfig, path: &Path) -> bool {
    config.medium_term_path != config.long_term_path
        && legacy_stream(&config.medium_term_path, path).is_some()
}

pub(super) struct Background {
    sender: Option<mpsc::SyncSender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    published: Arc<AtomicBool>,
}

impl Background {
    pub(super) fn start(
        config: StorageConfig,
        catalog: Option<RecordingCatalogHandle>,
        health: RecordingHealthRegistry,
    ) -> Self {
        let (sender, receiver) = mpsc::sync_channel(1);
        let published = Arc::new(AtomicBool::new(false));
        let updated = published.clone();
        let thread = std::thread::Builder::new()
            .name("recording-relocation".into())
            .spawn(move || run_background(receiver, &config, catalog.as_ref(), &health, &updated))
            .expect("failed to spawn recording relocation worker");
        Self {
            sender: Some(sender),
            thread: Some(thread),
            published,
        }
    }

    pub(super) fn wake(&self) {
        if let Some(sender) = &self.sender {
            let _ = sender.try_send(());
        }
    }

    pub(super) fn take_published(&self) -> bool {
        self.published.swap(false, Ordering::AcqRel)
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_background(
    rx: mpsc::Receiver<()>,
    config: &StorageConfig,
    catalog: Option<&RecordingCatalogHandle>,
    health: &RecordingHealthRegistry,
    published: &AtomicBool,
) {
    let mut scan = Scan::new(true);
    let mut dirty = false;
    let mut stopping = false;
    loop {
        let delay = scan.step(config, catalog, health, Instant::now());
        if scan.published {
            published.store(true, Ordering::Release);
            scan.published = false;
        }
        // New finalizations may be in a directory already visited. Coalesce all of them
        // into one further complete pass, and finish the last pass on orderly shutdown.
        let complete = scan.pass_finished;
        if complete && dirty {
            dirty = false;
            scan.due = Some(Instant::now());
            continue;
        }
        if stopping {
            if complete {
                break;
            }
            scan.due = Some(Instant::now());
            continue;
        }
        let received = match delay {
            None => rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
            Some(delay) => rx.recv_timeout(delay),
        };
        match received {
            Ok(()) if complete => scan.due = Some(Instant::now()),
            Ok(()) => dirty = true,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                stopping = true;
                dirty = true;
                scan.due = Some(Instant::now());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

impl Scan {
    /// Visit one journal record or one source-directory page per background turn.
    fn step(
        &mut self,
        config: &StorageConfig,
        catalog: Option<&RecordingCatalogHandle>,
        health: &RecordingHealthRegistry,
        now: Instant,
    ) -> Option<Duration> {
        let due = self.due?;
        if now < due {
            return Some(due - now);
        }
        self.pass_finished = false;
        if !self.journal_done
            && let Some(catalog) = catalog
        {
            match catalog.next_relocation(&self.journal_cursor) {
                Ok(Some(job)) => {
                    self.journal_cursor = job.id.clone();
                    if let Err(error) = recover_job(config, catalog, &job) {
                        self.fail(&job.stream, now);
                        health.relocation_failed(&job.stream);
                        tracing::debug!(%error,"recording relocation journal retry deferred");
                    }
                    self.due = Some(now.max(Instant::now()) + PAGE_DELAY);
                    return Some(PAGE_DELAY);
                }
                Ok(None) => self.journal_done = true,
                Err(error) => {
                    tracing::debug!(%error,"recording relocation journal unavailable");
                    self.incomplete = true;
                    self.pass_finished = true;
                    self.due = Some(now.max(Instant::now()) + RETRY_DELAY);
                    return Some(RETRY_DELAY);
                }
            }
        }
        let page = self.next(&config.medium_term_path, &config.long_term_path);
        match page {
            Ok(Next::File(stream, path)) => {
                if let Err(error) = retry_file(config, catalog, &stream, &path) {
                    self.fail(&stream, now);
                    health.relocation_failed(&stream);
                    tracing::warn!(camera = stream, %error, "recording relocation retry deferred");
                } else {
                    self.published = true;
                }
                self.due = Some(now.max(Instant::now()) + PAGE_DELAY);
            }
            Ok(Next::More) => self.due = Some(now + PAGE_DELAY),
            Ok(Next::Done) => {
                self.pass_finished = true;
                if !self.incomplete {
                    health.finish_relocation_scan(&self.failed);
                }
                self.due = (self.incomplete || !self.failed.is_empty())
                    .then_some(now.max(Instant::now()) + RETRY_DELAY);
                self.failed.clear();
                self.journal_cursor.clear();
                self.journal_done = false;
                self.incomplete = false;
            }
            Err(error) => {
                self.pass_finished = true;
                self.incomplete = true;
                self.due = Some(now.max(Instant::now()) + RETRY_DELAY);
                tracing::debug!(%error, "recording relocation directory retry deferred");
            }
        }
        self.due
            .map(|at| at.saturating_duration_since(now.max(Instant::now())))
    }
}

fn retry_file(
    config: &StorageConfig,
    catalog: Option<&RecordingCatalogHandle>,
    stream: &str,
    path: &Path,
) -> io::Result<()> {
    // The writer joins the logical stream key (such as "front/main") in one step.
    // Rebuild that spelling after a directory walk: Windows enumeration returns
    // backslashes for both components, while the catalog path is a unique TEXT key.
    let stream_root = config.medium_term_path.join(stream);
    let tail = path.strip_prefix(&stream_root).map_err(io::Error::other)?;
    let path = stream_root.join(tail);
    let destination = config.long_term_path.join(stream).join(tail);
    let id = if let Some(catalog) = catalog {
        let Some((id, catalog_stream)) = catalog
            .finalized_relocation(&path, &destination)
            .map_err(io::Error::other)?
        else {
            return Ok(());
        };
        if catalog_stream != stream {
            return Err(io::Error::other(
                "recording stream does not match legacy path",
            ));
        }
        id
    } else {
        String::new()
    };
    move_recording(config, catalog, stream, &path, &id).map(|_| ())
}

pub(super) fn move_recording(
    config: &StorageConfig,
    catalog: Option<&RecordingCatalogHandle>,
    camera_id: &str,
    path: &Path,
    recording_id: &str,
) -> io::Result<PathBuf> {
    if config.volume_runtime.is_some() {
        use crate::storage::catalog::locations::{Kind, Object, Reply, Request};
        if let Some(catalog) = catalog
            && matches!(
                catalog
                    .volume_location(Request::Lookup(Object {
                        kind: Kind::Recording,
                        id: recording_id.to_owned()
                    }))
                    .map_err(std::io::Error::other)?,
                Reply::Location(Some(_))
            )
        {
            if let Some(mover) = &config.volume_mover {
                mover.scan().map_err(std::io::Error::other)?;
            }
            if let Some(archive) = &config.archive_runtime
                && let Err(error) = archive.scan()
            {
                tracing::warn!(%error,"archive wakeup deferred; finalized recording remains available locally");
            }
            return Ok(path.to_path_buf());
        }
    }
    if config.medium_term_path == config.long_term_path {
        return Ok(path.to_owned());
    }
    let relative = path
        .strip_prefix(&config.medium_term_path)
        .map_err(io::Error::other)?;
    let destination = config.long_term_path.join(relative);
    if same_entry(path, &destination)? {
        return Ok(path.to_owned());
    }
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::other("missing recording directory"))?;
    fs::create_dir_all(parent)?;
    if let Some(catalog) = catalog {
        if catalog
            .reader_leases()
            .conflicts(recording_id, &path.to_string_lossy())
            .map_err(io::Error::other)?
        {
            return Err(io::Error::other(
                "active recording reader defers relocation",
            ));
        }
        if destination.try_exists()? {
            verify_existing(path, &destination)?;
        }
        let metadata = fs::symlink_metadata(path)?;
        let identity = crate::storage::catalog::recording_file_identity(path, &metadata);
        let job = catalog
            .reserve_relocation(Move {
                id: recording_id.to_owned(),
                stream: camera_id.to_owned(),
                source: path.to_owned(),
                destination: destination.clone(),
                staged: uuid::Uuid::new_v4(),
                destination_published: false,
                source_identity: identity
                    .ok_or_else(|| io::Error::other("recording identity unavailable"))?,
            })
            .map_err(io::Error::other)?;
        recover_job(config, catalog, &job)?;
    } else {
        // The camera diagnostic runner has no catalog or retention writer. Derive the stage
        // from the immutable finalized source so a restart can reclaim its own incomplete copy.
        use sha2::{Digest, Sha256};
        let identity =
            crate::storage::catalog::recording_file_identity(path, &fs::symlink_metadata(path)?)
                .ok_or_else(|| io::Error::other("recording identity unavailable"))?;
        let digest = Sha256::digest(identity.as_bytes());
        let stage =
            uuid::Uuid::from_bytes(digest[..16].try_into().expect("SHA-256 contains 16 bytes"));
        stage_source(path, &destination, stage)?;
        fs::remove_file(path)?;
    }
    Ok(destination)
}

fn recover_job(
    config: &StorageConfig,
    catalog: &RecordingCatalogHandle,
    job: &Move,
) -> io::Result<()> {
    let tail = job
        .source
        .strip_prefix(&config.medium_term_path)
        .map_err(io::Error::other)?;
    if config.long_term_path.join(tail) != job.destination
        || legacy_stream(&config.medium_term_path, &job.source).as_deref() != Some(&job.stream)
    {
        return Err(io::Error::other(
            "recording relocation no longer matches configured roots",
        ));
    }
    let staged = job.destination.with_file_name(staged_name(job.staged));
    // The UUID was reserved by the catalog before even one media byte could be written.
    remove_stage(&staged)?;
    if !job.source.try_exists()? {
        if !job.destination_published && job.destination.try_exists()? {
            return Err(io::Error::other(
                "source disappeared before recording handoff",
            ));
        }
        return catalog.retire_relocation(job).map_err(io::Error::other);
    }
    let meta = fs::symlink_metadata(&job.source)?;
    if !meta.is_file()
        || crate::storage::catalog::recording_file_identity(&job.source, &meta).as_deref()
            != Some(&job.source_identity)
    {
        return Err(io::Error::other(
            "recording source changed during relocation",
        ));
    }
    if same_entry(&job.source, &job.destination)? {
        return catalog.retire_relocation(job).map_err(io::Error::other);
    }
    if catalog
        .reader_leases()
        .conflicts(&job.id, &job.source.to_string_lossy())
        .map_err(io::Error::other)?
    {
        return Err(io::Error::other(
            "active recording reader defers relocation",
        ));
    }
    stage_source(&job.source, &job.destination, job.staged)?;
    sync_parent(&job.destination)?;
    catalog
        .update_recording_path(&job.id, &job.destination, true)
        .map_err(io::Error::other)?;
    if same_entry(&job.source, &job.destination)? {
        return Err(io::Error::other(
            "recording directories became aliases during publication",
        ));
    }
    fs::remove_file(&job.source)?;
    sync_parent(&job.source)?;
    catalog.retire_relocation(job).map_err(io::Error::other)
}

fn sync_parent(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing recording directory"))?;
    let directory = cap_std::fs::Dir::open_ambient_dir(parent, cap_std::ambient_authority())?;
    crate::storage::long_term::inspection::removal::sync_directory(&directory)
}

fn staged_name(id: uuid::Uuid) -> String {
    format!(".keeppeek-{id}.partial")
}

fn remove_stage(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => fs::remove_file(path),
        Ok(_) => Err(io::Error::other("recording stage is not a regular file")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Two spellings of the same parent may name the only directory entry.
pub(super) fn same_entry(source: &Path, destination: &Path) -> io::Result<bool> {
    if source.file_name() != destination.file_name() {
        return Ok(false);
    }
    let (Some(first), Some(second)) = (source.parent(), destination.parent()) else {
        return Err(io::Error::other("missing recording parent"));
    };
    match second.canonicalize() {
        Ok(second) => Ok(first.canonicalize()? == second),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Keep the published source readable until its catalog pointer has switched.
pub(super) fn stage_source(source: &Path, destination: &Path, stage: uuid::Uuid) -> io::Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::other("missing recording directory"))?;
    fs::create_dir_all(parent)?;
    remove_stage(&parent.join(staged_name(stage)))?;
    match fs::hard_link(source, destination) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return verify_existing(source, destination);
        }
        Err(_) => {}
    }
    copy_and_publish(source, destination, parent, stage)
}

fn copy_and_publish(
    source: &Path,
    destination: &Path,
    parent: &Path,
    stage: uuid::Uuid,
) -> io::Result<()> {
    let directory = cap_std::fs::Dir::open_ambient_dir(parent, cap_std::ambient_authority())?;
    let temporary = staged_name(stage);
    let mut options = cap_std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt;
        use windows::Win32::{Foundation::GENERIC_WRITE, Storage::FileSystem::DELETE};
        options.access_mode(GENERIC_WRITE.0 | DELETE.0);
    }
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        use std::os::unix::fs::PermissionsExt;
        options.mode(fs::metadata(source)?.permissions().mode() & 0o600);
    }
    let mut staged = directory.open_with(&temporary, &options)?;
    let copied = (|| {
        io::copy(&mut File::open(source)?, &mut staged)?;
        staged.sync_all()?;
        let name = destination
            .file_name()
            .ok_or_else(|| io::Error::other("missing recording name"))?;
        use crate::storage::long_term::inspection::removal;
        #[cfg(windows)]
        {
            removal::windows::rename_to(&staged, &directory, name)
        }
        #[cfg(unix)]
        {
            publish_no_replace(&directory, temporary.as_ref(), name, || {
                removal::unix::rename_to(&directory, temporary.as_ref(), &directory, name)
            })
        }
        #[cfg(not(any(windows, unix)))]
        {
            Err(io::Error::other(
                "recording publication is unsupported on this platform",
            ))
        }
    })();
    drop(staged);
    if copied.is_err() {
        let _ = directory.remove_file(&temporary);
    }
    // Windows can report a failed no-replace handle rename as a generic I/O
    // error. The destination itself decides whether a previous publication can
    // be reused; its complete contents must agree with the retained source.
    match copied {
        Err(_) if destination.try_exists()? => verify_existing(source, destination),
        other => other,
    }
}

#[cfg(any(unix, test))]
fn publish_no_replace(
    directory: &cap_std::fs::Dir,
    temporary: &std::ffi::OsStr,
    name: &std::ffi::OsStr,
    rename: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    match rename() {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(error),
        Err(_) => {
            directory.hard_link(temporary, directory, name)?;
            directory.remove_file(temporary)
        }
    }
}

fn verify_existing(source: &Path, destination: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(destination)?;
    let mut left = File::open(source)?;
    if !metadata.is_file() || metadata.len() != left.metadata()?.len() {
        return Err(io::Error::other(
            "conflicting finalized recording destination",
        ));
    }
    let mut right = File::open(destination)?;
    let mut left_buf = [0; 8192];
    let mut right_buf = [0; 8192];
    loop {
        let count = left.read(&mut left_buf)?;
        if count == 0 {
            return Ok(());
        }
        right.read_exact(&mut right_buf[..count])?;
        if left_buf[..count] != right_buf[..count] {
            return Err(io::Error::other("conflicting finalized recording contents"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_worker_coalesces_capture_wakeups_without_waiting_for_the_copy() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            gate.recv().unwrap();
            assert_eq!(receiver.into_iter().count(), 1);
        });
        let worker = Background {
            sender: Some(sender),
            thread: Some(thread),
            published: Arc::new(AtomicBool::new(false)),
        };
        // The service worker is held until all notifications from the capture side returned.
        for _ in 0..4096 {
            worker.wake();
        }
        release.send(()).unwrap();
        drop(worker);
    }

    #[test]
    fn parent_aliases_keep_the_single_directory_entry() {
        let root = std::env::temp_dir().join(format!("keeppeek-alias-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("sub")).unwrap();
        let source = root.join("file.mp4");
        fs::write(&source, b"recording").unwrap();
        let alias = root.join("sub").join("..").join("file.mp4");
        assert!(same_entry(&source, &alias).unwrap());
        let other = root.join("sub/file.mp4");
        fs::hard_link(&source, &other).unwrap();
        assert!(!same_entry(&source, &other).unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unsupported_rename_publishes_by_link_without_replacing_existing_bytes() {
        let root = std::env::temp_dir().join(format!("keeppeek-fallback-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let directory =
            cap_std::fs::Dir::open_ambient_dir(&root, cap_std::ambient_authority()).unwrap();
        let publish = || {
            publish_no_replace(&directory, "stage".as_ref(), "object.mp4".as_ref(), || {
                Err(io::ErrorKind::Unsupported.into())
            })
        };
        fs::write(root.join("stage"), b"first").unwrap();
        publish().unwrap();
        assert!(!root.join("stage").exists());
        fs::write(root.join("stage"), b"second").unwrap();
        assert!(publish().is_err());
        assert_eq!(fs::read(root.join("object.mp4")).unwrap(), b"first");
        drop(directory);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cross_filesystem_copy_publishes_exact_bytes_and_preserves_a_conflicting_destination() {
        let root = std::env::temp_dir().join(format!("keeppeek-relocate-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let source = root.join("source.mp4");
        let destination = root.join("destination.mp4");
        fs::write(&source, b"first recording").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
        }
        copy_and_publish(&source, &destination, &root, uuid::Uuid::new_v4()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(fs::read(&source).unwrap(), b"first recording");
        assert_eq!(fs::read(&destination).unwrap(), b"first recording");
        copy_and_publish(&source, &destination, &root, uuid::Uuid::new_v4()).unwrap();
        fs::write(&source, b"later recording").unwrap();
        assert!(copy_and_publish(&source, &destination, &root, uuid::Uuid::new_v4()).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"first recording");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
        fs::remove_dir_all(root).unwrap();
    }
}
