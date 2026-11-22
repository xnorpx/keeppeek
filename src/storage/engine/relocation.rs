//! Recover split-root recording moves from the finalized source file itself.
use super::WriterWorker;
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
        self.due.get_or_insert(now + RETRY_DELAY);
    }

    fn next(&mut self, source: &Path, destination: &Path) -> io::Result<Next> {
        if self.directories.is_empty() {
            match fs::read_dir(source) {
                Ok(entries) => self.directories.push(entries),
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Next::Done),
                Err(e) => return Err(e),
            }
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

    /// At most one file and a bounded directory page between incoming media commands.
    pub(super) fn retry_relocations(&mut self, now: Instant) -> Option<Duration> {
        let due = self.relocation_scan.due?;
        if now < due {
            return Some(due - now);
        }
        let page = self
            .relocation_scan
            .next(&self.config.medium_term_path, &self.config.long_term_path);
        match page {
            Ok(Next::File(stream, path)) => {
                if let Err(error) = self.retry_file(&stream, &path) {
                    self.relocation_scan.fail(&stream, now);
                    self.health.relocation_failed(&stream);
                    tracing::warn!(camera = stream, %error, "recording relocation retry deferred");
                }
                self.relocation_scan.due = Some(now + PAGE_DELAY);
            }
            Ok(Next::More) => self.relocation_scan.due = Some(now + PAGE_DELAY),
            Ok(Next::Done) => {
                if !self.relocation_scan.incomplete {
                    self.health
                        .finish_relocation_scan(&self.relocation_scan.failed);
                }
                self.relocation_scan.due = (self.relocation_scan.incomplete
                    || !self.relocation_scan.failed.is_empty())
                .then_some(now + RETRY_DELAY);
                self.relocation_scan.failed.clear();
                self.relocation_scan.incomplete = false;
            }
            Err(error) => {
                self.relocation_scan.incomplete = true;
                self.relocation_scan.due = Some(now + RETRY_DELAY);
                tracing::debug!(%error, "recording relocation directory retry deferred");
            }
        }
        self.relocation_scan
            .due
            .map(|at| at.saturating_duration_since(now))
    }

    fn retry_file(&self, stream: &str, path: &Path) -> io::Result<()> {
        // The writer joins the logical stream key (such as "front/main") in one step.
        // Rebuild that spelling after a directory walk: Windows enumeration returns
        // backslashes for both components, while the catalog path is a unique TEXT key.
        let stream_root = self.config.medium_term_path.join(stream);
        let tail = path.strip_prefix(&stream_root).map_err(io::Error::other)?;
        let path = stream_root.join(tail);
        let destination = self.config.long_term_path.join(stream).join(tail);
        let id = if let Some(catalog) = &self.catalog {
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
        self.move_to_long_term(stream, &path, &id).map(|_| ())
    }
}

/// Keep the published source readable until its catalog pointer has switched.
pub(super) fn stage_source(source: &Path, destination: &Path) -> io::Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::other("missing recording directory"))?;
    fs::create_dir_all(parent)?;
    match fs::hard_link(source, destination) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return verify_existing(source, destination);
        }
        Err(_) => {}
    }
    let directory = cap_std::fs::Dir::open_ambient_dir(parent, cap_std::ambient_authority())?;
    let temporary = format!(".keeppeek-{}.partial", uuid::Uuid::new_v4());
    let mut options = cap_std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt;
        use windows::Win32::{Foundation::GENERIC_WRITE, Storage::FileSystem::DELETE};
        options.access_mode(GENERIC_WRITE.0 | DELETE.0);
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
            removal::unix::rename_to(&directory, temporary.as_ref(), &directory, name)
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
    match copied {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => verify_existing(source, destination),
        other => other,
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
