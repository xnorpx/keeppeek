//! Durable volume bindings and allocation ownership, serialized by the catalog actor.

use super::{BUSY_TIMEOUT, Command, RecordingCatalogHandle, to_i64, to_u64};
use std::{
    fmt,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

pub mod archives;
mod growth;
mod materialization;
pub mod moves;
pub use materialization::Materialization;
mod ownership;
pub use ownership::{Location, Publication};

const MAX_BINDINGS: i64 = 37;
const MAX_PENDING_ALLOCATIONS: i64 = 4_096;
const OBSERVATION_LIFETIME: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub(super) struct FatalTransaction;

impl fmt::Display for FatalTransaction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("volume catalog transaction state is unknown; writer stopped")
    }
}

impl std::error::Error for FatalTransaction {}

/// A pinned root identity. Changing its spelling or identity requires a new volume ID.
#[derive(Clone, PartialEq, Eq)]
pub struct Binding {
    pub id: String,
    pub generation: u64,
    pub root: PathBuf,
    pub filesystem: String,
    pub root_identity: String,
    pub writable: bool,
    pub draining: bool,
    pub limit_bytes: Option<u64>,
    pub minimum_free_bytes: u64,
}

impl fmt::Debug for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Binding")
            .field("id", &self.id)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Recording,
    Export,
    Thumbnail,
}

impl Kind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Recording => "recording",
            Self::Export => "export",
            Self::Thumbnail => "thumbnail",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub kind: Kind,
    pub id: String,
}

/// Probe after reading the ledger revision; any intervening mutation rejects admission.
#[derive(Debug, Clone)]
pub struct Capacity {
    pub ledger_revision: u64,
    pub observed_at: Instant,
    pub available_bytes: u64,
    pub filesystem: String,
    pub root_identity: String,
}

#[derive(Clone)]
pub struct Allocation {
    pub operation: String,
    pub object: Object,
    pub volume: String,
    pub generation: u64,
    pub relative_key: String,
    pub bytes: u64,
    pub capacity: Capacity,
}

/// Ensures a pending writer owns at least this many bytes before writing them.
#[derive(Debug, Clone)]
pub struct Growth {
    pub operation: String,
    pub bytes: u64,
    pub capacity: Capacity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usage {
    pub volume: String,
    pub filesystem: String,
    pub allocated_bytes: u64,
    pub reserved_bytes: u64,
}

impl fmt::Debug for Allocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Allocation")
            .field("operation", &self.operation)
            .field("object", &self.object)
            .field("volume", &self.volume)
            .field("bytes", &self.bytes)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub enum Request {
    Bind(Binding),
    Revision,
    Reserve(Allocation),
    ReserveArchive(Allocation, archives::Intent),
    Grow(Growth),
    Materialize(Materialization),
    BeginMove(moves::Intent),
    Move(String),
    Moves(moves::Page),
    PendingMoves(moves::Page),
    Archive(String),
    CompleteArchive { id: String, source: Location },
    AdvanceMove(moves::Step),
    Usage,
    Publish(Publication),
    Finalize(Publication),
    Lookup(Object),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Bound,
    Revision(u64),
    Reserved { operation: String, bytes: u64 },
    Location(Option<Location>),
    Usage(Vec<Usage>),
    Move(Box<moves::Job>),
    Moves(Vec<moves::Job>),
    PendingMoves(Vec<String>),
    Archive(Option<Box<archives::Job>>),
}

impl RecordingCatalogHandle {
    /// Serializes allocation decisions without touching media files.
    ///
    /// # Errors
    /// A timeout has an unknown outcome. Retry the exact same operation ID and intent.
    pub fn volume_location(&self, request: Request) -> anyhow::Result<Reply> {
        validate(&request)?;
        let deadline = Instant::now() + BUSY_TIMEOUT;
        let (reply, response) = mpsc::sync_channel(1);
        self.tx
            .try_send(Command::VolumeLocation {
                request,
                deadline,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("volume catalog unavailable"))?;
        response
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| anyhow::anyhow!("volume catalog outcome unknown; retry operation"))?
    }

