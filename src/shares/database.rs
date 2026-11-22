use super::{model::*, record::Record};
use crate::storage::volumes::root::Root;
use cap_fs_ext::{FollowSymlinks, MetadataExt, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use std::{
    cell::Cell,
    fs::File,
    path::{Path, PathBuf},
};
use subtle::ConstantTimeEq;
use uuid::Uuid;

const MAX_DB_BYTES: u64 = 64 * 1_024 * 1_024;
const MAX_WAL_BYTES: u64 = 80 * 1_024 * 1_024;

pub(super) struct Database {
    connection: turso::Connection,
    root: Root,
    directory: Dir,
    _owner: File,
    high_water_ms: Cell<i64>,
    poisoned: Cell<bool>,
    #[cfg(test)]
    fail_commit: Cell<bool>,
    #[cfg(test)]
    fail_checkpoint: Cell<bool>,
}

impl Database {
    pub fn open(path: &Path, now_ms: i64) -> Result<Self, Error> {
        if now_ms < 0 {
            return Err(Error::Invalid);
        }
        let (path, root, directory, owner) = ownership(path)?;
        let path = path.to_str().ok_or(Error::Storage)?;
        let database = pollster::block_on(turso::Builder::new_local(path).build())
            .map_err(|_| Error::Storage)?;
        let connection = database.connect().map_err(|_| Error::Storage)?;
        let store = Self {
            connection,
            root,
            directory,
            _owner: owner,
            high_water_ms: Cell::new(0),
            poisoned: Cell::new(false),
            #[cfg(test)]
            fail_commit: Cell::new(false),
            #[cfg(test)]
            fail_checkpoint: Cell::new(false),
        };
        store
            .root
            .matches_directory(&store.directory)
            .map_err(|_| Error::Storage)?;
        store.limits()?;
        store.initialize(now_ms)?;
        store.root.revalidate().map_err(|_| Error::Storage)?;
        Ok(store)
    }

    fn initialize(&self, now_ms: i64) -> Result<(), Error> {
        let version = pollster::block_on(async {
            let mut rows = self.connection.query("PRAGMA user_version", ()).await?;
            rows.next()
                .await?
                .ok_or(turso::Error::QueryReturnedNoRows)?
                .get::<i64>(0)
        })
        .map_err(|_| Error::Storage)?;
        if version == 0 {
            self.connection_batch("BEGIN IMMEDIATE")?;
            let schema = self.connection_batch(
                "CREATE TABLE guest_meta(id INTEGER PRIMARY KEY CHECK(id=1), clock_ms INTEGER NOT NULL CHECK(clock_ms>=0));
                 INSERT INTO guest_meta VALUES(1,0);
                 CREATE TABLE guest_grants(
                    id TEXT PRIMARY KEY, token_hash BLOB NOT NULL UNIQUE CHECK(length(token_hash)=32),
                    creator TEXT NOT NULL, expires_ms INTEGER NOT NULL, ended_ms INTEGER,
                    payload BLOB NOT NULL CHECK(length(payload)<=4096));
                 PRAGMA user_version=1;");
            self.complete(schema)?;
        } else if version != 1 {
            return Err(Error::Storage);
        }
        let persisted = pollster::block_on(async {
            let mut rows = self
                .connection
                .query("SELECT clock_ms FROM guest_meta WHERE id=1", ())
                .await?;
            let row = rows
                .next()
                .await?
                .ok_or(turso::Error::QueryReturnedNoRows)?;
            row.get::<i64>(0)
        })
        .map_err(|_| Error::Storage)?;
        if persisted < 0 {
            return Err(Error::Storage);
        }
        self.high_water_ms.set(persisted);
        if now_ms < persisted {
            return Err(Error::Denied);
        }
        let invalid = self.scalar("SELECT COUNT(*) FROM guest_grants WHERE length(token_hash)!=32 OR length(id)>36 OR length(creator)>256 OR length(payload)>4096")?;
        if invalid != 0 {
            return Err(Error::Storage);
        }
        if self.scalar("SELECT COUNT(*) FROM guest_meta")? != 1 {
            return Err(Error::Storage);
        }
        let records = self.all()?;
        if records.len() > MAX_GRANT_RECORDS {
            return Err(Error::Storage);
        }
        validate_restored(&records, persisted)?;
        self.observe(now_ms)
    }

    pub const fn high_water_ms(&self) -> i64 {
        self.high_water_ms.get()
    }

    pub const fn healthy(&self) -> Result<(), Error> {
        if self.poisoned.get() {
            Err(Error::Storage)
        } else {
            Ok(())
        }
    }

    pub fn observe(&self, now_ms: i64) -> Result<(), Error> {
        self.healthy()?;
        if now_ms < self.high_water_ms.get() {
            return Err(Error::Denied);
        }
        if now_ms > self.high_water_ms.get() {
            self.root
                .matches_directory(&self.directory)
                .map_err(|_| Error::Storage)?;
            let changed = pollster::block_on(self.connection.execute(
                "UPDATE guest_meta SET clock_ms=?1 WHERE id=1",
                turso::params![now_ms],
            ))
            .map_err(|_| Error::Storage)?;
            if changed != 1 {
                self.poisoned.set(true);
                return Err(Error::Storage);
            }
            if self.checkpoint().is_err() {
                self.poisoned.set(true);
                return Err(Error::Storage);
            }
            self.high_water_ms.set(now_ms);
        }
        Ok(())
    }

    pub fn transaction<T>(
        &self,
        now_ms: i64,
        change: impl FnOnce(&Self) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.healthy()?;
        self.root
            .matches_directory(&self.directory)
            .map_err(|_| Error::Storage)?;
        self.connection_batch("BEGIN IMMEDIATE")?;
        let result = change(self).and_then(|value| {
            let changed = pollster::block_on(self.connection.execute(
                "UPDATE guest_meta SET clock_ms=?1 WHERE id=1",
                turso::params![now_ms],
            ))
            .map_err(|_| Error::Storage)?;
            if changed != 1 {
                self.poisoned.set(true);
                return Err(Error::Storage);
            }
            Ok(value)
        });
        let result = self.complete(result)?;
        self.high_water_ms.set(now_ms);
        Ok(result)
    }

    fn complete<T>(&self, result: Result<T, Error>) -> Result<T, Error> {
        let result = result.and_then(|value| {
            #[cfg(test)]
            if self.fail_commit.replace(false) {
                return Err(Error::Storage);
            }
            if self.connection_batch("COMMIT").is_err() {
                self.poisoned.set(true);
                return Err(Error::Storage);
            }
            if self.checkpoint().is_err() {
                self.poisoned.set(true);
                return Err(Error::Storage);
            }
            Ok(value)
        });
        if result.is_err() && self.connection_batch("ROLLBACK").is_err() {
            self.poisoned.set(true);
            return Err(Error::Storage);
        }
        result
    }

    fn limits(&self) -> Result<(), Error> {
        let page_size =
            u64::try_from(self.scalar("PRAGMA page_size")?).map_err(|_| Error::Storage)?;
        if !(512..=65_536).contains(&page_size) || !page_size.is_power_of_two() {
            return Err(Error::Storage);
        }
        let maximum = MAX_DB_BYTES / page_size;
        let actual = self.scalar(&format!("PRAGMA max_page_count={maximum}"))?;
        if u64::try_from(actual).map_err(|_| Error::Storage)? != maximum {
            return Err(Error::Storage);
        }
        Ok(())
    }

    fn scalar(&self, sql: &str) -> Result<i64, Error> {
        pollster::block_on(async {
            let mut rows = self.connection.query(sql, ()).await?;
            rows.next()
                .await?
                .ok_or(turso::Error::QueryReturnedNoRows)?
                .get::<i64>(0)
        })
        .map_err(|_| Error::Storage)
    }
    pub fn get(&self, id: Uuid) -> Result<Option<Record>, Error> {
        self.healthy()?;
        pollster::block_on(async {
            let mut rows = self
                .connection
                .query(
                    "SELECT CASE WHEN length(id)<=36 THEN id ELSE NULL END,CASE WHEN length(creator)<=256 THEN creator ELSE NULL END,expires_ms,ended_ms,length(payload),CASE WHEN length(payload)<=4096 THEN payload ELSE NULL END FROM guest_grants WHERE id=?1",
                    turso::params![id.to_string()],
                )
                .await
                .map_err(|_| Error::Storage)?;
            rows.next()
                .await
                .map_err(|_| Error::Storage)?
                .map(|row| decode(&row))
                .transpose()
        })
    }

    pub fn by_hash(&self, hash: &[u8; 32]) -> Result<Option<Record>, Error> {
        self.healthy()?;
        pollster::block_on(async {
            let mut rows = self.connection.query("SELECT CASE WHEN length(id)<=36 THEN id ELSE NULL END,CASE WHEN length(creator)<=256 THEN creator ELSE NULL END,expires_ms,ended_ms,length(payload),CASE WHEN length(payload)<=4096 THEN payload ELSE NULL END,token_hash FROM guest_grants WHERE token_hash=?1",
                turso::params![hash.as_slice()]).await.map_err(|_| Error::Storage)?;
            let Some(row) = rows.next().await.map_err(|_| Error::Storage)? else {
                return Ok(None);
            };
            let stored: [u8; 32] = row
                .get::<Vec<u8>>(6)
                .map_err(|_| Error::Storage)?
                .try_into()
                .map_err(|_| Error::Storage)?;
            if !bool::from(stored.ct_eq(hash)) {
                return Err(Error::Denied);
            }
            decode(&row).map(Some)
        })
    }

    pub fn all(&self) -> Result<Vec<Record>, Error> {
        self.healthy()?;
        pollster::block_on(async {
            let mut rows = self.connection.query("SELECT CASE WHEN length(id)<=36 THEN id ELSE NULL END,CASE WHEN length(creator)<=256 THEN creator ELSE NULL END,expires_ms,ended_ms,length(payload),CASE WHEN length(payload)<=4096 THEN payload ELSE NULL END FROM guest_grants ORDER BY id LIMIT 4097", ())
                .await.map_err(|_| Error::Storage)?;
            let mut records = Vec::new();
            while let Some(row) = rows.next().await.map_err(|_| Error::Storage)? {
                if records.len() == MAX_GRANT_RECORDS {
                    return Err(Error::Storage);
                }
                records.push(decode(&row)?);
            }
            Ok(records)
        })
    }

    pub fn insert(&self, record: &Record, hash: &[u8; 32]) -> Result<(), Error> {
        let payload = encode(record)?;
        pollster::block_on(self.connection.execute(
            "INSERT INTO guest_grants(id,token_hash,creator,expires_ms,ended_ms,payload) VALUES(?1,?2,?3,?4,?5,?6)",
            turso::params![record.intent.request_id.to_string(), hash.as_slice(), record.intent.issuer.creator_id(),
                record.intent.expires_at_ms, record.revoked_at_ms, payload])).map_err(|_| Error::Storage)?;
        Ok(())
    }

    pub fn save(&self, record: &Record) -> Result<(), Error> {
        let payload = encode(record)?;
        let changed = pollster::block_on(self.connection.execute(
            "UPDATE guest_grants SET ended_ms=?1,payload=?2 WHERE id=?3",
            turso::params![
                    record
                        .revoked_at_ms
                        .map(|time| time.min(record.intent.expires_at_ms)),
                    payload,
                    record.intent.request_id.to_string()
                ],
        ))
        .map_err(|_| Error::Storage)?;
        if changed == 1 {
            Ok(())
        } else {
            Err(Error::Storage)
        }
    }

    pub fn prune(&self, now_ms: i64) -> Result<u64, Error> {
        let cutoff = now_ms.saturating_sub(AUDIT_RETENTION_MS);
        pollster::block_on(self.connection.execute(
            "DELETE FROM guest_grants WHERE id IN (SELECT id FROM guest_grants
             WHERE COALESCE(ended_ms,expires_ms)<=?1 ORDER BY COALESCE(ended_ms,expires_ms),id LIMIT 256)",
            turso::params![cutoff])).map_err(|_| Error::Storage)
    }

    fn connection_batch(&self, sql: &str) -> Result<(), Error> {
        pollster::block_on(self.connection.execute_batch(sql)).map_err(|_| Error::Storage)
    }

    #[cfg(test)]
    pub fn fail_next_commit(&self) {
        self.fail_commit.set(true);
    }

    #[cfg(test)]
    pub fn fail_next_checkpoint(&self) {
        self.fail_checkpoint.set(true);
    }

    pub fn checkpoint(&self) -> Result<(), Error> {
        #[cfg(test)]
        if self.fail_checkpoint.replace(false) {
            return Err(Error::Storage);
        }
        pollster::block_on(async {
            let mut rows = self
                .connection
                .query("PRAGMA wal_checkpoint(TRUNCATE)", ())
                .await
                .map_err(|_| Error::Storage)?;
            let row = rows
                .next()
                .await
                .map_err(|_| Error::Storage)?
                .ok_or(Error::Storage)?;
            if row.get::<i64>(0).map_err(|_| Error::Storage)? != 0 {
                return Err(Error::Storage);
            }
            if rows.next().await.map_err(|_| Error::Storage)?.is_some() {
                return Err(Error::Storage);
            }
            Ok(())
        })
    }
}

fn encode(record: &Record) -> Result<Vec<u8>, Error> {
    record.validate()?;
    let payload = serde_json::to_vec(record).map_err(|_| Error::Storage)?;
    if payload.len() <= MAX_RECORD_BYTES {
        Ok(payload)
    } else {
        Err(Error::Invalid)
    }
}

fn decode(row: &turso::Row) -> Result<Record, Error> {
    let length = row.get::<i64>(4).map_err(|_| Error::Storage)?;
    if length < 0 || length > i64::try_from(MAX_RECORD_BYTES).map_err(|_| Error::Storage)? {
        return Err(Error::Storage);
    }
    let payload = row.get::<Vec<u8>>(5).map_err(|_| Error::Storage)?;
    if payload.len() > MAX_RECORD_BYTES {
        return Err(Error::Storage);
    }
    let record: Record = serde_json::from_slice(&payload).map_err(|_| Error::Storage)?;
    record.validate()?;
    if row.get::<String>(0).map_err(|_| Error::Storage)? != record.intent.request_id.to_string()
        || row.get::<String>(1).map_err(|_| Error::Storage)? != record.intent.issuer.creator_id()
        || row.get::<i64>(2).map_err(|_| Error::Storage)? != record.intent.expires_at_ms
        || row.get::<Option<i64>>(3).map_err(|_| Error::Storage)?
            != record
                .revoked_at_ms
                .map(|time| time.min(record.intent.expires_at_ms))
    {
        return Err(Error::Storage);
    }
    Ok(record)
}
fn ownership(path: &Path) -> Result<(PathBuf, Root, Dir, File), Error> {
    let requested = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(Error::Invalid)?;
    let parent = std::path::absolute(requested).map_err(|_| Error::Storage)?;
    let ancestor =
        Root::open(parent.parent().ok_or(Error::Invalid)?).map_err(|_| Error::Storage)?;
    let child = parent
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(Error::Invalid)?;
    let root = ancestor
        .create_private_child(child)
        .map_err(|_| Error::Storage)?;
    let directory =
        Dir::open_ambient_dir(&parent, cap_std::ambient_authority()).map_err(|_| Error::Storage)?;
    root.matches_directory(&directory)
        .map_err(|_| Error::Storage)?;
    let name = path.file_name().ok_or(Error::Invalid)?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut lock_name = name.to_os_string();
    lock_name.push(".owner.lock");
    let owner = directory
        .open_with(&lock_name, &options)
        .map_err(|_| Error::Storage)?;
    regular(&owner, 0)?;
    let owner = owner.into_std();
    owner.try_lock().map_err(|_| Error::Storage)?;
    let database = directory
        .open_with(name, &options)
        .map_err(|_| Error::Storage)?;
    regular(&database, MAX_DB_BYTES)?;
    drop(database);
    journals(&directory, name)?;
    root.revalidate().map_err(|_| Error::Storage)?;
    Ok((parent.join(name), root, directory, owner))
}

fn journals(directory: &Dir, name: &std::ffi::OsStr) -> Result<(), Error> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    for suffix in ["-wal", "-shm"] {
        let mut journal = name.to_os_string();
        journal.push(suffix);
        match directory.open_with(&journal, &options) {
            Ok(file) => regular(&file, MAX_WAL_BYTES)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(Error::Storage),
        }
    }
    Ok(())
}
fn regular(file: &cap_std::fs::File, limit: u64) -> Result<(), Error> {
    let metadata = file.metadata().map_err(|_| Error::Storage)?;
    if metadata.is_file()
        && !metadata.file_type().is_symlink()
        && metadata.nlink() == 1
        && metadata.len() <= limit
    {
        Ok(())
    } else {
        Err(Error::Storage)
    }
}

fn validate_restored(records: &[Record], floor_ms: i64) -> Result<(), Error> {
    let mut active = 0usize;
    let mut creators = std::collections::HashMap::new();
    for record in records {
        if record.created_at_ms > floor_ms
            || record.revoked_at_ms.is_some_and(|time| time > floor_ms)
            || record.last_access_at_ms.is_some_and(|time| time > floor_ms)
        {
            return Err(Error::Storage);
        }
        if record.active(floor_ms) {
            active += 1;
            let count = creators
                .entry(record.intent.issuer.creator_id())
                .or_insert(0usize);
            *count += 1;
            if active > MAX_ACTIVE_GRANTS || *count > MAX_CREATOR_GRANTS {
                return Err(Error::Storage);
            }
        }
    }
    Ok(())
}
