use super::state_store::{
    Error, ExpiredEntry, Invalid, MAX_ENTRIES_PER_NAMESPACE, MAX_NAMESPACES, MAX_TOTAL_VALUE_BYTES,
    MAX_VALUE_BYTES, StoredEntry, authorize_read, authorize_write, check_expected, ttl_expiry_ms,
    validate_key, validate_namespace, validate_schema,
};
use prost::Message as _;
use prost_types::Duration;
use std::path::Path;

const SCHEMA_VERSION: i64 = 1;

pub struct DurableStore {
    connection: turso::Connection,
    stored_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct NamespaceExport {
    pub namespace: String,
    pub revision: u64,
    pub entries: Vec<StoredEntry>,
}

impl DurableStore {
    pub fn open(path: &Path, now_ms: u64) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let path = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("state-store path is not valid UTF-8"))?;
        let database = pollster::block_on(turso::Builder::new_local(path).build())?;
        let connection = database.connect()?;
        pollster::block_on(initialize_schema(&connection))?;
        pollster::block_on(purge_expired(&connection, now_ms))?;
        let stored_bytes = pollster::block_on(stored_value_bytes(&connection))?;
        Ok(Self {
            connection,
            stored_bytes,
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "put mirrors the wire, auth, and time-injection inputs one-to-one"
    )]
    pub fn put(
        &mut self,
        namespace: &str,
        key: &str,
        schema: &str,
        value: Option<prost_types::Struct>,
        expected_revision: Option<u64>,
        ttl: Option<Duration>,
        owner_id: &str,
        writer_admin: bool,
        now_ms: u64,
    ) -> Result<StoredEntry, Error> {
        let layout = validate_namespace(namespace)?;
        validate_key(key)?;
        validate_schema(schema)?;
        let value = value.ok_or(Error::Invalid(Invalid::ValueMissing))?;
        if value.encoded_len() > MAX_VALUE_BYTES {
            return Err(Error::Invalid(Invalid::ValueTooLarge));
        }
        let expires_ms = ttl
            .as_ref()
            .map(|ttl| ttl_expiry_ms(ttl, now_ms))
            .transpose()?;
        authorize_write(&layout, owner_id, writer_admin)?;
        super::state_store_schema::validate_value(schema, &value)?;
        let value_bytes = encoded_bytes(&value)?;
        pollster::block_on(self.put_transaction(
            namespace,
            key,
            schema,
            &value,
            value_bytes,
            expected_revision,
            expires_ms,
            owner_id,
            now_ms,
        ))
        .map_err(storage_error)
    }

    pub fn get(
        &mut self,
        namespace: &str,
        key: &str,
        owner_id: &str,
        reader_admin: bool,
        now_ms: u64,
    ) -> Result<StoredEntry, Error> {
        let layout = validate_namespace(namespace)?;
        authorize_read(&layout, owner_id, reader_admin)?;
        validate_key(key)?;
        pollster::block_on(self.get_transaction(namespace, key, now_ms)).map_err(storage_error)
    }

    pub fn delete(
        &mut self,
        namespace: &str,
        key: &str,
        expected_revision: Option<u64>,
        owner_id: &str,
        writer_admin: bool,
        now_ms: u64,
    ) -> Result<u64, Error> {
        let layout = validate_namespace(namespace)?;
        validate_key(key)?;
        authorize_write(&layout, owner_id, writer_admin)?;
        pollster::block_on(self.delete_transaction(namespace, key, expected_revision, now_ms))
            .map_err(storage_error)
    }

    pub const fn stored_bytes(&self) -> u64 {
        self.stored_bytes
    }

    pub fn export(&self, now_ms: u64) -> Result<Vec<NamespaceExport>, Error> {
        pollster::block_on(self.export_all(now_ms)).map_err(storage_error)
    }

    pub(super) fn expire_due(&mut self, now_ms: u64) -> Result<Vec<ExpiredEntry>, Error> {
        pollster::block_on(self.expire_due_transaction(now_ms)).map_err(storage_error)
    }

    async fn export_all(&self, now_ms: u64) -> Result<Vec<NamespaceExport>, TransactionError> {
        let mut namespaces = Vec::new();
        let mut namespace_rows = self
            .connection
            .query(
                "SELECT namespace, revision FROM namespaces ORDER BY namespace",
                (),
            )
            .await?;
        while let Some(row) = namespace_rows.next().await? {
            namespaces.push((row.get::<String>(0)?, to_u64(row.get::<i64>(1)?)?));
        }
        let mut exports = Vec::new();
        for (namespace, revision) in namespaces {
            let mut keys = Vec::new();
            let mut key_rows = self
                .connection
                .query(
                    "SELECT key FROM entries WHERE namespace = ?1 ORDER BY key",
                    turso::params![namespace.as_str()],
                )
                .await?;
            while let Some(row) = key_rows.next().await? {
                keys.push(row.get::<String>(0)?);
            }
            let mut entries = Vec::new();
            for key in keys {
                let Some(entry) = self.read_entry(&namespace, &key).await? else {
                    continue;
                };
                if entry
                    .expires_ms
                    .is_some_and(|expires_ms| expires_ms <= now_ms)
                {
                    continue;
                }
                entries.push(entry);
            }
            exports.push(NamespaceExport {
                namespace,
                revision,
                entries,
            });
        }
        Ok(exports)
    }

