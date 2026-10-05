//! Keeps legacy readers visible when catalog ownership changes during a move.

use super::{LeaseSet, RecordingCatalogHandle, Registry, Reply};
use std::{collections::BTreeSet, path::Path, sync::Arc};

pub(in crate::storage::catalog) enum Request {
    Export {
        id: String,
        path: String,
    },
    Image {
        event: String,
        attachment: String,
        revision: u64,
        path: String,
    },
}

impl RecordingCatalogHandle {
    pub(crate) fn lease_legacy_export(&self, id: &str, path: &Path) -> anyhow::Result<LeaseSet> {
        self.legacy_read_lease(Request::Export {
            id: id.to_owned(),
            path: reader_path(path)?,
        })
    }

    pub(crate) fn lease_legacy_image(
        &self,
        event: &super::super::TimelineEvent,
        attachment: &str,
        path: &Path,
    ) -> anyhow::Result<LeaseSet> {
        self.legacy_read_lease(Request::Image {
            event: event.id.clone(),
            attachment: attachment.into(),
            revision: event.revision,
            path: reader_path(path)?,
        })
    }

    fn legacy_read_lease(&self, request: Request) -> anyhow::Result<LeaseSet> {
        match self.read_lease(super::Request::Legacy(request))? {
            Reply::Snapshots(lease) => Ok(lease),
            _ => anyhow::bail!("invalid legacy reader reply"),
        }
    }
}

fn reader_path(path: &Path) -> anyhow::Result<String> {
    let path = std::path::absolute(path)?;
    let path = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("legacy reader path is not UTF-8"))?;
    anyhow::ensure!(
        path.len() <= 4096 && !path.chars().any(char::is_control),
        "invalid legacy reader path"
    );
    Ok(path.to_owned())
}

pub(super) async fn execute(
    connection: &turso::Connection,
    registry: &Arc<Registry>,
    request: Request,
) -> anyhow::Result<Reply> {
    let (id, path) = match request {
        Request::Export { id, path } => {
            anyhow::ensure!(
                !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control),
                "invalid export reader ID"
            );
            let mut rows = connection
                .query(
                    "SELECT 1 FROM storage_volume_allocations WHERE kind='export' AND object_id=?1
                 UNION ALL SELECT 1 FROM storage_export_cleanup WHERE object_id=?1 LIMIT 1",
                    [id.as_str()],
                )
                .await?;
            anyhow::ensure!(
                rows.next().await?.is_none(),
                "export ownership changed; resolve its managed location"
            );
            (id, path)
        }
        Request::Image {
            event,
            attachment,
            revision,
            path,
        } => {
            anyhow::ensure!(
                matches!(
                    super::image(connection, registry, &event, &attachment, revision).await?,
                    Reply::Image(None)
                ),
                "image ownership changed; resolve its managed location"
            );
            (event, path)
        }
    };
    Ok(Reply::Snapshots(
        registry.acquire(BTreeSet::from([(id, path)]))?,
    ))
}
