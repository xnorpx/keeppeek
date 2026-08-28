use super::{
    ControlCommandError, ServerState, discover_camera_settings, present_discovered_cameras,
    proto_camera_discovery_result,
};
use crate::{
    api::proto::{self, ok as control_ok},
    cameras::{CameraDiscoveryNetwork, DiscoveredCamera},
    runtime::{FacadeSender, RouterMessage},
    webrtc::SessionId,
};
use std::{
    collections::{HashMap, HashSet},
    net::Ipv4Addr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

const MAX_TASKS: usize = 16;
type TaskKey = (SessionId, String);

pub(super) fn discover(
    state: &ServerState,
    router_tx: &FacadeSender<RouterMessage>,
    session_id: SessionId,
    request: proto::DiscoverCameras,
) -> Result<control_ok::Result, ControlCommandError> {
    let discovery_id = request.discovery_id.trim().to_owned();
    if discovery_id.len() > 128 || discovery_id.chars().any(char::is_control) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "camera discovery ID must be at most 128 printable characters",
        ));
    }
    let task = state
        .camera_discovery_tasks
        .start(session_id, &discovery_id)?;
    let networks = request
        .networks
        .iter()
        .map(|network| {
            let network = network.parse::<ipnet::Ipv4Net>().map_err(|_| {
                ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "camera discovery networks must be IPv4 CIDRs",
                )
            })?;
            if network.prefix_len() != 24 || !network.network().is_private() {
                return Err(ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "camera discovery networks must be private IPv4 /24 CIDRs",
                ));
            }
            Ok(network)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let subnets = request
        .subnets
        .into_iter()
        .map(|subnet| {
            u8::try_from(subnet).map_err(|_| {
                ControlCommandError::new(
                    proto::ErrorCode::InvalidRequest,
                    400,
                    "camera discovery subnet prefixes must be between 0 and 255",
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let cameras = discover_camera_settings(networks, subnets, router_tx, state, task.as_ref())?;
    let cancelled = task.as_ref().is_some_and(TaskHandle::finish);
    Ok(control_ok::Result::CameraDiscoveryResult(
        proto_camera_discovery_result(discovery_id, cameras, true, cancelled),
    ))
}

pub(super) fn get(
    state: &ServerState,
    router_tx: &FacadeSender<RouterMessage>,
    session_id: SessionId,
    request: proto::GetCameraDiscovery,
) -> Result<control_ok::Result, ControlCommandError> {
    let task = state
        .camera_discovery_tasks
        .snapshot(session_id, &request.discovery_id)?;
    let cameras = present_discovered_cameras(task.cameras, router_tx, state);
    Ok(control_ok::Result::CameraDiscoveryResult(
        proto_camera_discovery_result(request.discovery_id, cameras, task.complete, task.cancelled),
    ))
}

pub(super) fn cancel(
    state: &ServerState,
    router_tx: &FacadeSender<RouterMessage>,
    session_id: SessionId,
    request: proto::CancelCameraDiscovery,
) -> Result<control_ok::Result, ControlCommandError> {
    let cameras = state
        .camera_discovery_tasks
        .cancel(session_id, &request.discovery_id)?;
    let cameras = present_discovered_cameras(cameras, router_tx, state);
    Ok(control_ok::Result::CameraDiscoveryResult(
        proto_camera_discovery_result(request.discovery_id, cameras, false, true),
    ))
}

pub(super) fn prefer_configured_networks(
    mut networks: Vec<CameraDiscoveryNetwork>,
    configured_ips: impl IntoIterator<Item = Ipv4Addr>,
) -> Vec<CameraDiscoveryNetwork> {
    let configured_networks = configured_ips
        .into_iter()
        .filter(Ipv4Addr::is_private)
        .filter_map(|ip| {
            ipnet::Ipv4Net::new(ip, 24)
                .ok()
                .map(|network| network.trunc())
        })
        .collect::<HashSet<_>>();
    if configured_networks.is_empty() {
        return networks;
    }
    for network in &mut networks {
        network.preferred = false;
    }
    for network in configured_networks {
        let cidr = network.to_string();
        if let Some(existing) = networks.iter_mut().find(|item| item.cidr == cidr) {
            existing.preferred = true;
        } else {
            networks.push(CameraDiscoveryNetwork {
                cidr,
                interface_name: "configured cameras".to_owned(),
                preferred: true,
            });
        }
    }
    networks.sort_by(|left, right| {
        right
            .preferred
            .cmp(&left.preferred)
            .then_with(|| left.cidr.cmp(&right.cidr))
    });
    networks
}

#[derive(Clone)]
struct Task {
    cameras: Vec<DiscoveredCamera>,
    complete: bool,
    cancelled: Arc<AtomicBool>,
}

pub(super) struct Snapshot {
    pub(super) cameras: Vec<DiscoveredCamera>,
    pub(super) complete: bool,
    pub(super) cancelled: bool,
}

#[derive(Clone)]
pub(super) struct TaskHandle {
    tasks: Arc<Mutex<HashMap<TaskKey, Task>>>,
    key: TaskKey,
    cancelled: Arc<AtomicBool>,
}

impl TaskHandle {
    pub(super) fn cancellation_token(&self) -> Arc<AtomicBool> {
        self.cancelled.clone()
    }

    pub(super) fn update(&self, cameras: &[DiscoveredCamera]) {
        if let Some(task) = self
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&self.key)
        {
            task.cameras = cameras.to_vec();
        }
    }

    pub(super) fn finish(&self) -> bool {
        let cancelled = self
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&self.key)
            .map(|task| {
                task.complete = true;
                task.cancelled.load(Ordering::Acquire)
            })
            .unwrap_or(false);
        self.tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.key);
        cancelled
    }

    #[cfg(test)]
    pub(super) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[derive(Clone, Default)]
pub(super) struct Registry {
    tasks: Arc<Mutex<HashMap<TaskKey, Task>>>,
}

impl Registry {
    pub(super) fn start(
        &self,
        session_id: SessionId,
        discovery_id: &str,
    ) -> Result<Option<TaskHandle>, ControlCommandError> {
        if discovery_id.is_empty() {
            return Ok(None);
        }
        let key = (session_id, discovery_id.to_owned());
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut tasks = self
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if tasks.len() >= MAX_TASKS && !tasks.contains_key(&key) {
            return Err(ControlCommandError::new(
                proto::ErrorCode::Rejected,
                429,
                "too many camera discovery tasks are active",
            ));
        }
        tasks.insert(
            key.clone(),
            Task {
                cameras: Vec::new(),
                complete: false,
                cancelled: cancelled.clone(),
            },
        );
        Ok(Some(TaskHandle {
            tasks: self.tasks.clone(),
            key,
            cancelled,
        }))
    }

    pub(super) fn snapshot(
        &self,
        session_id: SessionId,
        discovery_id: &str,
    ) -> Result<Snapshot, ControlCommandError> {
        let task = self
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(session_id, discovery_id.to_owned()))
            .cloned()
            .ok_or_else(task_not_found)?;
        Ok(Snapshot {
            cameras: task.cameras,
            complete: task.complete,
            cancelled: task.cancelled.load(Ordering::Acquire),
        })
    }

    pub(super) fn cancel(
        &self,
        session_id: SessionId,
        discovery_id: &str,
    ) -> Result<Vec<DiscoveredCamera>, ControlCommandError> {
        let task = self
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(session_id, discovery_id.to_owned()))
            .cloned()
            .ok_or_else(task_not_found)?;
        task.cancelled.store(true, Ordering::Release);
        Ok(task.cameras)
    }

    pub(super) fn close_session(&self, session_id: SessionId) {
        let cancelled = {
            let mut tasks = self
                .tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let cancelled = tasks
                .iter()
                .filter(|((owner_session_id, _), _)| *owner_session_id == session_id)
                .map(|(_, task)| task.cancelled.clone())
                .collect::<Vec<_>>();
            tasks.retain(|(owner_session_id, _), _| *owner_session_id != session_id);
            cancelled
        };
        for token in cancelled {
            token.store(true, Ordering::Release);
        }
    }
}

