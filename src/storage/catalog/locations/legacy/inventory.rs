//! Records legacy file evidence without transferring retention or capacity ownership.

use super::super::{Kind, Object, Reply, identifier, to_i64, to_u64};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub file_identity: String,
    pub catalog_identity: String,
    pub bytes: u64,
    pub digest: [u8; 32],
}

#[derive(Clone, PartialEq, Eq)]
pub struct Reference {
    pub object: Object,
    pub path: PathBuf,
    pub revision: u64,
    pub evidence: Option<Evidence>,
}

impl std::fmt::Debug for Reference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LegacyReference")
            .field("object", &self.object)
            .field("revision", &self.revision)
            .field("verified", &self.evidence.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub enum Action {
    Recordings { after: Option<String>, limit: u16 },
    Lookup(Object),
    Verify(Box<Reference>, Evidence),
}

impl Action {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        match self {
            Self::Recordings { after, limit } => validate_page(after.as_deref(), *limit),
            Self::Lookup(object) => validate_object(object),
            Self::Verify(reference, evidence) => {
                validate_object(&reference.object)?;
                anyhow::ensure!(reference.revision > 0, "invalid legacy reference revision");
                to_i64(reference.revision, "legacy reference revision")?;
                crate::storage::volumes::validation::comparison_root(&reference.path)?;
                identifier(&evidence.file_identity)?;
                identifier(&evidence.catalog_identity)?;
                to_i64(evidence.bytes, "legacy file bytes")?;
                Ok(())
            }
        }
    }
}

pub(crate) async fn dispatch(
    connection: &turso::Connection,
    action: Action,
) -> anyhow::Result<Reply> {
    action.validate()?;
    Ok(match action {
        Action::Recordings { after, limit } => {
            Reply::LegacyReferences(register_recordings(connection, after.as_deref(), limit).await?)
        }
        Action::Lookup(object) => {
            Reply::LegacyReference(lookup(connection, &object).await?.map(Box::new))
        }
        Action::Verify(reference, evidence) => Reply::LegacyReference(Some(Box::new(
            verify(connection, &reference, &evidence).await?,
        ))),
    })
}

pub(crate) async fn initialize(connection: &turso::Connection) -> anyhow::Result<()> {
    connection
        .execute_batch(include_str!("inventory.sql"))
        .await?;
    Ok(())
}

pub(super) async fn register_recordings(
    connection: &turso::Connection,
    after: Option<&str>,
    limit: u16,
) -> anyhow::Result<Vec<Reference>> {
    validate_page(after, limit)?;
    let mut rows = connection.query(
        "SELECT id,path FROM storage_legacy_recording_candidates WHERE id>?1 ORDER BY id LIMIT ?2",
        turso::params![after.unwrap_or(""), i64::from(limit)],
    ).await?;
    let mut candidates = Vec::with_capacity(usize::from(limit));
    while let Some(row) = rows.next().await? {
        candidates.push((row.get::<String>(0)?, row.get::<String>(1)?));
    }
    drop(rows);
    let revision = next_revision(connection).await?;
    let mut references = Vec::with_capacity(candidates.len());
    for (id, path) in candidates {
        connection.execute(
            "INSERT OR IGNORE INTO storage_legacy_recordings(recording_id,path,revision) VALUES(?1,?2,?3)",
            turso::params![id.clone(), path, revision],
        ).await?;
        let object = Object {
            kind: Kind::Recording,
            id,
        };
        references.push(
            lookup(connection, &object)
                .await?
                .ok_or_else(|| anyhow::anyhow!("legacy owner changed during registration"))?,
        );
    }
    Ok(references)
}

