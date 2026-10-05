//! Transfers confirmed legacy recording ownership and admits its move atomically.

use super::super::{Binding, Location, Reply, bump_revision, identifier, moves, revision, to_i64};
use super::{inventory, roots};

#[derive(Debug, Clone)]
pub struct Intent {
    pub reference: inventory::Reference,
    pub role: roots::Role,
    pub operation: String,
    pub destination: moves::Intent,
}

impl Intent {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        identifier(&self.operation)?;
        uuid::Uuid::parse_str(&self.operation)?;
        let evidence = self
            .reference
            .evidence
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("legacy source is not verified"))?;
        inventory::Action::Verify(Box::new(self.reference.clone()), evidence.clone()).validate()?;
        super::super::validate_move(&self.destination)?;
        anyhow::ensure!(
            matches!(self.role, roots::Role::Active | roots::Role::Archive)
                && evidence.bytes > 0
                && self.destination.object == self.reference.object
                && self.destination.expected_revision == 1
                && self.operation != self.destination.id,
            "invalid legacy adoption intent"
        );
        Ok(())
    }
}

pub(in crate::storage::catalog) async fn initialize(
    connection: &turso::Connection,
) -> anyhow::Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS storage_legacy_adoptions (
        operation TEXT PRIMARY KEY REFERENCES storage_volume_allocations(operation),
        reference_revision INTEGER NOT NULL CHECK(reference_revision > 0),
        catalog_identity TEXT NOT NULL,
        role TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS storage_legacy_owner_path
        ON recording_files(replace(path,char(92),'/') COLLATE NOCASE);",
        )
        .await?;
    Ok(())
}

pub(crate) async fn begin(
    connection: &turso::Connection,
    intent: &Intent,
) -> anyhow::Result<Reply> {
    intent.validate()?;
    let roots::State::Bound(binding) = roots::lookup(connection, intent.role).await? else {
        anyhow::bail!("legacy root has not been captured");
    };
    let source = source_location(&intent.reference, &binding)?;
    if let Some(job) = moves::find(connection, &intent.destination.id).await? {
        check_retry(connection, intent, &source, &job).await?;
        moves::validate_destination_intent(connection, &intent.destination.destination).await?;
        return Ok(Reply::Move(job));
    }
    anyhow::ensure!(
        intent.destination.destination.capacity.ledger_revision == revision(connection).await?,
        "volume observation superseded"
    );
    let current = inventory::lookup(connection, &intent.reference.object)
        .await?
        .ok_or_else(|| anyhow::anyhow!("legacy owner is unavailable"))?;
    anyhow::ensure!(
        current == intent.reference,
        "legacy adoption preview changed"
    );
    ensure_single_owner(connection, &current).await?;
    inventory::verify(
        connection,
        &current,
        current.evidence.as_ref().expect("validated evidence"),
    )
    .await?;
    insert_source(connection, intent, &source).await?;
    let mut destination = intent.destination.clone();
    // Source promotion is our own mutation in this transaction; the original sample was checked above.
    destination.destination.capacity.ledger_revision = revision(connection).await?;
    Ok(Reply::Move(Box::new(
        moves::begin(connection, &destination).await?,
    )))
}

async fn ensure_single_owner(
    connection: &turso::Connection,
    reference: &inventory::Reference,
) -> anyhow::Result<()> {
    let path = reference
        .path
        .to_str()
        .expect("validated path")
        .replace('\\', "/");
    let mut rows = connection
        .query(
            "SELECT 1 FROM recording_files WHERE replace(path,char(92),'/')=?1 COLLATE NOCASE
         AND id<>?2 LIMIT 1",
            (path.as_str(), reference.object.id.as_str()),
        )
        .await?;
    anyhow::ensure!(
        rows.next().await?.is_none(),
        "legacy file has multiple catalog owners"
    );
    Ok(())
}

pub(crate) fn source_location(
    reference: &inventory::Reference,
    binding: &Binding,
) -> anyhow::Result<Location> {
    let key = reference
        .path
        .strip_prefix(&binding.root)?
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("legacy path is not UTF-8"))?
        .replace('\\', "/");
    crate::storage::volumes::root::validate_legacy_key(&key)?;
    let evidence = reference.evidence.as_ref().expect("validated evidence");
    Ok(Location {
        object: reference.object.clone(),
        volume: binding.id.clone(),
        generation: binding.generation,
        relative_key: key,
        revision: 1,
        bytes: evidence.bytes,
        file_identity: evidence.file_identity.clone(),
        digest: evidence.digest,
    })
}

async fn insert_source(
    connection: &turso::Connection,
    intent: &Intent,
    source: &Location,
) -> anyhow::Result<()> {
    let evidence = intent
        .reference
        .evidence
        .as_ref()
        .expect("validated evidence");
    let path = intent
        .reference
        .path
        .to_str()
        .expect("validated path")
        .replace('\\', "/");
    connection.execute("INSERT INTO storage_volume_allocations
        (operation,kind,object_id,volume_id,generation,relative_key,destination_path,bytes,intent_bytes,materialized_bytes,state,file_identity,digest,location_revision)
        VALUES(?1,'recording',?2,?3,?4,?5,?6,?7,?7,?7,'published',?8,?9,1)",
        turso::params![intent.operation.clone(), source.object.id.clone(), source.volume.clone(), to_i64(source.generation,"source generation")?,
            source.relative_key.clone(), path, to_i64(source.bytes,"source bytes")?, source.file_identity.clone(), source.digest.to_vec()]).await?;
    connection.execute("INSERT INTO storage_legacy_adoptions(operation,reference_revision,catalog_identity,role) VALUES(?1,?2,?3,?4)",
        turso::params![intent.operation.clone(), to_i64(intent.reference.revision,"legacy revision")?, evidence.catalog_identity.clone(), intent.role.id()]).await?;
    bump_revision(connection).await
}

async fn check_retry(
    connection: &turso::Connection,
    intent: &Intent,
    source: &Location,
    job: &moves::Job,
) -> anyhow::Result<()> {
    anyhow::ensure!(job.source == *source, "legacy adoption source changed");
    let mut rows = connection.query("SELECT p.reference_revision,p.catalog_identity,p.role FROM storage_legacy_adoptions p
        JOIN storage_volume_moves m ON m.source_operation=p.operation WHERE p.operation=?1 AND m.id=?2",
        (intent.operation.as_str(), intent.destination.id.as_str())).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("legacy adoption provenance changed"))?;
    anyhow::ensure!(
        row.get::<i64>(0)? == to_i64(intent.reference.revision, "legacy revision")?
            && row.get::<String>(1)?
                == intent
                    .reference
                    .evidence
                    .as_ref()
                    .expect("validated evidence")
                    .catalog_identity
            && row.get::<String>(2)? == intent.role.id(),
        "legacy adoption intent changed"
    );
    Ok(())
}