    async fn expire_due_transaction(
        &mut self,
        now_ms: u64,
    ) -> Result<Vec<ExpiredEntry>, TransactionError> {
        self.connection.execute_batch("BEGIN IMMEDIATE").await?;
        let result = async {
            let mut namespaces = Vec::new();
            let mut namespace_rows = self
                .connection
                .query("SELECT namespace FROM namespaces ORDER BY namespace", ())
                .await?;
            while let Some(row) = namespace_rows.next().await? {
                namespaces.push(row.get::<String>(0)?);
            }
            let mut due = Vec::new();
            for namespace in namespaces {
                for key in self.expired_keys(&namespace, now_ms).await? {
                    due.push((namespace.clone(), key));
                }
            }
            due.sort_unstable();
            let mut expired = Vec::new();
            let mut freed_bytes = 0u64;
            for (namespace, key) in due {
                let Some(entry) = self.read_entry(&namespace, &key).await? else {
                    continue;
                };
                freed_bytes = freed_bytes.saturating_add(encoded_bytes(&entry.value)?);
                self.connection
                    .execute(
                        "DELETE FROM entries WHERE namespace = ?1 AND key = ?2",
                        turso::params![namespace.as_str(), key.as_str()],
                    )
                    .await?;
                let revision = self.bump_namespace_revision(&namespace).await?;
                expired.push(ExpiredEntry {
                    namespace,
                    key,
                    revision,
                    schema: entry.schema,
                    value: entry.value,
                    owner_id: entry.owner_id,
                    expires_ms: entry.expires_ms.unwrap_or(now_ms),
                });
            }
            Ok((expired, self.stored_bytes.saturating_sub(freed_bytes)))
        }
        .await;
        self.finish_transaction(result).await
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "transaction inputs mirror the validated put request one-to-one"
    )]
    async fn put_transaction(
        &mut self,
        namespace: &str,
        key: &str,
        schema: &str,
        value: &prost_types::Struct,
        value_bytes: u64,
        expected_revision: Option<u64>,
        expires_ms: Option<u64>,
        owner_id: &str,
        now_ms: u64,
    ) -> Result<StoredEntry, TransactionError> {
        self.connection.execute_batch("BEGIN IMMEDIATE").await?;
        let result = async {
            let mut freed_bytes = self.expire_key_if_due(namespace, key, now_ms).await?;
            let current = self.entry_revision(namespace, key).await?;
            check_expected(expected_revision, current)?;
            let old_bytes = self.entry_bytes(namespace, key).await?;
            if current.is_none() {
                freed_bytes =
                    freed_bytes.saturating_add(self.admit_namespace(namespace, now_ms).await?);
            }
            let total_bytes = self
                .stored_bytes
                .saturating_add(value_bytes)
                .saturating_sub(old_bytes)
                .saturating_sub(freed_bytes);
            if total_bytes > MAX_TOTAL_VALUE_BYTES {
                return Err(Error::Invalid(Invalid::StoreFull).into());
            }
            let revision = self
                .insert_entry(namespace, key, schema, value, expires_ms, owner_id, now_ms)
                .await?;
            Ok((
                StoredEntry {
                    namespace: namespace.to_owned(),
                    key: key.to_owned(),
                    schema: schema.to_owned(),
                    value: value.clone(),
                    revision,
                    updated_ms: now_ms,
                    expires_ms,
                    owner_id: owner_id.to_owned(),
                },
                total_bytes,
            ))
        }
        .await;
        self.finish_transaction(result).await
    }

    async fn get_transaction(
        &mut self,
        namespace: &str,
        key: &str,
        now_ms: u64,
    ) -> Result<StoredEntry, TransactionError> {
        self.connection.execute_batch("BEGIN IMMEDIATE").await?;
        let result = async {
            let freed_bytes = self.expire_key_if_due(namespace, key, now_ms).await?;
            let entry = self
                .read_entry(namespace, key)
                .await?
                .ok_or(Error::NotFound)
                .map_err(TransactionError::from)?;
            Ok((entry, self.stored_bytes.saturating_sub(freed_bytes)))
        }
        .await;
        self.finish_transaction(result).await
    }

    async fn delete_transaction(
        &mut self,
        namespace: &str,
        key: &str,
        expected_revision: Option<u64>,
        now_ms: u64,
    ) -> Result<u64, TransactionError> {
        self.connection.execute_batch("BEGIN IMMEDIATE").await?;
        let result = async {
            let freed_bytes = self.expire_key_if_due(namespace, key, now_ms).await?;
            let current = self.entry_revision(namespace, key).await?;
            check_expected(expected_revision, current)?;
            current.ok_or(Error::NotFound)?;
            let removed_bytes = self.entry_bytes(namespace, key).await?;
            self.connection
                .execute(
                    "DELETE FROM entries WHERE namespace = ?1 AND key = ?2",
                    turso::params![namespace, key],
                )
                .await?;
            let revision = self.bump_namespace_revision(namespace).await?;
            let total_bytes = self
                .stored_bytes
                .saturating_sub(freed_bytes)
                .saturating_sub(removed_bytes);
            Ok((revision, total_bytes))
        }
        .await;
        self.finish_transaction(result).await
    }

    async fn finish_transaction<T>(
        &mut self,
        result: Result<(T, u64), TransactionError>,
    ) -> Result<T, TransactionError> {
        match result {
            Ok((value, staged_bytes)) => {
                if let Err(error) = self.connection.execute_batch("COMMIT").await {
                    let _ = self.connection.execute_batch("ROLLBACK").await;
                    return Err(TransactionError::Storage(error.to_string()));
                }
                self.stored_bytes = staged_bytes;
                Ok(value)
            }
            Err(error) => {
                let _ = self.connection.execute_batch("ROLLBACK").await;
                Err(error)
            }
        }
    }

    async fn expire_key_if_due(
        &self,
        namespace: &str,
        key: &str,
        now_ms: u64,
    ) -> Result<u64, TransactionError> {
        let expired = self.entry_expires_ms(namespace, key).await?;
        if expired.is_some_and(|expires_ms| expires_ms <= now_ms) {
            let removed_bytes = self.entry_bytes(namespace, key).await?;
            self.connection
                .execute(
                    "DELETE FROM entries WHERE namespace = ?1 AND key = ?2",
                    turso::params![namespace, key],
                )
                .await?;
            self.bump_namespace_revision(namespace).await?;
            return Ok(removed_bytes);
        }
        Ok(0)
    }

    async fn admit_namespace(&self, namespace: &str, now_ms: u64) -> Result<u64, TransactionError> {
        if self.namespace_revision(namespace).await?.is_none() {
            let count = self.namespace_count().await?;
            if count >= MAX_NAMESPACES as u64 {
                return Err(Error::Invalid(Invalid::StoreFull).into());
            }
            self.connection
                .execute(
                    "INSERT INTO namespaces(namespace, revision) VALUES(?1, 0)",
                    turso::params![namespace],
                )
                .await?;
            return Ok(0);
        }
        let freed_bytes = self.reclaim_namespace(namespace, now_ms).await?;
        if self.entry_count(namespace).await? >= MAX_ENTRIES_PER_NAMESPACE as u64 {
            return Err(Error::Invalid(Invalid::NamespaceFull).into());
        }
        Ok(freed_bytes)
    }

    async fn reclaim_namespace(
        &self,
        namespace: &str,
        now_ms: u64,
    ) -> Result<u64, TransactionError> {
        let expired = self.expired_keys(namespace, now_ms).await?;
        let mut freed_bytes = 0u64;
        for key in expired {
            freed_bytes =
                freed_bytes.saturating_add(self.expire_key_if_due(namespace, &key, now_ms).await?);
        }
        Ok(freed_bytes)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "insert inputs mirror the validated put request one-to-one"
    )]
    async fn insert_entry(
        &self,
        namespace: &str,
        key: &str,
        schema: &str,
        value: &prost_types::Struct,
        expires_ms: Option<u64>,
        owner_id: &str,
        now_ms: u64,
    ) -> Result<u64, TransactionError> {
        let revision = self.bump_namespace_revision(namespace).await?;
        self.connection
            .execute(
                "INSERT OR REPLACE INTO entries(namespace, key, schema, value, revision, updated_ms, expires_ms, owner_id)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                turso::params![
                    namespace,
                    key,
                    schema,
                    value.encode_to_vec(),
                    to_i64(revision)?,
                    to_i64(now_ms)?,
                    expires_ms.map(to_i64).transpose()?,
                    owner_id
                ],
            )
            .await?;
        Ok(revision)
    }

    async fn bump_namespace_revision(&self, namespace: &str) -> Result<u64, TransactionError> {
        let revision = self
            .namespace_revision(namespace)
            .await?
            .expect("namespace row must exist for mutation")
            .checked_add(1)
            .expect("namespace revision overflow");
        self.connection
            .execute(
                "UPDATE namespaces SET revision = ?1 WHERE namespace = ?2",
                turso::params![to_i64(revision)?, namespace],
            )
            .await?;
        Ok(revision)
    }

    async fn namespace_revision(&self, namespace: &str) -> Result<Option<u64>, TransactionError> {
        let mut rows = self
            .connection
            .query(
                "SELECT revision FROM namespaces WHERE namespace = ?1",
                turso::params![namespace],
            )
            .await?;
        if let Some(row) = rows.next().await? {
            Ok(Some(to_u64(row.get::<i64>(0)?)?))
        } else {
            Ok(None)
        }
    }

    async fn namespace_count(&self) -> Result<u64, TransactionError> {
        let mut rows = self
            .connection
            .query("SELECT COUNT(*) FROM namespaces", ())
            .await?;
        let row = rows.next().await?.expect("COUNT(*) always returns one row");
        to_u64(row.get::<i64>(0)?)
    }

    async fn entry_count(&self, namespace: &str) -> Result<u64, TransactionError> {
        let mut rows = self
            .connection
            .query(
                "SELECT COUNT(*) FROM entries WHERE namespace = ?1",
                turso::params![namespace],
            )
            .await?;
        let row = rows.next().await?.expect("COUNT(*) always returns one row");
        to_u64(row.get::<i64>(0)?)
    }

    async fn entry_revision(
        &self,
        namespace: &str,
        key: &str,
    ) -> Result<Option<u64>, TransactionError> {
        let mut rows = self
            .connection
            .query(
                "SELECT revision FROM entries WHERE namespace = ?1 AND key = ?2",
                turso::params![namespace, key],
            )
            .await?;
        if let Some(row) = rows.next().await? {
            Ok(Some(to_u64(row.get::<i64>(0)?)?))
        } else {
            Ok(None)
        }
    }

    async fn entry_bytes(&self, namespace: &str, key: &str) -> Result<u64, TransactionError> {
        let mut rows = self
            .connection
            .query(
                "SELECT LENGTH(value) FROM entries WHERE namespace = ?1 AND key = ?2",
                turso::params![namespace, key],
            )
            .await?;
        if let Some(row) = rows.next().await? {
            Ok(to_u64(row.get::<i64>(0)?)?)
        } else {
            Ok(0)
        }
    }

    async fn entry_expires_ms(
        &self,
        namespace: &str,
        key: &str,
    ) -> Result<Option<u64>, TransactionError> {
        let mut rows = self
            .connection
            .query(
                "SELECT expires_ms FROM entries WHERE namespace = ?1 AND key = ?2",
                turso::params![namespace, key],
            )
            .await?;
        if let Some(row) = rows.next().await? {
            row.get::<Option<i64>>(0)?.map(to_u64).transpose()
        } else {
            Ok(None)
        }
    }

    async fn expired_keys(
        &self,
        namespace: &str,
        now_ms: u64,
    ) -> Result<Vec<String>, TransactionError> {
        let mut rows = self
            .connection
            .query(
                "SELECT key FROM entries WHERE namespace = ?1 AND expires_ms IS NOT NULL AND expires_ms <= ?2",
                turso::params![namespace, to_i64(now_ms)?],
            )
            .await?;
        let mut keys = Vec::new();
        while let Some(row) = rows.next().await? {
            keys.push(row.get::<String>(0)?);
        }
        Ok(keys)
    }

    async fn read_entry(
        &self,
        namespace: &str,
        key: &str,
    ) -> Result<Option<StoredEntry>, TransactionError> {
        let mut rows = self
            .connection
            .query(
                "SELECT schema, value, revision, updated_ms, expires_ms, owner_id
                 FROM entries WHERE namespace = ?1 AND key = ?2",
                turso::params![namespace, key],
            )
            .await?;
        if let Some(row) = rows.next().await? {
            let bytes = row.get::<Vec<u8>>(1)?;
            let value = prost_types::Struct::decode(bytes.as_slice()).map_err(|error| {
                TransactionError::Storage(format!("stored state value is corrupt: {error}"))
            })?;
            Ok(Some(StoredEntry {
                namespace: namespace.to_owned(),
                key: key.to_owned(),
                schema: row.get::<String>(0)?,
                value,
                revision: to_u64(row.get::<i64>(2)?)?,
                updated_ms: to_u64(row.get::<i64>(3)?)?,
                expires_ms: row.get::<Option<i64>>(4)?.map(to_u64).transpose()?,
                owner_id: row.get::<String>(5)?,
            }))
        } else {
            Ok(None)
        }
    }
}

