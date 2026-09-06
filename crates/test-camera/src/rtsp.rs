use crate::{Protocol, Transport};
use anyhow::ensure;
use retina::server::{Mp4Playback, RtspServer};
use std::{
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    sync::Arc,
    time::Duration,
};

mod relay;

const DOCUMENTS_MAX: usize = 64;
const DOCUMENT_BYTES_MAX: usize = 1024 * 1024;
const STREAM_BYTES_MAX: usize = 8 * DOCUMENT_BYTES_MAX;
const ENCODING_BYTES_MAX: usize = 128;

#[derive(Debug, Clone)]
pub struct Metadata {
    encoding: String,
    documents: Vec<Vec<u8>>,
}

impl Metadata {
    pub(super) fn new(encoding: &str, documents: Vec<Vec<u8>>) -> Self {
        Self {
            encoding: encoding.to_owned(),
            documents,
        }
    }

    pub(super) fn validate(&self, protocol: Protocol, transport: Transport) -> anyhow::Result<()> {
        ensure!(
            protocol == Protocol::Rtsp,
            "metadata requires the RTSP camera protocol"
        );
        ensure!(
            transport == Transport::Tcp,
            "metadata requires TCP interleaved RTP"
        );
        ensure!(
            !self.encoding.is_empty()
                && self.encoding.len() <= ENCODING_BYTES_MAX
                && self.encoding.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-' | b'_')
                }),
            "metadata encoding must be an ASCII RTP encoding token of at most 128 bytes"
        );
        ensure!(
            self.documents.len() <= DOCUMENTS_MAX,
            "metadata must contain at most 64 documents"
        );
        let mut total_bytes = 0;
        for document in &self.documents {
            ensure!(
                document.len() <= DOCUMENT_BYTES_MAX,
                "metadata document exceeds 1 MiB"
            );
            total_bytes += document.len();
            ensure!(
                total_bytes <= STREAM_BYTES_MAX,
                "metadata stream exceeds 8 MiB"
            );
        }
        Ok(())
    }
}

pub struct Camera {
    pub(super) address: SocketAddr,
    pub(super) main_url: String,
    pub(super) sub_url: String,
    _relays: Vec<relay::Relay>,
    _servers: Vec<RtspServer>,
}

impl Camera {
    pub(super) fn start(
        bind_ip: Ipv4Addr,
        sources: [&Path; 2],
        start_at: Option<Duration>,
        metadata: Option<Metadata>,
    ) -> anyhow::Result<Self> {
        let address = SocketAddr::from((bind_ip, 0));
        if metadata.is_none() && start_at.is_none() {
            let server = RtspServer::from_mp4_streams_on(address, sources[0], sources[1])?;
            return Ok(Self {
                address: server.address(),
                main_url: server.high_resolution_url().to_string(),
                sub_url: server.low_resolution_url().to_string(),
                _relays: Vec::new(),
                _servers: vec![server],
            });
        }

        let metadata = metadata.map(|mut metadata| {
            metadata.documents.iter_mut().for_each(Vec::shrink_to_fit);
            metadata.documents.shrink_to_fit();
            Arc::new(metadata)
        });
        let playback = start_at.map_or_else(Mp4Playback::default, Mp4Playback::realtime_looping);
        let mut servers = Vec::with_capacity(2);
        let mut relays = Vec::with_capacity(2);
        let mut urls = Vec::with_capacity(2);
        for source in sources {
            let server = RtspServer::from_mp4_on_with_playback(address, source, playback)?;
            let mut url = server.url();
            if let Some(metadata) = &metadata {
                let relay = relay::Relay::start(server.address(), Arc::clone(metadata))?;
                url.set_port(Some(relay.address().port()))
                    .map_err(|()| anyhow::anyhow!("RTSP URL cannot accept a relay port"))?;
                relays.push(relay);
            }
            urls.push(url.to_string());
            servers.push(server);
        }
        Ok(Self {
            address: relays
                .first()
                .map_or_else(|| servers[0].address(), relay::Relay::address),
            main_url: urls.remove(0),
            sub_url: urls.remove(0),
            _relays: relays,
            _servers: servers,
        })
    }
}
