//! Preserves configured secret references in volume identifiers on the wire.

use super::*;

pub(super) struct Names(Vec<(String, String)>);

impl Names {
    pub(super) fn load(state: &ServerState) -> Result<Self> {
        if let Some(path) = &state.camera_config_path {
            let config = crate::config::load_config(path).map_err(failure)?;
            let resolved = config.storage.named_volumes.as_ref();
            let public = super::super::sanitized(&config);
            return Ok(Self(resolved.zip(public).map_or_else(
                Vec::new,
                |(resolved, public)| {
                    resolved
                        .volumes
                        .iter()
                        .zip(public.volumes)
                        .map(|(private, public)| (private.id.to_string(), public.id))
                        .collect()
                },
            )));
        }
        Ok(Self(
            state
                .storage_config
                .volume_runtime
                .as_ref()
                .map_or_else(Vec::new, |manager| {
                    manager
                        .configuration()
                        .volumes
                        .iter()
                        .map(|volume| (volume.id.to_string(), volume.id.to_string()))
                        .collect()
                }),
        ))
    }

    pub(super) fn resolve(
        &self,
        mut action: Option<proto::storage_volume_command::Action>,
    ) -> Result<Option<proto::storage_volume_command::Action>> {
        use proto::storage_volume_command::Action;
        if matches!(&action, Some(Action::Objects(value)) if legacy_id(&value.volume_id)) {
            return Ok(action);
        }
        let id = match &mut action {
            Some(Action::Probe(value)) => Some(&mut value.volume_id),
            Some(Action::Objects(value)) => Some(&mut value.volume_id),
            Some(Action::PreviewMetadata(value)) => Some(&mut value.destination_volume_id),
            Some(Action::PreviewMove(value)) => Some(&mut value.destination_volume_id),
            Some(Action::SetDraining(value)) => Some(&mut value.volume_id),
            _ => None,
        };
        if let Some(id) = id {
            *id = self
                .0
                .iter()
                .find(|(_, public)| public == id)
                .map(|(private, _)| private.clone())
                .ok_or_else(|| {
                    error(proto::ErrorCode::NotFound, 404, "volume is not configured")
                })?;
        }
        Ok(action)
    }

    pub(super) fn private(&self, public: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(_, name)| name == public)
            .map(|(private, _)| private.as_str())
    }

    fn redact(&self, id: &mut String) {
        if legacy_id(id) {
            return;
        }
        *id = self
            .0
            .iter()
            .find(|(private, _)| private == id)
            .map_or_else(|| "Removed volume".into(), |(_, public)| public.clone());
    }

    fn location(&self, location: &mut Option<proto::StorageObjectLocation>) {
        if let Some(location) = location {
            self.redact(&mut location.volume_id);
        }
    }

    fn job(&self, job: &mut proto::StorageMoveJob) {
        self.location(&mut job.source);
        self.redact(&mut job.destination_volume_id);
    }

    pub(super) fn response(&self, result: &mut proto::storage_volume_result::Result) {
        use proto::storage_volume_result::Result as Wire;
        match result {
            Wire::LegacyObjects(_) => {}
            Wire::MetadataPreview(value) => self.redact(&mut value.destination_volume_id),
            Wire::Metadata(value) => {
                if let Some(id) = &mut value.current_volume_id {
                    self.redact(id);
                }
                if let Some(id) = &mut value.pending_volume_id {
                    self.redact(id);
                }
            }
            Wire::Volumes(value) => {
                for volume in &mut value.volumes {
                    self.redact(&mut volume.volume_id);
                }
            }
            Wire::Probe(value) => self.redact(&mut value.volume_id),
            Wire::Placement(value) => {
                if let Some(id) = &mut value.selected_volume_id {
                    self.redact(id);
                }
                for rejected in &mut value.rejected {
                    self.redact(&mut rejected.volume_id);
                }
            }
            Wire::Objects(value) => {
                for object in &mut value.objects {
                    self.redact(&mut object.volume_id);
                }
            }
            Wire::Preview(value) => {
                self.location(&mut value.source);
                self.redact(&mut value.destination_volume_id);
            }
            Wire::Job(value) => self.job(value),
            Wire::Jobs(value) => {
                for job in &mut value.jobs {
                    self.job(job);
                }
            }
        }
    }
}

fn legacy_id(id: &str) -> bool {
    locations::legacy::roots::Role::ALL
        .into_iter()
        .any(|role| role.id() == id)
}