enum TransactionError {
    Domain(Error),
    Storage(String),
}

impl From<Error> for TransactionError {
    fn from(error: Error) -> Self {
        Self::Domain(error)
    }
}

impl From<turso::Error> for TransactionError {
    fn from(error: turso::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

fn storage_error(error: TransactionError) -> Error {
    match error {
        TransactionError::Domain(error) => error,
        TransactionError::Storage(message) => Error::Storage(message),
    }
}

fn to_i64(value: u64) -> Result<i64, TransactionError> {
    i64::try_from(value).map_err(|_| {
        TransactionError::Storage(format!("state-store integer {value} overflows i64"))
    })
}

fn to_u64(value: i64) -> Result<u64, TransactionError> {
    u64::try_from(value)
        .map_err(|_| TransactionError::Storage(format!("state-store integer {value} is negative")))
}

fn encoded_bytes(value: &prost_types::Struct) -> Result<u64, Error> {
    u64::try_from(value.encoded_len()).map_err(|_| Error::Invalid(Invalid::ValueTooLarge))
}

async fn initialize_schema(connection: &turso::Connection) -> anyhow::Result<()> {
    let version = schema_version(connection).await?;
    if version > SCHEMA_VERSION {
        anyhow::bail!("unsupported state-store schema version {version}");
    }
    if version == 0 {
        connection.execute_batch("BEGIN IMMEDIATE").await?;
        let created = connection
            .execute_batch(
                "CREATE TABLE namespaces(
                    namespace TEXT PRIMARY KEY,
                    revision INTEGER NOT NULL
                );
                CREATE TABLE entries(
                    namespace TEXT NOT NULL,
                    key TEXT NOT NULL,
                    schema TEXT NOT NULL,
                    value BLOB NOT NULL,
                    revision INTEGER NOT NULL,
                    updated_ms INTEGER NOT NULL,
                    expires_ms INTEGER,
                    owner_id TEXT NOT NULL,
                    PRIMARY KEY(namespace, key)
                );
                PRAGMA user_version = 1;",
            )
            .await;
        if let Err(error) = created {
            let _ = connection.execute_batch("ROLLBACK").await;
            return Err(error.into());
        }
        if let Err(error) = connection.execute_batch("COMMIT").await {
            let _ = connection.execute_batch("ROLLBACK").await;
            return Err(error.into());
        }
        return Ok(());
    }
    let mut rows = connection
        .query("SELECT 1 FROM namespaces LIMIT 1", ())
        .await
        .map_err(|error| anyhow::anyhow!("state-store schema is unrecognized: {error}"))?;
    let _ = rows.next().await?;
    Ok(())
}

async fn schema_version(connection: &turso::Connection) -> anyhow::Result<i64> {
    let mut rows = connection.query("PRAGMA user_version", ()).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("state-store version query returned no rows"))?;
    Ok(row.get::<i64>(0)?)
}