    /// Returns the revision to fence a subsequent filesystem capacity probe.
    ///
    /// # Errors
    /// Returns an error when the bounded catalog request cannot complete.
    pub fn volume_ledger_revision(&self) -> anyhow::Result<u64> {
        match self.volume_location(Request::Revision)? {
            Reply::Revision(revision) => Ok(revision),
            _ => unreachable!("revision requests return revisions"),
        }
    }
}

pub(super) async fn initialize(connection: &turso::Connection) -> anyhow::Result<()> {
    materialization::migrate(connection).await?;
    connection
        .execute_batch(include_str!("locations/schema.sql"))
        .await?;
    super::ensure_column(
        connection,
        "storage_volume_bindings",
        "draining",
        "INTEGER NOT NULL DEFAULT 0 CHECK (draining IN (0, 1))",
    )
    .await?;
    moves::initialize(connection).await?;
    archives::initialize(connection).await?;
    // A sample from a previous actor lifetime must never authorize another allocation.
    bump_revision(connection).await?;
    connection.execute("INSERT OR IGNORE INTO catalog_schema_migrations (version, applied_at_ms) VALUES (3, ?1)", [super::current_unix_time_ms()]).await?;
    Ok(())
}

fn identifier(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control),
        "invalid volume ledger identifier"
    );
    Ok(())
}

fn validate(request: &Request) -> anyhow::Result<()> {
    match request {
        Request::Revision | Request::Usage => {}
        Request::Move(id) => identifier(id)?,
        Request::Archive(id) => identifier(id)?,
        Request::CompleteArchive { id, source } => {
            identifier(id)?;
            identifier(&source.object.id)?;
            anyhow::ensure!(source.revision > 0, "invalid archive source revision");
        }
        Request::Moves(page) | Request::PendingMoves(page) => {
            anyhow::ensure!((1..=64).contains(&page.limit), "invalid move page size");
            if let Some(after) = &page.after {
                identifier(after)?;
            }
        }
        Request::AdvanceMove(step) => validate_step(step)?,
        Request::BeginMove(intent) => validate_move(intent)?,
        Request::Materialize(materialized) => {
            identifier(&materialized.operation)?;
            identifier(&materialized.file_identity)?;
            to_i64(materialized.bytes, "materialized bytes")?;
        }
        Request::Grow(growth) => {
            identifier(&growth.operation)?;
            anyhow::ensure!(growth.bytes > 0, "invalid allocation growth");
            to_i64(growth.bytes, "allocation growth")?;
            identifier(&growth.capacity.filesystem)?;
            identifier(&growth.capacity.root_identity)?;
            to_i64(
                growth.capacity.available_bytes,
                "filesystem available bytes",
            )?;
        }
        Request::Publish(publication) | Request::Finalize(publication) => {
            validate_publication(publication)?;
        }
        Request::Lookup(object) => identifier(&object.id)?,
        Request::Bind(binding) => validate_binding(binding)?,
        Request::Reserve(allocation) => validate_allocation(allocation)?,
        Request::ReserveArchive(allocation, intent) => {
            validate_allocation(allocation)?;
            intent.validate()?;
            anyhow::ensure!(
                allocation.object.kind == Kind::Recording
                    && allocation.object.id != intent.id
                    && allocation.operation != intent.id,
                "archive requires a recording"
            );
        }
    }
    Ok(())
}

fn validate_step(step: &moves::Step) -> anyhow::Result<()> {
    identifier(step.id())?;
    if let moves::Step::Verified(evidence) | moves::Step::Retired(evidence) = step {
        validate_publication(evidence)?;
    }
    if let moves::Step::CancellationVerified {
        evidence:
            moves::Cancellation::File {
                relative_key,
                bytes,
                file_identity,
                ..
            },
        ..
    } = step
    {
        validate_key(relative_key)?;
        identifier(file_identity)?;
        to_i64(*bytes, "cancelled copy bytes")?;
    }
    Ok(())
}

fn validate_move(intent: &moves::Intent) -> anyhow::Result<()> {
    identifier(&intent.id)?;
    identifier(&intent.object.id)?;
    anyhow::ensure!(intent.expected_revision > 0, "invalid source revision");
    to_i64(intent.expected_revision, "source revision")?;
    anyhow::ensure!(
        intent.destination.operation == intent.id
            && intent.destination.object.id == intent.id
            && intent.destination.object.kind == intent.object.kind
            && intent.object.id != intent.id,
        "invalid move destination identity"
    );
    validate_allocation(&intent.destination)
}

fn validate_binding(binding: &Binding) -> anyhow::Result<()> {
    identifier(&binding.id)?;
    identifier(&binding.filesystem)?;
    identifier(&binding.root_identity)?;
    anyhow::ensure!(binding.generation > 0, "invalid volume generation");
    to_i64(binding.generation, "volume generation")?;
    to_i64(binding.minimum_free_bytes, "volume reserve")?;
    if let Some(limit) = binding.limit_bytes {
        anyhow::ensure!(limit > 0, "invalid volume limit");
        to_i64(limit, "volume limit")?;
    }
    crate::storage::volumes::validation::comparison_root(&binding.root)?;
    Ok(())
}

