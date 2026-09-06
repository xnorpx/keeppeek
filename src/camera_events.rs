mod consumer;
mod lifecycle;
mod metadata;
mod pullpoint;
mod registry;
mod snapshot;

pub use registry::{Evidence, Registry};

use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;

use crate::cameras::Camera;
use crate::keeppeek::KeepPeekEvent;
use crate::shutdown::Shutdown;

pub fn spawn(
    camera: &Camera,
    sent: mpsc::SyncSender<KeepPeekEvent>,
    registry: Registry,
    shutdown: Shutdown,
) -> anyhow::Result<Vec<JoinHandle<()>>> {
    camera.config.events.validate(camera.config.ip)?;
    let mut camera = camera.clone();
    if let Some(service) = &camera.event_service {
        registry.record_service(camera.config.ip, service);
    } else {
        camera.event_service = registry.service(camera.config.ip);
    }
    let camera = &camera;
    for profile in &camera.profiles {
        if registry.record_snapshot(camera.config.ip, profile.snapshot_uri.as_deref()) {
            break;
        }
    }
    let (input, received) = mpsc::sync_channel(32);
    let slot = registry.install(
        camera.config.ip,
        input,
        shutdown.clone(),
        camera.config.events.clone(),
    )?;
    let snapshot = snapshot::spawn(camera, Arc::clone(&slot), registry, shutdown.clone())?;
    let snapshots = snapshot.as_ref().map(|(sent, _)| sent.clone());
    let actor = consumer::Consumer::new(
        camera,
        sent,
        received,
        Arc::clone(&slot),
        shutdown.clone(),
        snapshots,
    );
    let actor = std::thread::Builder::new()
        .name(format!("camera-events-{}", camera.config.ip))
        .spawn(move || actor.run())?;
    let producer = pullpoint::Producer::new(camera, slot, shutdown.clone());
    match std::thread::Builder::new()
        .name(format!("onvif-pull-{}", camera.config.ip))
        .spawn(move || producer.run())
    {
        Ok(producer) => {
            let mut handles = vec![actor, producer];
            if let Some((_, handle)) = snapshot {
                handles.push(handle);
            }
            Ok(handles)
        }
        Err(error) => {
            shutdown.cancel();
            actor
                .join()
                .expect("event consumer must stop after producer spawn fails");
            if let Some((_, handle)) = snapshot {
                handle
                    .join()
                    .expect("snapshot worker must stop after producer spawn fails");
            }
            Err(error.into())
        }
    }
}

pub fn unix_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
mod tests;
