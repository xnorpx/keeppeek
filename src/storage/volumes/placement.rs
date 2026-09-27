use super::{
    PlacementDecision, PlacementRequest, PlacementRule, PlacementStrategy, RejectedVolume,
    RejectionReason, VOLUMES_MAX, Volume, VolumeConfiguration, VolumeHealth, VolumeObservation,
    VolumeState,
};

impl VolumeConfiguration {
    /// Selects a destination from bounded capacity evidence without performing I/O.
    ///
    /// # Errors
    /// Rejects invalid configuration, malformed observations, and zero-byte allocations.
    /// A missing rule or unavailable pool returns a decision without a selected volume.
    pub fn place(
        &self,
        request: &PlacementRequest<'_>,
        observations: &[VolumeObservation],
    ) -> anyhow::Result<PlacementDecision> {
        self.validate()?;
        anyhow::ensure!(
            request.required_bytes > 0,
            "placement must request at least one byte"
        );
        validate_observations(observations)?;
        let Some(rule) = self.matching_rule(request) else {
            return Ok(PlacementDecision {
                selected: None,
                rejected: Vec::new(),
            });
        };
        let count = if rule.allow_fallback {
            rule.candidates.len()
        } else {
            1
        };
        let mut rejected = Vec::with_capacity(count);
        let mut eligible = Vec::with_capacity(count);
        for id in rule.candidates.iter().take(count) {
            let volume = self
                .volumes
                .iter()
                .find(|volume| volume.id == *id)
                .expect("validated placement candidates name existing volumes");
            let observation = observations
                .iter()
                .find(|observation| observation.id == *id);
            match eligibility(volume, request, observation) {
                Ok(available) => eligible.push((volume, available)),
                Err(reason) => rejected.push(RejectedVolume {
                    id: id.clone(),
                    reason,
                }),
            }
        }
        eligible.sort_unstable_by(|(left, left_free), (right, right_free)| {
            let order = match rule.strategy {
                PlacementStrategy::Priority => left.priority.cmp(&right.priority),
                PlacementStrategy::FreeSpace => right_free.cmp(left_free),
            };
            order.then_with(|| left.id.cmp(&right.id))
        });
        Ok(PlacementDecision {
            selected: eligible.first().map(|(volume, _)| volume.id.clone()),
            rejected,
        })
    }

    fn matching_rule(&self, request: &PlacementRequest<'_>) -> Option<&PlacementRule> {
        self.placement
            .iter()
            .filter(|rule| {
                rule.role == request.role
                    && rule
                        .source
                        .as_deref()
                        .is_none_or(|source| source == request.source)
                    && rule
                        .group
                        .as_deref()
                        .is_none_or(|group| group == request.group)
            })
            .max_by_key(|rule| {
                if rule.source.is_some() {
                    2
                } else {
                    u8::from(rule.group.is_some())
                }
            })
    }
}

fn validate_observations(observations: &[VolumeObservation]) -> anyhow::Result<()> {
    anyhow::ensure!(
        observations.len() <= VOLUMES_MAX,
        "at most 32 volume observations are allowed"
    );
    for (index, observation) in observations.iter().enumerate() {
        anyhow::ensure!(
            observations[..index]
                .iter()
                .all(|other| other.id != observation.id),
            "duplicate volume observation"
        );
        anyhow::ensure!(
            observation.available_bytes <= observation.total_bytes,
            "available volume space exceeds total space"
        );
    }
    Ok(())
}

fn eligibility(
    volume: &Volume,
    request: &PlacementRequest<'_>,
    observation: Option<&VolumeObservation>,
) -> Result<u64, RejectionReason> {
    match volume.state {
        VolumeState::Enabled => {}
        VolumeState::Disabled => return Err(RejectionReason::Disabled),
        VolumeState::Draining => return Err(RejectionReason::Draining),
        VolumeState::ReadOnly => return Err(RejectionReason::ReadOnly),
    }
    if !volume.roles.contains(&request.role) {
        return Err(RejectionReason::RoleMismatch);
    }
    if (!volume.sources.is_empty() || !volume.groups.is_empty())
        && !volume.sources.iter().any(|source| source == request.source)
        && !volume.groups.iter().any(|group| group == request.group)
    {
        return Err(RejectionReason::SourceDenied);
    }
    let observation = observation.ok_or(RejectionReason::MissingObservation)?;
    match observation.health {
        VolumeHealth::Online => {}
        VolumeHealth::Offline => return Err(RejectionReason::Offline),
        VolumeHealth::ReadOnly => return Err(RejectionReason::ReadOnly),
    }
    let available = observation
        .available_bytes
        .saturating_sub(volume.minimum_free_bytes.max(volume.critical_free_bytes));
    if available < request.required_bytes {
        return Err(RejectionReason::InsufficientSpace);
    }
    let remaining_cap = volume
        .capacity_bytes
        .map_or(u64::MAX, |cap| cap.saturating_sub(observation.owned_bytes));
    if remaining_cap < request.required_bytes {
        return Err(RejectionReason::CapacityExceeded);
    }
    Ok(available.min(remaining_cap))
}