fn validate_key(key: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !key.is_empty() && key.len() <= 1_024,
        "invalid object key length"
    );
    let parts: Vec<_> = key.split('/').collect();
    anyhow::ensure!(parts.len() <= 16, "object key is too deep");
    for part in parts {
        anyhow::ensure!(
            !part.is_empty()
                && part.len() <= 240
                && !part.ends_with(['.', ' '])
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')),
            "object key is not confined"
        );
        let stem = part
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        anyhow::ensure!(
            !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                && !(stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && matches!(stem.as_bytes()[3], b'1'..=b'9')),
            "reserved object key"
        );
    }
    Ok(())
}

fn check_deadline(deadline: Instant) -> anyhow::Result<()> {
    anyhow::ensure!(Instant::now() < deadline, "volume catalog deadline expired");
    Ok(())
}

pub(super) async fn execute(
    connection: &turso::Connection,
    request: Request,
    deadline: Instant,
) -> anyhow::Result<Reply> {
    check_deadline(deadline)?;
    validate(&request)?;
    if matches!(request, Request::Revision) {
        return Ok(Reply::Revision(revision(connection).await?));
    }
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = async {
        let reply = match request {
            Request::Bind(binding) => bind(connection, &binding).await?,
            Request::Reserve(allocation) => reserve(connection, &allocation).await?,
            Request::ReserveArchive(allocation, intent) => {
                archives::reserve(connection, &allocation, &intent).await?
            }
            Request::Grow(growth) => growth::grow(connection, &growth).await?,
            Request::BeginMove(intent) => {
                Reply::Move(Box::new(archives::begin_move(connection, &intent).await?))
            }
            Request::Move(id) => Reply::Move(Box::new(moves::load(connection, &id).await?)),
            Request::Moves(page) => Reply::Moves(moves::page(connection, &page).await?),
            Request::PendingMoves(page) => {
                Reply::PendingMoves(archives::pending(connection, &page).await?)
            }
            Request::Archive(id) => {
                Reply::Archive(archives::load(connection, &id).await?.map(Box::new))
            }
            Request::CompleteArchive { id, source } => {
                archives::complete(connection, &id, &source).await?
            }
            Request::AdvanceMove(step) => {
                Reply::Move(Box::new(moves::advance(connection, &step).await?))
            }
            Request::Materialize(materialized) => {
                materialization::checkpoint(connection, &materialized).await?
            }
            Request::Usage => growth::usage(connection).await?,
            Request::Publish(publication) => ownership::publish(connection, &publication).await?,
            Request::Finalize(publication) => ownership::finalize(connection, &publication).await?,
            Request::Lookup(object) => ownership::lookup(connection, &object).await?,
            Request::Revision => unreachable!("read returned before transaction"),
        };
        check_deadline(deadline)?;
        Ok(reply)
    }
    .await;
    match result {
        Ok(reply) => {
            if let Err(error) = connection.execute_batch("COMMIT").await {
                rollback(connection).await?;
                return Err(error.into());
            }
            Ok(reply)
        }
        Err(error) => {
            rollback(connection).await?;
            Err(error)
        }
    }
}

async fn rollback(connection: &turso::Connection) -> anyhow::Result<()> {
    if connection
        .is_autocommit()
        .map_err(|error| anyhow::Error::new(error).context(FatalTransaction))?
    {
        return Ok(());
    }
    let result = connection.execute_batch("ROLLBACK").await;
    match connection.is_autocommit() {
        Ok(true) => {}
        Ok(false) => {
            return Err(result.err().map_or_else(
                || anyhow::Error::new(FatalTransaction),
                |error| anyhow::Error::new(error).context(FatalTransaction),
            ));
        }
        Err(error) => return Err(anyhow::Error::new(error).context(FatalTransaction)),
    }
    Ok(())
}

async fn revision(connection: &turso::Connection) -> anyhow::Result<u64> {
    let mut rows = connection
        .query(
            "SELECT revision FROM storage_volume_ledger WHERE singleton = 1",
            (),
        )
        .await?;
    to_u64(
        rows.next()
            .await?
            .ok_or_else(|| anyhow::anyhow!("volume ledger missing"))?
            .get::<i64>(0)?,
        "volume ledger revision",
    )
}