async fn purge_expired(connection: &turso::Connection, now_ms: u64) -> anyhow::Result<()> {
    let cutoff = to_i64(now_ms).map_err(|error| match error {
        TransactionError::Storage(message) => anyhow::anyhow!("{message}"),
        TransactionError::Domain(_) => anyhow::anyhow!("state-store clock is out of range"),
    })?;
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let purged: Result<(), anyhow::Error> = async {
        let mut rows = connection
            .query(
                "SELECT namespace, COUNT(*) FROM entries
                 WHERE expires_ms IS NOT NULL AND expires_ms <= ?1
                 GROUP BY namespace",
                turso::params![cutoff],
            )
            .await?;
        let mut expired = Vec::new();
        while let Some(row) = rows.next().await? {
            expired.push((row.get::<String>(0)?, row.get::<i64>(1)?));
        }
        drop(rows);
        let mut bumps = Vec::new();
        for (namespace, count) in expired {
            let mut current = connection
                .query(
                    "SELECT revision FROM namespaces WHERE namespace = ?1",
                    turso::params![namespace.as_str()],
                )
                .await?;
            let row = current
                .next()
                .await?
                .ok_or_else(|| anyhow::anyhow!("state-store namespace is missing"))?;
            let revision = row.get::<i64>(0)?;
            drop(current);
            let count = u64::try_from(count)
                .map_err(|_| anyhow::anyhow!("state-store expired count is out of range"))?;
            let next = to_u64(revision)
                .map_err(|_| anyhow::anyhow!("state-store revision is corrupt"))?
                .checked_add(count)
                .ok_or_else(|| anyhow::anyhow!("state-store revision overflows u64"))?;
            bumps.push((
                namespace,
                to_i64(next).map_err(|_| {
                    anyhow::anyhow!("state-store revision exceeds the persisted range")
                })?,
            ));
        }
        connection
            .execute(
                "DELETE FROM entries WHERE expires_ms IS NOT NULL AND expires_ms <= ?1",
                turso::params![cutoff],
            )
            .await?;
        for (namespace, revision) in bumps {
            connection
                .execute(
                    "UPDATE namespaces SET revision = ?1 WHERE namespace = ?2",
                    turso::params![revision, namespace],
                )
                .await?;
        }
        Ok(())
    }
    .await;
    if let Err(error) = purged {
        let _ = connection.execute_batch("ROLLBACK").await;
        return Err(error);
    }
    if let Err(error) = connection.execute_batch("COMMIT").await {
        let _ = connection.execute_batch("ROLLBACK").await;
        return Err(error.into());
    }
    Ok(())
}