fn task_not_found() -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::InvalidRequest,
        404,
        "camera discovery task was not found",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network(cidr: &str, interface_name: &str, preferred: bool) -> CameraDiscoveryNetwork {
        CameraDiscoveryNetwork {
            cidr: cidr.to_owned(),
            interface_name: interface_name.to_owned(),
            preferred,
        }
    }

    #[test]
    fn configured_camera_networks_override_local_preference_and_add_missing_subnets() {
        let networks = vec![
            network("192.168.1.0/24", "en0", true),
            network("10.0.0.0/24", "en1", false),
        ];

        let preferred = prefer_configured_networks(
            networks,
            [
                Ipv4Addr::new(10, 0, 0, 42),
                Ipv4Addr::new(172, 16, 2, 8),
                Ipv4Addr::new(8, 8, 8, 8),
            ],
        );

        assert_eq!(
            preferred,
            vec![
                network("10.0.0.0/24", "en1", true),
                network("172.16.2.0/24", "configured cameras", true),
                network("192.168.1.0/24", "en0", false),
            ]
        );
    }

    #[test]
    fn no_private_configured_camera_network_preserves_local_order_and_preference() {
        let networks = vec![
            network("192.168.2.0/24", "en1", false),
            network("192.168.1.0/24", "en0", true),
        ];

        let preferred =
            prefer_configured_networks(networks.clone(), [Ipv4Addr::new(203, 0, 113, 8)]);

        assert_eq!(preferred, networks);
    }
}
