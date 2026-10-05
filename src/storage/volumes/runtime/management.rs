//! Previews and admits operator moves through the existing durable move journal.

use super::*;
use crate::storage::catalog::locations::Location;

mod legacy;

/// A captured source and explicit destination. Admission rechecks both.
#[derive(Debug, Clone)]
pub struct MovePreview {
    source: Location,
    destination: super::super::VolumeId,
    role: VolumeRole,
    source_id: String,
    groups: Vec<String>,
    legacy: Option<legacy::Preview>,
}

impl MovePreview {
    /// Confirmation permanently adopts this source even if the transfer is later cancelled.
    pub const fn adopts_legacy(&self) -> bool {
        self.legacy.is_some()
    }
    pub const fn source(&self) -> &Location {
        &self.source
    }
    pub fn destination(&self) -> &str {
        self.destination.as_str()
    }
}

impl Manager {
    /// Stops or resumes new placements without revoking admitted writers.
    ///
    /// # Errors
    /// Rejects unconfigured or unbound volumes and unavailable catalog ownership.
    pub fn set_draining(&self, volume: &str, draining: bool) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.inner
                .configuration
                .volumes
                .iter()
                .any(|item| item.id.as_str() == volume),
            "volume is not configured"
        );
        // ponytail: bindings currently use generation 1 and never permit identity rebinding.
        let reply = self.inner.catalog.volume_location(Request::SetDraining {
            volume: volume.to_owned(),
            generation: 1,
            draining,
        })?;
        anyhow::ensure!(reply == Reply::Bound, "invalid drain reply");
        Ok(())
    }

    /// Reports whether new objects use named admission, including unavailable roots.
    pub(crate) fn uses_named_policy(
        &self,
        role: super::super::VolumeRole,
        source: &str,
        groups: &[&str],
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(
            groups.len() <= super::super::RULES_MAX,
            "too many source groups"
        );
        let request = PlacementRequest {
            role,
            source,
            group: "",
            required_bytes: 0,
        };
        Ok(self
            .inner
            .configuration
            .matching_rule(&request, groups)
            .is_some())
    }

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
        let Reply::Location(source) = self
            .inner
            .catalog
            .volume_location(Request::Lookup(object.clone()))?
        else {
            anyhow::bail!("invalid move source reply");
        };
        let (source, legacy) = match source {
            Some(source) => (source, None),
            None => self.preview_legacy_recording(object)?,
        };
        self.make_move_preview(source, legacy, destination, request, groups)
    }

    fn make_move_preview(
        &self,
        source: Location,
        legacy: Option<legacy::Preview>,
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
        let preview = MovePreview {
            source,
            legacy,
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
        if preview.legacy.is_some() {
            return self.admit_legacy_media(job_id, preview);
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
        object_extension(preview.role, preview.source.object.kind)?;
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
