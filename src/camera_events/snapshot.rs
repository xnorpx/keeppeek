use std::net::IpAddr;
use std::sync::{
    Arc,
    mpsc::{self, Receiver, SyncSender},
};
use std::thread::JoinHandle;
use std::time::Duration;

use onvif::{
    event::{Client, Endpoint},
    soap::client::Credentials,
};

use super::registry::{Input, Registry, Slot};
use crate::{cameras::Camera, shutdown::Shutdown};

pub(super) struct Job {
    pub camera_id: String,
    pub event_id: String,
}

pub(super) fn spawn(
    camera: &Camera,
    slot: Arc<Slot>,
    registry: Registry,
    shutdown: Shutdown,
) -> anyhow::Result<Option<(SyncSender<Job>, JoinHandle<()>)>> {
    if !camera.config.events.snapshots {
        return Ok(None);
    }
    let ip = camera.config.ip;
    let address = std::net::SocketAddr::new(
        ip,
        camera.config.http_port.or(camera.ports.http).unwrap_or(80),
    );
    let origin = Endpoint::new(format!("http://{address}/"))?;
    let client = Client::new(
        origin,
        Credentials {
            username: camera.config.username.clone(),
            password: camera.config.password.clone(),
        },
    )?;
    let (sent, received) = mpsc::sync_channel(4);
    let handle = std::thread::Builder::new()
        .name(format!("event-snapshot-{ip}"))
        .spawn(move || run(client, ip, registry, received, slot, shutdown))?;
    Ok(Some((sent, handle)))
}

fn run(
    mut client: Client,
    ip: IpAddr,
    registry: Registry,
    received: Receiver<Job>,
    slot: Arc<Slot>,
    shutdown: Shutdown,
) {
    while !shutdown.is_cancelled() {
        let job = match received.recv_timeout(Duration::from_millis(100)) {
            Ok(job) => job,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        if shutdown.is_cancelled() {
            return;
        }
        let Some(endpoint) = registry.snapshot_endpoint(ip) else {
            slot.update(|evidence| evidence.snapshot_failures += 1);
            continue;
        };
        let jpeg = match client.snapshot(&endpoint, Duration::from_secs(2)) {
            Ok(jpeg) if crate::storage::events::jpeg_dimensions(&jpeg).is_ok() => jpeg,
            _ => {
                slot.update(|evidence| evidence.snapshot_failures += 1);
                continue;
            }
        };
        if shutdown.is_cancelled() {
            return;
        }
        if slot
            .try_send(Input::Snapshot {
                camera_id: job.camera_id,
                event_id: job.event_id,
                jpeg,
            })
            .is_ok()
        {
            slot.update(|evidence| evidence.snapshots += 1);
        } else {
            slot.update(|evidence| evidence.snapshot_failures += 1);
        }
    }
}

#[cfg(test)]
mod tests;
