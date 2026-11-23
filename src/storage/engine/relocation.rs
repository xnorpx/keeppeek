//! Recover split-root recording moves from the finalized source file itself.
use super::{RecordingCatalogHandle, RecordingHealthRegistry, StorageConfig, WriterWorker};
use crate::storage::long_term::legacy_stream;
mod background;
mod files;
mod handoff;
pub(super) use background::Background;
pub(super) use handoff::move_recording;
use handoff::recover_job;

use std::{
    collections::HashSet,
    fs::{self, ReadDir},
    io,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

// A failed storage mount must not cause I/O or a growing queue per incoming frame.
const RETRY_DELAY: Duration = Duration::from_secs(30);
const PAGE_DELAY: Duration = Duration::from_millis(25);
const DIRECTORY_ENTRIES_PER_PAGE: usize = 32;

#[derive(Default)]
pub(super) struct Scan {
    stop: Arc<AtomicBool>,
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

    pub(super) const fn enabled(&self) -> bool {
        self.due.is_some()
    }

    pub(super) fn take_published(&mut self) -> bool {
        std::mem::take(&mut self.published)
    }

    fn fail(&mut self, stream: &str, now: Instant) {
        self.failed.insert(stream.to_owned());
        self.due
            .get_or_insert_with(|| now.max(Instant::now()) + RETRY_DELAY);
    }

    fn next(&mut self, source: &Path, destination: &Path) -> io::Result<Next> {
        if source == destination {
            return Ok(Next::Done);
        }
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

impl WriterWorker {
    pub(super) fn finish_relocation(&mut self, stream: &str, path: &Path, id: &str) {
        if let Err(error) = self.move_to_long_term(stream, path, id) {
            if can_scan(&self.config, self.catalog.is_some()) && legacy_path(&self.config, path) {
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

pub(super) fn can_scan(config: &StorageConfig, has_catalog: bool) -> bool {
    // Without a catalog, a nested file can name both a capture and a published stream.
    // Only the writer knows the actual stream at finalization.
    has_catalog || !config.medium_term_path.starts_with(&config.long_term_path)
}

pub(super) fn legacy_path(config: &StorageConfig, path: &Path) -> bool {
    config.medium_term_path != config.long_term_path
        && legacy_stream(&config.medium_term_path, path).is_some()
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
                    if let Err(error) = recover_job(catalog, &job, &self.stop) {
                        self.fail(&job.stream, now);
                        health.relocation_failed(&job.stream);
                        tracing::debug!(%error,"recording relocation journal retry deferred");
                    } else {
                        self.published = true;
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
                match retry_file(config, catalog, &stream, &path, &self.stop) {
                    Ok(moved) => self.published |= moved,
                    Err(error) => {
                        self.fail(&stream, now);
                        health.relocation_failed(&stream);
                        tracing::warn!(camera = stream, %error, "recording relocation retry deferred");
                    }
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
    stop: &AtomicBool,
) -> io::Result<bool> {
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
            return Ok(false);
        };
        if catalog_stream != stream {
            if legacy_stream(&config.long_term_path, &path).as_deref()
                == Some(catalog_stream.as_str())
            {
                return Ok(false);
            }
            return Err(io::Error::other(
                "recording stream does not match legacy path",
            ));
        }
        id
    } else {
        String::new()
    };
    move_recording(config, catalog, stream, &path, &id, stop).map(|_| true)
}