async fn bump_revision(connection: &turso::Connection) -> anyhow::Result<()> {
    let count = connection.execute("UPDATE storage_volume_ledger SET revision = revision + 1 WHERE singleton = 1 AND revision < 9223372036854775807", ()).await?;
    anyhow::ensure!(count == 1, "volume ledger revision exhausted");
    Ok(())
}

async fn bind(connection: &turso::Connection, binding: &Binding) -> anyhow::Result<Reply> {
    if !binding.id.starts_with("legacy-") {
        super::authority::install_format_barrier(connection).await?;
    }
    let root = binding.root.to_str().expect("validated UTF-8 root");
    let generation = to_i64(binding.generation, "volume generation")?;
    let mut rows = connection.query("SELECT generation, root, filesystem, root_identity FROM storage_volume_bindings WHERE id = ?1", [binding.id.as_str()]).await?;
    if let Some(row) = rows.next().await? {
        anyhow::ensure!(
            row.get::<i64>(0)? == generation
                && row.get::<String>(1)? == root
                && row.get::<String>(2)? == binding.filesystem
                && row.get::<String>(3)? == binding.root_identity,
            "volume identity cannot be rebound"
        );
    } else {
        let mut counts = connection
            .query("SELECT COUNT(*) FROM storage_volume_bindings", ())
            .await?;
        anyhow::ensure!(
            counts.next().await?.expect("count row").get::<i64>(0)? < MAX_BINDINGS,
            "volume binding limit reached"
        );
    }
    connection.execute("INSERT INTO storage_volume_bindings (id, generation, root, filesystem, root_identity, writable, limit_bytes, minimum_free_bytes, draining) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) ON CONFLICT(id) DO UPDATE SET writable = excluded.writable, limit_bytes = excluded.limit_bytes, minimum_free_bytes = excluded.minimum_free_bytes, draining = excluded.draining", turso::params![binding.id.clone(), generation, root, binding.filesystem.clone(), binding.root_identity.clone(), i64::from(binding.writable), binding.limit_bytes.map(|v| to_i64(v, "volume limit")).transpose()?, to_i64(binding.minimum_free_bytes, "volume reserve")?, i64::from(binding.draining)]).await?;
    bump_revision(connection).await?;
    Ok(Reply::Bound)
}

async fn reserve(connection: &turso::Connection, allocation: &Allocation) -> anyhow::Result<Reply> {
    if let Some(reply) = retry(connection, allocation).await? {
        return Ok(reply);
    }
    let capacity = &allocation.capacity;
    anyhow::ensure!(
        capacity.observed_at <= Instant::now()
            && capacity.observed_at.elapsed() <= OBSERVATION_LIFETIME,
        "volume observation expired"
    );
    anyhow::ensure!(
        capacity.ledger_revision == revision(connection).await?,
        "volume observation superseded"
    );
    let destination_path = admission(connection, allocation, true).await?;
    connection.execute("INSERT INTO storage_volume_allocations (operation, kind, object_id, volume_id, generation, relative_key, bytes, intent_bytes, state, destination_path) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, 'reserved', ?8)", turso::params![allocation.operation.clone(), allocation.object.kind.as_str(), allocation.object.id.clone(), allocation.volume.clone(), to_i64(allocation.generation, "volume generation")?, allocation.relative_key.clone(), to_i64(allocation.bytes, "allocation bytes")?, destination_path]).await?;
    bump_revision(connection).await?;
    Ok(Reply::Reserved {
        operation: allocation.operation.clone(),
        bytes: allocation.bytes,
    })
}

async fn retry(
    connection: &turso::Connection,
    allocation: &Allocation,
) -> anyhow::Result<Option<Reply>> {
    let mut rows = connection.query("SELECT kind, object_id, volume_id, generation, relative_key, intent_bytes, state, bytes FROM storage_volume_allocations WHERE operation = ?1", [allocation.operation.as_str()]).await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    anyhow::ensure!(
        row.get::<String>(0)? == allocation.object.kind.as_str()
            && row.get::<String>(1)? == allocation.object.id
            && row.get::<String>(2)? == allocation.volume
            && to_u64(row.get::<i64>(3)?, "volume generation")? == allocation.generation
            && row.get::<String>(4)? == allocation.relative_key
            && to_u64(row.get::<i64>(5)?, "allocation bytes")? == allocation.bytes,
        "operation ID was already used with different allocation intent"
    );
    anyhow::ensure!(
        row.get::<String>(6)? == "reserved",
        "allocation is no longer pending"
    );
    Ok(Some(Reply::Reserved {
        operation: allocation.operation.clone(),
        bytes: to_u64(row.get::<i64>(7)?, "allocation bytes")?,
    }))
}

