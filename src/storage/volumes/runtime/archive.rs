use super::*;
use crate::storage::catalog::locations::archives::{Job, Policy};

impl Manager {
    pub(super) fn archive_policy(&self, source: &str, groups: &[&str]) -> Option<Policy> {
        let rule = self
            .inner
            .configuration
            .matching_rule(
                &PlacementRequest {
                    role: VolumeRole::Archive,
                    source,
                    group: "",
                    required_bytes: 0,
                },
                groups,
            )?
            .clone();
        let volumes = self
            .inner
            .configuration
            .volumes
            .iter()
            .filter(|volume| rule.candidates.contains(&volume.id))
            .cloned()
            .collect();
        Some(Policy {
            configuration: VolumeConfiguration {
                volumes,
                placement: vec![rule],
            },
            source: source.to_owned(),
            groups: groups.iter().map(|group| (*group).to_owned()).collect(),
        })
    }

    pub(super) fn admit_archive(&self, id: &str) -> anyhow::Result<bool> {
        let Reply::Archive(job) = self
            .inner
            .catalog
            .volume_location(Request::Archive(id.to_owned()))?
        else {
            anyhow::bail!("invalid archive journal reply");
        };
        let Some(job) = job else { return Ok(true) };
        let _guard = self
            .inner
            .admission
            .lock()
            .map_err(|_| anyhow::anyhow!("volume admission unavailable"))?;
        let selected = self.archive_destination(&job)?;
        if selected.as_str() == job.source.volume {
            self.inner
                .catalog
                .volume_location(Request::CompleteArchive {
                    id: id.to_owned(),
                    source: job.source,
                })?;
            return Ok(false);
        }
        let index = self
            .inner
            .configuration
            .volumes
            .iter()
            .position(|volume| volume.id == selected)
            .expect("available configured archive destination");
        self.commit_move(
            index,
            job.source.clone(),
            job.source.object,
            id,
            VolumeRole::Archive,
        )?;
        Ok(true)
    }

    fn archive_destination(&self, job: &Job) -> anyhow::Result<super::super::VolumeId> {
        let configuration = self.archive_configuration(&job.policy);
        let groups = job
            .policy
            .groups
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let observations = self
            .inner
            .observations(&configuration.placement[0].candidates)?;
        let request = PlacementRequest {
            role: VolumeRole::Archive,
            source: &job.policy.source,
            group: "",
            required_bytes: job.source.bytes,
        };
        let rule = &configuration.placement[0];
        if (!rule.allow_fallback || rule.candidates.len() == 1)
            && rule.candidates[0].as_str() == job.source.volume
            && configuration.matching_rule(&request, &groups).is_some()
        {
            let volume = configuration
                .volumes
                .iter()
                .find(|volume| volume.id == rule.candidates[0])
                .expect("captured archive candidate exists");
            let observation = observations.iter().find(|sample| sample.id == volume.id);
            // Keeping an existing recording requires no additional allocation.
            if super::super::placement::eligibility(
                volume,
                &PlacementRequest {
                    required_bytes: 0,
                    ..request
                },
                &groups,
                observation,
            )
            .is_ok()
            {
                return Ok(volume.id.clone());
            }
        }
        let decision = configuration.place_with_groups(&request, &groups, &observations)?;
        let Some(selected) = decision.selected else {
            self.request_pressure_retention(&decision.rejected, request.required_bytes);
            anyhow::bail!("archive policy has no writable destination");
        };
        Ok(selected)
    }

    fn archive_configuration(&self, policy: &Policy) -> VolumeConfiguration {
        let mut configuration = policy.configuration.clone();
        for captured in &mut configuration.volumes {
            let Some(current) = self
                .inner
                .configuration
                .volumes
                .iter()
                .find(|volume| volume.id == captured.id && volume.root == captured.root)
            else {
                captured.state = VolumeState::Disabled;
                continue;
            };
            captured.state = if current.roles.contains(&VolumeRole::Archive) {
                current.state
            } else {
                VolumeState::Disabled
            };
            // Destination policy stays captured; current write limits remain authoritative.
            captured.capacity_bytes = current.capacity_bytes;
            captured.minimum_free_bytes = current.minimum_free_bytes;
            captured.warning_free_bytes = current.warning_free_bytes;
            captured.critical_free_bytes = current.critical_free_bytes;
        }
        configuration
    }
}
