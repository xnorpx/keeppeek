//! Previews and admits operator moves through the existing durable move journal.

use super::*;
use crate::storage::catalog::locations::Location;

/// A captured source and explicit destination. Admission rechecks both.
#[derive(Debug, Clone)]
pub struct MovePreview {
    source: Location,
    destination: super::super::VolumeId,
    role: VolumeRole,
    source_id: String,
    groups: Vec<String>,
}

impl MovePreview {
    pub const fn source(&self) -> &Location {
        &self.source
    }
    pub fn destination(&self) -> &str {
        self.destination.as_str()
    }
}

impl Manager {
    /// Returns the immutable configuration used by active writers and move admission.
    pub fn configuration(&self) -> &VolumeConfiguration {
        &self.inner.configuration
    }
    /// Reports bounded observations without reserving or creating media files.
    ///
    /// # Errors
    /// Returns an error if the catalog cannot provide capacity ownership.
    pub fn observations(&self) -> anyhow::Result<Vec<VolumeObservation>> {
        let candidates = self
            .inner
            .configuration
            .volumes
            .iter()
            .map(|v| v.id.clone())
            .collect::<Vec<_>>();
        self.inner.observations(&candidates)
    }

    /// Evaluates new-object placement without changing existing objects.
    ///
    /// # Errors
    /// Rejects invalid requests or unavailable catalog observations.
    pub fn preview_placement(
        &self,
        request: &PlacementRequest<'_>,
        groups: &[&str],
    ) -> anyhow::Result<super::super::PlacementDecision> {
        anyhow::ensure!(
            groups.len() <= super::super::RULES_MAX,
            "too many source groups"
        );
        self.inner
            .configuration
            .place_with_groups(request, groups, &self.observations()?)
    }

    /// Captures a move without changing placement policies or reserving space.
    ///
    /// # Errors
    /// Rejects unavailable sources, incompatible destinations, and insufficient capacity.
    pub fn preview_move(
        &self,
        object: Object,
        destination: &str,
        request: &PlacementRequest<'_>,
        groups: &[&str],
    ) -> anyhow::Result<MovePreview> {
        anyhow::ensure!(
            groups.len() <= super::super::RULES_MAX,
            "too many source groups"
        );
        anyhow::ensure!(
            request.source.len() <= 256 && groups.iter().all(|group| group.len() <= 256),
            "source selector is too long"
        );
        let Reply::Location(Some(source)) = self
            .inner
            .catalog
            .volume_location(Request::Lookup(object))?
        else {
            anyhow::bail!("move source is not owned");
        };
        let preview = MovePreview {
            source,
            destination: super::super::VolumeId::parse(destination)?,
            role: request.role,
            source_id: request.source.to_owned(),
            groups: groups.iter().map(|group| (*group).to_owned()).collect(),
        };
        self.move_destination(&preview)?;
        let _file = self.open_owned(&preview.source)?;
        Ok(preview)
    }

    /// Journals a confirmed move before a worker is notified. It does not copy files.
    ///
    /// # Errors
    /// Rejects changed sources, unavailable capacity, and conflicting work.
    pub fn admit_move(&self, job_id: &str, preview: &MovePreview) -> anyhow::Result<()> {
        anyhow::ensure!(uuid::Uuid::parse_str(job_id).is_ok(), "invalid move job ID");
        let _guard = self
            .inner
            .admission
            .lock()
            .map_err(|_| anyhow::anyhow!("volume admission unavailable"))?;
        let Reply::OptionalMove(existing) = self
            .inner
            .catalog
            .volume_location(Request::FindMove(job_id.to_owned()))?
        else {
            anyhow::bail!("invalid move journal reply");
        };
        if let Some(existing) = existing {
            anyhow::ensure!(
                existing.source == preview.source
                    && existing.destination.volume == preview.destination.as_str(),
                "move intent changed"
            );
            return Ok(());
        }
        let Reply::Location(Some(current)) = self
            .inner
            .catalog
            .volume_location(Request::Lookup(preview.source.object.clone()))?
        else {
            anyhow::bail!("move source is not owned");
        };
        anyhow::ensure!(current == preview.source, "move preview is stale");
        let index = self.move_destination(preview)?;
        let _file = self.open_owned(&current)?;
        self.commit_move(index, current.clone(), current.object, job_id, preview.role)?;
        Ok(())
    }

    fn move_destination(&self, preview: &MovePreview) -> anyhow::Result<usize> {
        anyhow::ensure!(
            preview.source.volume != preview.destination.as_str(),
            "object is already on the destination"
        );
        object_key(preview.role, &preview.source.object)?;
        let (index, volume) = self
            .inner
            .configuration
            .volumes
            .iter()
            .enumerate()
            .find(|(_, volume)| volume.id == preview.destination)
            .ok_or_else(|| anyhow::anyhow!("move destination is not configured"))?;
        let observations = self
            .inner
            .observations(std::slice::from_ref(&preview.destination))?;
        let groups = preview
            .groups
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let request = PlacementRequest {
            role: preview.role,
            source: &preview.source_id,
            group: "",
            required_bytes: preview.source.bytes,
        };
        super::super::placement::eligibility(volume, &request, &groups, observations.first())
            .map_err(|reason| anyhow::anyhow!("move destination rejected: {reason:?}"))?;
        Ok(index)
    }
}

#[cfg(test)]
mod tests;