async fn admission(
    connection: &turso::Connection,
    allocation: &Allocation,
    new_allocation: bool,
) -> anyhow::Result<String> {
    let mut rows = connection.query("SELECT generation, writable, limit_bytes, minimum_free_bytes, filesystem, root_identity, root, draining FROM storage_volume_bindings WHERE id = ?1", [allocation.volume.as_str()]).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("volume is not bound"))?;
    anyhow::ensure!(
        to_u64(row.get::<i64>(0)?, "volume generation")? == allocation.generation
            && row.get::<i64>(1)? == 1,
        "volume is not writable at this generation"
    );
    anyhow::ensure!(
        !new_allocation || row.get::<i64>(7)? == 0,
        "volume is draining"
    );
    let filesystem = row.get::<String>(4)?;
    anyhow::ensure!(
        allocation.capacity.filesystem == filesystem
            && allocation.capacity.root_identity == row.get::<String>(5)?,
        "capacity observation belongs to another root"
    );
    // ponytail: At most 37 bindings share capacity; counters avoid scanning media history.
    let mut totals = connection.query("SELECT (SELECT allocated_bytes FROM storage_volume_bindings WHERE id = ?1), (SELECT COALESCE(SUM(reserved_bytes), 0) FROM storage_volume_bindings WHERE filesystem = ?2), (SELECT pending_count FROM storage_volume_ledger WHERE singleton = 1)", turso::params![allocation.volume.clone(), filesystem]).await?;
    let total = totals.next().await?.expect("aggregate row");
    let owned = to_u64(total.get::<i64>(0)?, "volume owned bytes")?;
    let reserved = to_u64(total.get::<i64>(1)?, "filesystem reserved bytes")?;
    to_i64(
        owned
            .checked_add(allocation.bytes)
            .ok_or_else(|| anyhow::anyhow!("volume accounting overflow"))?,
        "volume allocated bytes",
    )?;
    to_i64(
        reserved
            .checked_add(allocation.bytes)
            .ok_or_else(|| anyhow::anyhow!("filesystem accounting overflow"))?,
        "filesystem reserved bytes",
    )?;
    anyhow::ensure!(
        !new_allocation || total.get::<i64>(2)? < MAX_PENDING_ALLOCATIONS,
        "pending allocation limit reached"
    );
    let minimum = to_u64(row.get::<i64>(3)?, "volume reserve")?;
    anyhow::ensure!(
        allocation.bytes
            <= allocation
                .capacity
                .available_bytes
                .saturating_sub(reserved)
                .saturating_sub(minimum),
        "filesystem allocation capacity exhausted"
    );
    if let Some(limit) = row.get::<Option<i64>>(2)? {
        anyhow::ensure!(
            allocation.bytes <= to_u64(limit, "volume limit")?.saturating_sub(owned),
            "volume allocation capacity exhausted"
        );
    }
    let destination = PathBuf::from(row.get::<String>(6)?).join(&allocation.relative_key);
    Ok(destination
        .components()
        .collect::<PathBuf>()
        .to_string_lossy()
        .replace('\\', "/"))
}

#[cfg(test)]
mod archives_tests;
#[cfg(test)]
mod finalize_tests;
#[cfg(test)]
mod growth_tests;
#[cfg(test)]
mod materialization_tests;
#[cfg(test)]
mod move_recording_tests;
#[cfg(test)]
mod moves_tests;
#[cfg(test)]
mod tests;

fn validate_publication(publication: &Publication) -> anyhow::Result<()> {
    identifier(&publication.operation)?;
    identifier(&publication.file_identity)?;
    anyhow::ensure!(publication.bytes > 0, "empty media publication");
    to_i64(publication.bytes, "publication bytes")?;
    Ok(())
}

fn validate_allocation(allocation: &Allocation) -> anyhow::Result<()> {
    identifier(&allocation.operation)?;
    identifier(&allocation.object.id)?;
    identifier(&allocation.volume)?;
    identifier(&allocation.capacity.filesystem)?;
    identifier(&allocation.capacity.root_identity)?;
    validate_key(&allocation.relative_key)?;
    anyhow::ensure!(
        allocation.bytes > 0 && allocation.generation > 0,
        "invalid allocation"
    );
    to_i64(allocation.bytes, "allocation bytes")?;
    to_i64(allocation.generation, "volume generation")?;
    to_i64(
        allocation.capacity.available_bytes,
        "filesystem available bytes",
    )?;
    Ok(())
}