pub(super) async fn verify(
    connection: &turso::Connection,
    reference: &Reference,
    evidence: &Evidence,
) -> anyhow::Result<Reference> {
    Action::Verify(Box::new(reference.clone()), evidence.clone()).validate()?;
    let mut current = lookup(connection, &reference.object)
        .await?
        .ok_or_else(|| anyhow::anyhow!("legacy owner is unavailable"))?;
    anyhow::ensure!(
        current.path == reference.path && current.revision == reference.revision,
        "legacy verification is stale"
    );
    check_owner_evidence(connection, &reference.object.id, evidence).await?;
    if let Some(previous) = &current.evidence {
        anyhow::ensure!(previous == evidence, "legacy verification evidence changed");
        return Ok(current);
    }
    connection.execute(
        "UPDATE storage_legacy_recordings SET file_identity=?1,bytes=?2,digest=?3,catalog_identity=?6 WHERE recording_id=?4 AND revision=?5",
        turso::params![evidence.file_identity.clone(), to_i64(evidence.bytes,"legacy file bytes")?,
            evidence.digest.to_vec(), reference.object.id.clone(), to_i64(reference.revision,"legacy reference revision")?, evidence.catalog_identity.clone()],
    ).await?;
    current.evidence = Some(evidence.clone());
    Ok(current)
}

pub(super) async fn lookup(
    connection: &turso::Connection,
    object: &Object,
) -> anyhow::Result<Option<Reference>> {
    validate_object(object)?;
    let mut rows = connection.query(
        "SELECT l.path,l.revision,l.file_identity,l.bytes,l.digest,l.catalog_identity FROM storage_legacy_recordings l
        JOIN storage_legacy_recording_candidates r ON r.id=l.recording_id WHERE l.recording_id=?1",
        [object.id.as_str()],
    ).await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let evidence = row
        .get::<Option<String>>(2)?
        .map(|file_identity| -> anyhow::Result<Evidence> {
            Ok(Evidence {
                file_identity,
                catalog_identity: row.get(5)?,
                bytes: to_u64(row.get(3)?, "legacy file bytes")?,
                digest: row
                    .get::<Vec<u8>>(4)?
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("invalid legacy digest"))?,
            })
        })
        .transpose()?;
    Ok(Some(Reference {
        object: object.clone(),
        path: PathBuf::from(row.get::<String>(0)?),
        revision: to_u64(row.get(1)?, "legacy reference revision")?,
        evidence,
    }))
}

async fn check_owner_evidence(
    connection: &turso::Connection,
    id: &str,
    evidence: &Evidence,
) -> anyhow::Result<()> {
    let mut rows = connection
        .query(
            "SELECT file_identity,file_bytes FROM recording_files WHERE id=?1",
            [id],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("legacy owner is missing"))?;
    let identity: Option<String> = row.get(0)?;
    let bytes = to_u64(row.get(1)?, "catalog file bytes")?;
    anyhow::ensure!(
        identity
            .is_none_or(|identity| identity == evidence.file_identity
                || identity == evidence.catalog_identity),
        "legacy file differs from its catalog identity"
    );
    anyhow::ensure!(
        bytes == 0 || bytes == evidence.bytes,
        "legacy file differs from its catalog size"
    );
    Ok(())
}

fn validate_object(object: &Object) -> anyhow::Result<()> {
    anyhow::ensure!(object.kind == Kind::Recording, "unsupported legacy owner");
    identifier(&object.id)
}

fn validate_page(after: Option<&str>, limit: u16) -> anyhow::Result<()> {
    anyhow::ensure!(
        (1..=64).contains(&limit),
        "invalid legacy inventory page size"
    );
    if let Some(after) = after {
        identifier(after)?;
    }
    Ok(())
}

async fn next_revision(connection: &turso::Connection) -> anyhow::Result<i64> {
    let mut rows = connection
        .query(
            "SELECT revision FROM recording_catalog_state WHERE id=1",
            (),
        )
        .await?;
    let revision: i64 = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("catalog revision is missing"))?
        .get(0)?;
    anyhow::ensure!(
        (0..i64::MAX).contains(&revision),
        "catalog revision exhausted"
    );
    Ok(revision + 1)
}