async fn stored_value_bytes(connection: &turso::Connection) -> anyhow::Result<u64> {
    let mut rows = connection
        .query("SELECT COALESCE(SUM(LENGTH(value)), 0) FROM entries", ())
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("state-store size query returned no rows"))?;
    u64::try_from(row.get::<i64>(0)?).map_err(|error| anyhow::anyhow!("{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost_types::{Struct, Value, value::Kind};
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR_ID: AtomicU64 = AtomicU64::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let id = NEXT_DIR_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "keeppeek-state-store-test-{}-{}",
                std::process::id(),
                id
            ));
            std::fs::create_dir_all(&path).expect("test temp dir must be created");
            Self { path }
        }

        fn db_path(&self) -> PathBuf {
            self.path.join("state.db")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    const NOW_MS: u64 = 1_787_000_000_000;

    fn media_intent_value(role: &str) -> Struct {
        let mut fields = BTreeMap::from([
            (
                "role".to_owned(),
                Value {
                    kind: Some(Kind::StringValue(role.to_owned())),
                },
            ),
            (
                "source_id".to_owned(),
                Value {
                    kind: Some(Kind::StringValue("front-door".to_owned())),
                },
            ),
            (
                "media_kind".to_owned(),
                Value {
                    kind: Some(Kind::StringValue("video".to_owned())),
                },
            ),
            (
                "desired".to_owned(),
                Value {
                    kind: Some(Kind::BoolValue(true)),
                },
            ),
        ]);
        if role == "publish" {
            fields.insert(
                "recording_mode".to_owned(),
                Value {
                    kind: Some(Kind::StringValue("disabled".to_owned())),
                },
            );
        }
        Struct { fields }
    }

    fn duration_ms(millis: u64) -> Duration {
        Duration {
            seconds: (millis / 1_000) as i64,
            nanos: ((millis % 1_000) * 1_000_000) as i32,
        }
    }

    #[test]
    fn durable_put_get_roundtrip() {
        let dir = TempDir::new();
        let mut store = DurableStore::open(&dir.db_path(), NOW_MS).expect("open must succeed");
        let entry = store
            .put(
                "service/transcoder-a/",
                "intents/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                None,
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("put must succeed");
        assert_eq!(entry.revision, 1);
        assert_eq!(entry.owner_id, "transcoder-a");
        let read = store
            .get(
                "service/transcoder-a/",
                "intents/front-door",
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("get must succeed");
        assert_eq!(read, entry);
    }

    #[test]
    fn export_restores_revisions_and_skips_due_leases() {
        let dir = TempDir::new();
        let mut store = DurableStore::open(&dir.db_path(), NOW_MS).expect("open must succeed");
        let live = store
            .put(
                "service/transcoder-a/",
                "intents/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                None,
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("live put must succeed");
        store
            .put(
                "service/transcoder-a/",
                "leases/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                None,
                Some(duration_ms(1_000)),
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("lease put must succeed");
        let exports = store.export(NOW_MS + 5_000).expect("export must succeed");
        assert_eq!(exports.len(), 1);
        assert_eq!(exports[0].namespace, "service/transcoder-a/");
        assert_eq!(exports[0].revision, 2);
        assert_eq!(exports[0].entries, vec![live]);
    }

    #[test]
    fn durable_competing_cas_writers_have_exactly_one_winner() {
        let dir = TempDir::new();
        let mut store = DurableStore::open(&dir.db_path(), NOW_MS).expect("open must succeed");
        store
            .put(
                "service/transcoder-a/",
                "intents/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                None,
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("seed write must succeed");
        let first = store
            .put(
                "service/transcoder-a/",
                "intents/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("subscribe")),
                Some(1),
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("first compare-and-set must succeed");
        assert_eq!(first.revision, 2);
        let error = store
            .put(
                "service/transcoder-a/",
                "intents/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                Some(1),
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect_err("stale compare-and-set must fail");
        assert_eq!(
            error,
            Error::Conflict {
                current_revision: 2
            }
        );
        let read = store
            .get(
                "service/transcoder-a/",
                "intents/front-door",
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("get must succeed");
        assert_eq!(read.revision, 2);
    }

    #[test]
    fn durable_create_only_delete_and_missing_paths() {
        let dir = TempDir::new();
        let mut store = DurableStore::open(&dir.db_path(), NOW_MS).expect("open must succeed");
        store
            .put(
                "service/transcoder-a/",
                "intents/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                Some(0),
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("create-only write must succeed");
        let error = store
            .put(
                "service/transcoder-a/",
                "intents/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                Some(0),
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect_err("second create-only write must fail");
        assert_eq!(
            error,
            Error::Conflict {
                current_revision: 1
            }
        );
        let revision = store
            .delete(
                "service/transcoder-a/",
                "intents/front-door",
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("delete must succeed");
        assert_eq!(revision, 2);
        let error = store
            .delete(
                "service/transcoder-a/",
                "intents/front-door",
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect_err("second delete must fail");
        assert_eq!(error, Error::NotFound);
        let error = store
            .get(
                "service/transcoder-a/",
                "intents/front-door",
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect_err("deleted entries must read as not found");
        assert_eq!(error, Error::NotFound);
    }

    #[test]
    fn durable_restart_restores_entries_and_counters() {
        let dir = TempDir::new();
        let stored_before = {
            let mut store = DurableStore::open(&dir.db_path(), NOW_MS).expect("open must succeed");
            store
                .put(
                    "service/transcoder-a/",
                    "intents/front-door",
                    "keeppeek.media-intent.v1",
                    Some(media_intent_value("publish")),
                    None,
                    None,
                    "transcoder-a",
                    true,
                    NOW_MS,
                )
                .expect("put must succeed");
            store
                .put(
                    "user/viewer-a/",
                    "subscriptions/front-door",
                    "keeppeek.media-intent.v1",
                    Some(media_intent_value("subscribe")),
                    None,
                    Some(duration_ms(60_000)),
                    "viewer-a",
                    false,
                    NOW_MS,
                )
                .expect("lease must succeed");
            store.stored_bytes()
        };
        let mut store = DurableStore::open(&dir.db_path(), NOW_MS).expect("reopen must succeed");
        assert_eq!(
            store.stored_bytes(),
            stored_before,
            "byte counter must be recomputed on open"
        );
        let entry = store
            .get(
                "service/transcoder-a/",
                "intents/front-door",
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("entry must survive restart");
        assert_eq!(entry.revision, 1);
        assert_eq!(entry.owner_id, "transcoder-a");
        let lease = store
            .get(
                "user/viewer-a/",
                "subscriptions/front-door",
                "viewer-a",
                false,
                NOW_MS,
            )
            .expect("lease must survive restart");
        assert_eq!(lease.expires_ms, Some(NOW_MS + 60_000));
        let next = store
            .put(
                "service/transcoder-a/",
                "intents/back-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                None,
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("post-restart write must succeed");
        assert_eq!(next.revision, 2);
    }

    #[test]
    fn durable_counter_survives_empty_namespace() {
        let dir = TempDir::new();
        {
            let mut store = DurableStore::open(&dir.db_path(), NOW_MS).expect("open must succeed");
            store
                .put(
                    "service/transcoder-a/",
                    "intents/front-door",
                    "keeppeek.media-intent.v1",
                    Some(media_intent_value("publish")),
                    None,
                    None,
                    "transcoder-a",
                    true,
                    NOW_MS,
                )
                .expect("put must succeed");
            store
                .delete(
                    "service/transcoder-a/",
                    "intents/front-door",
                    None,
                    "transcoder-a",
                    true,
                    NOW_MS,
                )
                .expect("delete must succeed");
        }
        let mut store = DurableStore::open(&dir.db_path(), NOW_MS).expect("reopen must succeed");
        let next = store
            .put(
                "service/transcoder-a/",
                "intents/back-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                None,
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("post-restart write must succeed");
        assert_eq!(next.revision, 3);
    }

    #[test]
    fn durable_unknown_schema_version_is_unavailable() {
        let dir = TempDir::new();
        {
            let database = pollster::block_on(
                turso::Builder::new_local(dir.db_path().to_str().unwrap()).build(),
            )
            .expect("raw open must succeed");
            let connection = database.connect().expect("connect must succeed");
            pollster::block_on(connection.execute_batch("PRAGMA user_version = 999"))
                .expect("version bump must succeed");
        }
        let error = match DurableStore::open(&dir.db_path(), NOW_MS) {
            Ok(_) => panic!("future version must fail"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("unsupported"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn durable_namespace_bound_rejects_before_allocation() {
        let dir = TempDir::new();
        let mut store = DurableStore::open(&dir.db_path(), NOW_MS).expect("open must succeed");
        for index in 0..1_024 {
            store
                .put(
                    "service/transcoder-a/",
                    &format!("keys/k-{index:04}"),
                    "keeppeek.media-intent.v1",
                    Some(media_intent_value("publish")),
                    None,
                    None,
                    "transcoder-a",
                    true,
                    NOW_MS,
                )
                .expect("fill write must succeed");
        }
        let error = store
            .put(
                "service/transcoder-a/",
                "keys/overflow",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                None,
                None,
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect_err("1025th key must fail");
        assert_eq!(error, Error::Invalid(Invalid::NamespaceFull));
    }

    #[test]
    fn durable_expired_read_keeps_byte_accounting() {
        let dir = TempDir::new();
        let mut store = DurableStore::open(&dir.db_path(), NOW_MS).expect("open must succeed");
        store
            .put(
                "service/transcoder-a/",
                "intents/front-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                None,
                Some(duration_ms(1_000)),
                "transcoder-a",
                true,
                NOW_MS,
            )
            .expect("lease must succeed");
        let stored = store.stored_bytes();
        assert!(stored > 0, "seeded lease must occupy bytes");
        for now_ms in [NOW_MS + 1_000, NOW_MS + 2_000] {
            let error = store
                .get(
                    "service/transcoder-a/",
                    "intents/front-door",
                    "transcoder-a",
                    true,
                    now_ms,
                )
                .expect_err("expired lease must read as not found");
            assert_eq!(error, Error::NotFound);
            assert_eq!(
                store.stored_bytes(),
                stored,
                "rolled-back expiry must not move the byte counter"
            );
        }
        drop(store);
        let store = DurableStore::open(&dir.db_path(), NOW_MS).expect("reopen must succeed");
        assert_eq!(
            store.stored_bytes(),
            stored,
            "counter must match persisted rows after restart"
        );
    }

    #[test]
    fn durable_reopen_expires_overdue_leases() {
        let dir = TempDir::new();
        {
            let mut store = DurableStore::open(&dir.db_path(), NOW_MS).expect("open must succeed");
            store
                .put(
                    "service/transcoder-a/",
                    "intents/persistent",
                    "keeppeek.media-intent.v1",
                    Some(media_intent_value("publish")),
                    None,
                    None,
                    "transcoder-a",
                    true,
                    NOW_MS,
                )
                .expect("put must succeed");
            store
                .put(
                    "service/transcoder-a/",
                    "intents/short",
                    "keeppeek.media-intent.v1",
                    Some(media_intent_value("subscribe")),
                    None,
                    Some(duration_ms(1_000)),
                    "transcoder-a",
                    true,
                    NOW_MS,
                )
                .expect("lease must succeed");
            store
                .put(
                    "service/transcoder-a/",
                    "intents/long",
                    "keeppeek.media-intent.v1",
                    Some(media_intent_value("subscribe")),
                    None,
                    Some(duration_ms(60_000)),
                    "transcoder-a",
                    true,
                    NOW_MS,
                )
                .expect("lease must succeed");
        }
        let mut store =
            DurableStore::open(&dir.db_path(), NOW_MS + 5_000).expect("reopen must succeed");
        let expired = store
            .get(
                "service/transcoder-a/",
                "intents/short",
                "transcoder-a",
                true,
                NOW_MS + 5_000,
            )
            .expect_err("overdue lease must not survive restart");
        assert_eq!(expired, Error::NotFound);
        store
            .get(
                "service/transcoder-a/",
                "intents/persistent",
                "transcoder-a",
                true,
                NOW_MS + 5_000,
            )
            .expect("persistent entry must survive restart");
        store
            .get(
                "service/transcoder-a/",
                "intents/long",
                "transcoder-a",
                true,
                NOW_MS + 5_000,
            )
            .expect("live lease must survive restart");
        let next = store
            .put(
                "service/transcoder-a/",
                "intents/next",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                None,
                None,
                "transcoder-a",
                true,
                NOW_MS + 5_000,
            )
            .expect("put must succeed");
        assert_eq!(
            next.revision, 5,
            "startup purge bumps the counter like lazy expiry, so revisions stay monotonic"
        );
        for key in ["intents/persistent", "intents/long", "intents/next"] {
            store
                .delete(
                    "service/transcoder-a/",
                    key,
                    None,
                    "transcoder-a",
                    true,
                    NOW_MS + 5_000,
                )
                .expect("delete must succeed");
        }
        assert_eq!(
            store.stored_bytes(),
            0,
            "purged bytes must not linger in the counter"
        );
    }

    #[test]
    fn durable_reopen_with_all_expired_keeps_revision_monotonic() {
        let dir = TempDir::new();
        {
            let mut store = DurableStore::open(&dir.db_path(), NOW_MS).expect("open must succeed");
            for key in ["intents/a", "intents/b"] {
                store
                    .put(
                        "service/transcoder-a/",
                        key,
                        "keeppeek.media-intent.v1",
                        Some(media_intent_value("subscribe")),
                        None,
                        Some(duration_ms(1_000)),
                        "transcoder-a",
                        true,
                        NOW_MS,
                    )
                    .expect("lease must succeed");
            }
        }
        let mut store =
            DurableStore::open(&dir.db_path(), NOW_MS + 60_000).expect("reopen must succeed");
        assert_eq!(store.stored_bytes(), 0);
        let next = store
            .put(
                "service/transcoder-a/",
                "intents/next",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                None,
                None,
                "transcoder-a",
                true,
                NOW_MS + 60_000,
            )
            .expect("put must succeed");
        assert_eq!(
            next.revision, 5,
            "two purged leases must advance the counter by two"
        );
    }

    #[test]
    fn durable_reopen_counts_every_expired_entry_per_namespace() {
        let dir = TempDir::new();
        {
            let mut store = DurableStore::open(&dir.db_path(), NOW_MS).expect("open must succeed");
            for key in ["intents/a", "intents/b"] {
                store
                    .put(
                        "service/transcoder-a/",
                        key,
                        "keeppeek.media-intent.v1",
                        Some(media_intent_value("subscribe")),
                        None,
                        Some(duration_ms(1_000)),
                        "transcoder-a",
                        true,
                        NOW_MS,
                    )
                    .expect("lease must succeed");
            }
            store
                .put(
                    "service/transcoder-a/",
                    "intents/live",
                    "keeppeek.media-intent.v1",
                    Some(media_intent_value("publish")),
                    None,
                    None,
                    "transcoder-a",
                    true,
                    NOW_MS,
                )
                .expect("put must succeed");
            store
                .put(
                    "user/viewer-a/",
                    "subscriptions/front-door",
                    "keeppeek.media-intent.v1",
                    Some(media_intent_value("subscribe")),
                    None,
                    Some(duration_ms(60_000)),
                    "viewer-a",
                    false,
                    NOW_MS,
                )
                .expect("lease must succeed");
        }
        let mut store =
            DurableStore::open(&dir.db_path(), NOW_MS + 5_000).expect("reopen must succeed");
        for key in ["intents/a", "intents/b"] {
            let expired = store
                .get(
                    "service/transcoder-a/",
                    key,
                    "transcoder-a",
                    true,
                    NOW_MS + 5_000,
                )
                .expect_err("overdue leases must not survive restart");
            assert_eq!(expired, Error::NotFound);
        }
        let service_next = store
            .put(
                "service/transcoder-a/",
                "intents/next",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("publish")),
                None,
                None,
                "transcoder-a",
                true,
                NOW_MS + 5_000,
            )
            .expect("put must succeed");
        assert_eq!(
            service_next.revision, 6,
            "two expired entries must advance the counter by two, like lazy expiry"
        );
        let user_next = store
            .put(
                "user/viewer-a/",
                "subscriptions/back-door",
                "keeppeek.media-intent.v1",
                Some(media_intent_value("subscribe")),
                None,
                None,
                "viewer-a",
                false,
                NOW_MS + 5_000,
            )
            .expect("put must succeed");
        assert_eq!(
            user_next.revision, 2,
            "namespaces without expired entries must keep their counter"
        );
    }

    #[test]
    fn durable_reopen_rolls_back_purge_on_revision_overflow() {
        let dir = TempDir::new();
        {
            let database = pollster::block_on(
                turso::Builder::new_local(dir.db_path().to_str().unwrap()).build(),
            )
            .expect("raw open must succeed");
            let connection = database.connect().expect("connect must succeed");
            pollster::block_on(initialize_schema(&connection)).expect("schema must initialize");
            let value = media_intent_value("subscribe");
            pollster::block_on(connection.execute(
                "INSERT INTO namespaces(namespace, revision) VALUES(?1, ?2)",
                turso::params!["service/transcoder-a/", i64::MAX],
            ))
            .expect("namespace must be seeded");
            pollster::block_on(connection.execute(
                "INSERT INTO entries(namespace, key, schema, value, revision, updated_ms, expires_ms, owner_id)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                turso::params![
                    "service/transcoder-a/",
                    "intents/doomed",
                    "keeppeek.media-intent.v1",
                    prost::Message::encode_to_vec(&value),
                    1i64,
                    NOW_MS as i64,
                    NOW_MS as i64,
                    "transcoder-a",
                ],
            ))
            .expect("expired entry must be seeded");
        }
        let failed = match DurableStore::open(&dir.db_path(), NOW_MS + 60_000) {
            Ok(_) => panic!("purge past the persisted range must fail"),
            Err(error) => error,
        };
        assert!(
            failed.to_string().contains("exceeds the persisted range"),
            "unexpected error: {failed}"
        );
        let database =
            pollster::block_on(turso::Builder::new_local(dir.db_path().to_str().unwrap()).build())
                .expect("raw open must succeed");
        let connection = database.connect().expect("connect must succeed");
        let mut rows = pollster::block_on(connection.query(
            "SELECT revision FROM namespaces WHERE namespace = 'service/transcoder-a/'",
            (),
        ))
        .expect("revision must be readable");
        let row = pollster::block_on(rows.next())
            .expect("row read must succeed")
            .expect("namespace must still exist");
        assert_eq!(row.get::<i64>(0).expect("revision must read"), i64::MAX);
        drop(rows);
        let mut rows = pollster::block_on(connection.query(
            "SELECT key FROM entries WHERE namespace = 'service/transcoder-a/'",
            (),
        ))
        .expect("entries must be readable");
        let row = pollster::block_on(rows.next())
            .expect("row read must succeed")
            .expect("expired row must survive the rolled-back purge");
        assert_eq!(
            row.get::<String>(0).expect("key must read"),
            "intents/doomed"
        );
    }

    #[test]
    fn durable_failed_schema_init_leaves_no_partial_tables() {
        let dir = TempDir::new();
        {
            let database = pollster::block_on(
                turso::Builder::new_local(dir.db_path().to_str().unwrap()).build(),
            )
            .expect("raw open must succeed");
            let connection = database.connect().expect("connect must succeed");
            pollster::block_on(connection.execute_batch("CREATE TABLE entries(id INTEGER)"))
                .expect("obstacle table must be created");
            assert!(
                pollster::block_on(initialize_schema(&connection)).is_err(),
                "conflicting table must fail init"
            );
            let mut rows = pollster::block_on(connection.query(
                "SELECT name FROM sqlite_master WHERE name = 'namespaces'",
                (),
            ))
            .expect("schema query must succeed");
            assert!(
                pollster::block_on(rows.next())
                    .expect("row read must succeed")
                    .is_none(),
                "failed init must not leave the namespaces table behind"
            );
            drop(rows);
            pollster::block_on(connection.execute_batch("DROP TABLE entries"))
                .expect("obstacle cleanup must succeed");
        }
        let store =
            DurableStore::open(&dir.db_path(), NOW_MS).expect("open after cleanup must succeed");
        assert_eq!(store.stored_bytes(), 0);
    }
}
