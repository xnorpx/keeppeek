//! Captures legacy directory identities without admitting writes or adopting objects.

use super::super::{Binding, Reply, bind, bump_revision, to_u64, validate_binding};
use super::LegacyPaths;
use crate::storage::volumes::validation::comparison_root;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Active,
    Archive,
    Export,
    Thumbnail,
}

impl Role {
    pub const ALL: [Self; 4] = [Self::Active, Self::Archive, Self::Export, Self::Thumbnail];
    pub const fn id(self) -> &'static str {
        match self {
            Self::Active => "legacy-active",
            Self::Archive => "legacy-archive",
            Self::Export => "legacy-export",
            Self::Thumbnail => "legacy-thumbnail",
        }
    }
    pub fn path(self, paths: &LegacyPaths) -> &Path {
        match self {
            Self::Active => &paths.active_root,
            Self::Archive => &paths.archive_root,
            Self::Export => &paths.export_root,
            Self::Thumbnail => &paths.thumbnail_root,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Capture {
    pub paths: LegacyPaths,
    pub roots: Vec<(Role, Option<Binding>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Uncaptured,
    Offline,
    Bound(Box<Binding>),
}

impl Capture {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        self.paths.validate()?;
        anyhow::ensure!(
            self.roots.len() == Role::ALL.len(),
            "legacy capture requires every role"
        );
        for role in Role::ALL {
            anyhow::ensure!(
                self.roots
                    .iter()
                    .filter(|(candidate, _)| *candidate == role)
                    .count()
                    == 1,
                "legacy capture has duplicate or missing roles"
            );
        }
        for (role, binding) in &self.roots {
            if let Some(binding) = binding {
                validate_root(*role, &self.paths, binding)?;
            }
        }
        Ok(())
    }
}

fn validate_root(role: Role, paths: &LegacyPaths, binding: &Binding) -> anyhow::Result<()> {
    validate_binding(binding)?;
    let owner = Role::ALL
        .into_iter()
        .find(|candidate| candidate.id() == binding.id)
        .ok_or_else(|| anyhow::anyhow!("invalid legacy root binding ID"))?;
    anyhow::ensure!(
        comparison_root(&binding.root)? == comparison_root(role.path(paths))?
            && comparison_root(&binding.root)? == comparison_root(owner.path(paths))?,
        "legacy root is outside captured paths"
    );
    anyhow::ensure!(
        binding.generation == 1
            && !binding.writable
            && !binding.draining
            && binding.limit_bytes.is_none()
            && binding.minimum_free_bytes == 0,
        "legacy capture cannot authorize writes or quota"
    );
    Ok(())
}

pub(in crate::storage::catalog) async fn initialize(
    connection: &turso::Connection,
) -> anyhow::Result<()> {
    connection.execute_batch("CREATE TABLE IF NOT EXISTS storage_legacy_root_roles (
        role TEXT PRIMARY KEY CHECK(role IN ('legacy-active','legacy-archive','legacy-export','legacy-thumbnail')),
        binding_id TEXT REFERENCES storage_volume_bindings(id)
    );
    CREATE TRIGGER IF NOT EXISTS storage_legacy_root_role_fence BEFORE UPDATE ON storage_legacy_root_roles
    WHEN OLD.binding_id IS NOT NULL AND NEW.binding_id IS NOT OLD.binding_id
    BEGIN SELECT RAISE(ABORT,'legacy root mapping is immutable'); END;
    CREATE TRIGGER IF NOT EXISTS storage_legacy_root_role_delete_fence BEFORE DELETE ON storage_legacy_root_roles
    BEGIN SELECT RAISE(ABORT,'legacy root mapping is immutable'); END;").await?;
    Ok(())
}

pub(crate) async fn capture(
    connection: &turso::Connection,
    capture: &Capture,
) -> anyhow::Result<Reply> {
    capture.validate()?;
    let paths = super::register(connection, &capture.paths).await?;
    paths.ensure_same_media_roots(&capture.paths)?;
    crate::storage::catalog::authority::install_format_barrier(connection).await?;
    for (role, binding) in &capture.roots {
        capture_role(connection, *role, binding.as_ref(), &paths).await?;
    }
    Ok(Reply::Bound)
}

async fn capture_role(
    connection: &turso::Connection,
    role: Role,
    binding: Option<&Binding>,
    paths: &LegacyPaths,
) -> anyhow::Result<()> {
    if let Some(binding) = binding {
        validate_root(role, paths, binding)?;
        if let State::Bound(previous) = lookup(connection, role).await? {
            anyhow::ensure!(
                previous.id == binding.id,
                "legacy role cannot change its binding"
            );
        }
        bind(connection, binding).await?;
    }
    let changed = connection
        .execute(
            "INSERT INTO storage_legacy_root_roles(role,binding_id) VALUES(?1,?2)
         ON CONFLICT(role) DO UPDATE SET binding_id=excluded.binding_id
         WHERE storage_legacy_root_roles.binding_id IS NULL AND excluded.binding_id IS NOT NULL",
            turso::params![role.id(), binding.map(|binding| binding.id.clone())],
        )
        .await?;
    if changed > 0 {
        bump_revision(connection).await?;
    }
    Ok(())
}

pub(crate) async fn lookup(connection: &turso::Connection, role: Role) -> anyhow::Result<State> {
    let mut rows = connection
        .query(
            "SELECT binding_id FROM storage_legacy_root_roles WHERE role=?1",
            [role.id()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(State::Uncaptured);
    };
    let Some(id) = row.get::<Option<String>>(0)? else {
        return Ok(State::Offline);
    };
    drop(rows);
    let mut rows = connection.query("SELECT generation,root,filesystem,root_identity,writable,draining,limit_bytes,minimum_free_bytes
        FROM storage_volume_bindings WHERE id=?1", [id.as_str()]).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("captured legacy root binding is missing"))?;
    let binding = Binding {
        id,
        generation: to_u64(row.get(0)?, "legacy root generation")?,
        root: row.get::<String>(1)?.into(),
        filesystem: row.get(2)?,
        root_identity: row.get(3)?,
        writable: row.get::<i64>(4)? != 0,
        draining: row.get::<i64>(5)? != 0,
        limit_bytes: row
            .get::<Option<i64>>(6)?
            .map(|bytes| to_u64(bytes, "legacy root limit"))
            .transpose()?,
        minimum_free_bytes: to_u64(row.get(7)?, "legacy root reserve")?,
    };
    drop(rows);
    let paths = super::load(connection)
        .await?
        .ok_or_else(|| anyhow::anyhow!("captured legacy paths are missing"))?;
    validate_root(role, &paths, &binding)?;
    Ok(State::Bound(Box::new(binding)))
}
