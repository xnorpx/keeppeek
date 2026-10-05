use super::*;
use crate::storage::catalog::locations::{
    legacy::{adoption, inventory, roots},
    moves,
};

#[derive(Debug, Clone)]
pub(super) struct Preview {
    reference: inventory::Reference,
    role: roots::Role,
}

impl Manager {
    pub(super) fn preview_legacy_recording(
        &self,
        object: Object,
    ) -> anyhow::Result<(Location, Option<Preview>)> {
        anyhow::ensure!(
            object.kind == Kind::Recording,
            "legacy media kind is not supported"
        );
        let catalog = &self.inner.catalog;
        let Reply::LegacyReference(Some(reference)) =
            catalog.volume_location(Request::LegacyInventory(inventory::Action::Lookup(object)))?
        else {
            anyhow::bail!("legacy recording is not registered; refresh the inventory");
        };
        let reference = crate::storage::volumes::legacy::verify_recording(catalog, &reference)?;
        let (_, role) = crate::storage::volumes::legacy::recording_role(catalog, &reference)?;
        let Reply::LegacyRoot(roots::State::Bound(binding)) =
            catalog.volume_location(Request::LegacyRoot(role))?
        else {
            anyhow::bail!("legacy root identity has not been captured");
        };
        let location = adoption::source_location(&reference, &binding)?;
        Ok((location, Some(Preview { reference, role })))
    }

    pub(super) fn admit_legacy_recording(
        &self,
        job_id: &str,
        preview: &MovePreview,
    ) -> anyhow::Result<()> {
        let legacy = preview.legacy.as_ref().expect("legacy preview");
        let catalog = &self.inner.catalog;
        let _source_claim = catalog.claim_volume_move(&preview.source.object.id)?;
        let current =
            crate::storage::volumes::legacy::verify_recording(catalog, &legacy.reference)?;
        anyhow::ensure!(current == legacy.reference, "legacy preview changed");
        let index = self.move_destination(preview)?;
        let _file = self.open_owned(&preview.source)?;
        let object = Object {
            kind: Kind::Recording,
            id: job_id.to_owned(),
        };
        let destination = Allocation {
            operation: job_id.to_owned(),
            relative_key: object_key(preview.role, &object)?,
            object,
            volume: preview.destination.to_string(),
            generation: 1,
            bytes: preview.source.bytes,
            capacity: self
                .inner
                .root(index)?
                .capacity(catalog.volume_ledger_revision()?)?,
        };
        let Reply::Move(job) =
            catalog.volume_location(Request::AdoptLegacyRecording(Box::new(adoption::Intent {
                reference: current,
                role: legacy.role,
                operation: uuid::Uuid::new_v4().to_string(),
                destination: moves::Intent {
                    id: job_id.to_owned(),
                    object: preview.source.object.clone(),
                    expected_revision: preview.source.revision,
                    destination,
                },
            })))?
        else {
            anyhow::bail!("invalid legacy adoption reply");
        };
        anyhow::ensure!(job.phase == "reserved", "legacy move requires recovery");
        Ok(())
    }
}
