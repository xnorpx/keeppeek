use bytes::Bytes;
use clap::{Parser, ValueEnum};
use keeppeek::{
    cameras::{
        Camera, CameraBackend, CameraCapabilities, CameraConfig, CameraPorts, CameraTransport,
        DeviceInfo, MediaProfile, VideoConfig, VideoEncoding,
    },
    config,
    homekit::{HomeKitProbeProfile, HomeKitService},
    keeppeek::StreamKind,
    shutdown::Shutdown,
    storage::VideoCodec,
    webrtc::{Source, WebRtc},
};
use mp4::{MediaType, Mp4Reader, Mp4Sample};
use std::{
    collections::HashMap,
    fs::File,
    io::{BufReader, IsTerminal},
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tracing_subscriber::EnvFilter;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const DEFAULT_FIXTURE: &str = "crates/test-camera/testdata/cc-4k-1920x1080-h264.mp4";

/// Loopback address the synthetic accessory is keyed on; no packets are sent to it.
const SYNTHETIC_CAMERA_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

#[derive(Debug, Parser)]
#[command(
    name = "homekit-file-stream",
    about = "Serve a HomeKit camera accessory backed by H.264 frames read from an MP4 file"
)]
struct Cli {
    /// Configuration file supplying the `[homekit]` block and state directory.
    #[arg(short, long, default_value_os_t = config::config_path())]
    config: PathBuf,

    /// MP4 file whose H.264 track is looped to the Home app.
    #[arg(long, default_value = DEFAULT_FIXTURE)]
    file: PathBuf,

    /// Accessory name shown in the Home app.
    #[arg(long, default_value = "KeepPeek File Camera")]
    name: String,

    /// Overrides the `[homekit] bind` address.
    #[arg(long)]
    bind: Option<IpAddr>,

    /// Overrides the `[homekit] port`; 0 picks an ephemeral port.
    #[arg(long)]
    port: Option<u16>,

    /// FFmpeg executable used for H.264 RTP/SRTP streaming.
    #[arg(long, default_value = "ffmpeg")]
    ffmpeg: PathBuf,

    /// Accessory database to advertise.
    #[arg(long, value_enum, default_value_t = Profile::Legacy)]
    profile: Profile,

    /// Advertise and encode HEVC instead of H.264.
    #[arg(long)]
    hevc: bool,

    /// Persistent accessory identity. Defaults to the already-paired file camera.
    #[arg(long, default_value = "file-stream-1920x1080")]
    uid: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Profile {
    Legacy,
    Webrtc,
}

impl From<Profile> for HomeKitProbeProfile {
    fn from(value: Profile) -> Self {
        match value {
            Profile::Legacy => Self::Legacy,
            Profile::Webrtc => Self::WebRtc,
        }
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_ansi(std::io::stderr().is_terminal())
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("info,keeppeek::homekit=debug,keeppeek::webrtc=debug")
        }))
        .init();

    let cli = Cli::parse();
    let source = FileVideoSource::load(&cli.file)?;
    tracing::info!(
        file = %cli.file.display(),
        width = source.width,
        height = source.height,
        fps = source.fps,
        frames = source.sample_count,
        "loaded fixture"
    );

    let config_text = std::fs::read_to_string(&cli.config)?;
    let app_config: config::Config = toml::from_str(&config_text)?;
    let mut homekit = app_config.homekit;
    homekit.enabled = true;
    if let Some(bind) = cli.bind {
        homekit.bind = bind;
    }
    if let Some(port) = cli.port {
        homekit.port = port;
    }

    let cameras = HashMap::from([(
        SYNTHETIC_CAMERA_IP,
        synthetic_camera(&cli.name, &source, cli.hevc, &cli.uid),
    )]);
    let webrtc = WebRtc::new();
    let publisher = webrtc.live();
    let file_path = cli.file.clone();
    let shutdown = Shutdown::new();
    let signal_shutdown = shutdown.clone();
    ctrlc::set_handler(move || signal_shutdown.cancel())
        .map_err(|error| anyhow::anyhow!("unable to install Ctrl+C handler: {error}"))?;
    let publisher_shutdown = shutdown.clone();
    let publisher_thread = std::thread::Builder::new()
        .name("homekit-file-publisher".to_owned())
        .spawn(move || publish_file_frames(&file_path, &publisher, &publisher_shutdown))
        .map_err(|error| anyhow::anyhow!("unable to start file publisher: {error}"))?;

    let service = HomeKitService::start_legacy_file(
        &homekit,
        &cli.config,
        &cameras,
        webrtc,
        &cli.ffmpeg,
        &cli.file,
        shutdown.clone(),
        cli.profile.into(),
        cli.hevc,
    )?
    .ok_or_else(|| anyhow::anyhow!("HomeKit did not start; check the [homekit] configuration"))?;

    let state_directory = cli
        .config
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("homekit");
    for address in service.addresses() {
        println!("listening on {address}");
    }
    println!("setup codes and QR codes: {}", state_directory.display());
    println!(
        "add '{}' in the Home app, then open its live view",
        cli.name
    );
    println!("press Ctrl+C to stop");

    while !shutdown.is_cancelled() {
        shutdown.wait_timeout(Duration::from_millis(250));
    }

    service.join();
    join_publisher(publisher_thread);
    Ok(())
}

fn join_publisher(handle: JoinHandle<anyhow::Result<()>>) {
    match handle.join() {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!(%error, "file publisher stopped with error"),
        Err(_) => tracing::warn!("file publisher panicked"),
    }
}

fn publish_file_frames(
    path: &Path,
    publisher: &keeppeek::webrtc::Publisher,
    shutdown: &Shutdown,
) -> anyhow::Result<()> {
    let source = LoadedFile::open(path)?;
    tracing::info!(
        file = %path.display(),
        frames = source.samples.len(),
        fps = source.fps,
        codec = ?source.codec,
        "publishing fixture into HomeKit WebRTC"
    );
    let source_id = Source {
        camera_ip: SYNTHETIC_CAMERA_IP,
        stream: StreamKind::Main,
    };
    let frame_period = Duration::from_nanos(1_000_000_000 / u64::from(source.fps.max(1)));
    let mut next_due = Instant::now();
    let mut index = 0_usize;
    while !shutdown.is_cancelled() {
        let sample = &source.samples[index];
        publisher.publish(
            source_id,
            source.codec,
            sample.is_keyframe,
            Instant::now(),
            Some(sample.timestamp),
            sample.avcc.clone(),
        );
        index += 1;
        if index == source.samples.len() {
            index = 0;
        }
        next_due += frame_period;
        let now = Instant::now();
        if next_due > now && shutdown.wait_timeout(next_due - now) {
            break;
        }
        if now > next_due + frame_period {
            next_due = now;
        }
    }
    Ok(())
}

struct LoadedSample {
    timestamp: Duration,
    is_keyframe: bool,
    avcc: Bytes,
}

struct LoadedFile {
    codec: VideoCodec,
    fps: u8,
    samples: Vec<LoadedSample>,
}

impl LoadedFile {
    fn open(path: &Path) -> anyhow::Result<Self> {
        let file = File::open(path)
            .map_err(|error| anyhow::anyhow!("cannot open {}: {error}", path.display()))?;
        let size = file.metadata()?.len();
        let mut reader = Mp4Reader::read_header(BufReader::new(file), size)?;
        let track = reader
            .tracks()
            .values()
            .find(|track| matches!(track.media_type(), Ok(MediaType::H264)))
            .ok_or_else(|| anyhow::anyhow!("{} has no H.264 track", path.display()))?;
        let track_id = track.track_id();
        let fps = {
            let frame_rate = track.frame_rate();
            if frame_rate.is_finite() && frame_rate >= 1.0 {
                frame_rate.round().clamp(1.0, 255.0) as u8
            } else {
                30
            }
        };
        let timescale = u64::from(track.timescale().max(1));
        let sps = prepend_avcc_length(track.sequence_parameter_set()?);
        let pps = prepend_avcc_length(track.picture_parameter_set()?);
        let sample_count = reader.sample_count(track_id)?;
        anyhow::ensure!(sample_count > 0, "{} has no samples", path.display());
        let mut samples = Vec::with_capacity(sample_count as usize);
        for sample_id in 1..=sample_count {
            let Some(sample) = reader.read_sample(track_id, sample_id)? else {
                continue;
            };
            samples.push(LoadedSample::from_h264(&sample, timescale, &sps, &pps));
        }
        anyhow::ensure!(
            !samples.is_empty(),
            "{} produced no readable samples",
            path.display()
        );
        Ok(Self {
            codec: VideoCodec::H264,
            fps,
            samples,
        })
    }
}

impl LoadedSample {
    fn from_h264(sample: &Mp4Sample, timescale: u64, sps: &[u8], pps: &[u8]) -> Self {
        let timestamp =
            Duration::from_nanos(sample.start_time.saturating_mul(1_000_000_000) / timescale);
        let mut avcc = Vec::with_capacity(sps.len() + pps.len() + sample.bytes.len());
        if sample.is_sync {
            avcc.extend_from_slice(sps);
            avcc.extend_from_slice(pps);
        }
        avcc.extend_from_slice(&sample.bytes);
        Self {
            timestamp,
            is_keyframe: sample.is_sync,
            avcc: Bytes::from(avcc),
        }
    }
}

fn prepend_avcc_length(nalu: &[u8]) -> Vec<u8> {
    let mut value = Vec::with_capacity(4 + nalu.len());
    value.extend_from_slice(&(nalu.len() as u32).to_be_bytes());
    value.extend_from_slice(nalu);
    value
}

fn synthetic_camera(name: &str, source: &FileVideoSource, hevc: bool, uid: &str) -> Camera {
    Camera {
        config: CameraConfig {
            ip: SYNTHETIC_CAMERA_IP,
            name: Some(name.to_owned()),
            display_name: Some(name.to_owned()),
            manufacturer: Some("KeepPeek".to_owned()),
            username: "file".to_owned(),
            password: String::new(),
            onvif_port: None,
            http_port: None,
            main_rtsp_url: None,
            sub_rtsp_url: None,
            uid: Some(uid.to_owned()),
            backend: CameraBackend::Auto,
            transport: CameraTransport::Tcp,
        },
        device: DeviceInfo {
            manufacturer: Some("KeepPeek".to_owned()),
            model: Some("File Stream".to_owned()),
            firmware_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            serial_number: Some("file-stream-0".to_owned()),
            ..DeviceInfo::default()
        },
        reported_manufacturer: Some("KeepPeek".to_owned()),
        hostname: None,
        mac_address: None,
        ports: CameraPorts::default(),
        capabilities: CameraCapabilities::default(),
        // The advertised tiers are derived from these dimensions, so they must
        // match the file or the controller selects a tier that cannot be served.
        profiles: vec![MediaProfile {
            token: "mainStream".to_owned(),
            name: "mainStream".to_owned(),
            stream_uri: None,
            snapshot_uri: None,
            video: Some(VideoConfig {
                encoding: if hevc {
                    VideoEncoding::H265
                } else {
                    VideoEncoding::H264
                },
                width: u32::from(source.width),
                height: u32::from(source.height),
                framerate: f64::from(source.fps),
                bitrate_kbps: Some(2_000),
                quality: None,
                gov_length: None,
                h264_profile: Some("Baseline".to_owned()),
            }),
            audio: None,
        }],
        is_reolink: false,
        ptz: None,
        imaging: None,
    }
}

struct FileVideoSource {
    width: u16,
    height: u16,
    fps: u8,
    sample_count: u32,
}

impl FileVideoSource {
    fn load(path: &Path) -> anyhow::Result<Self> {
        let file = File::open(path)
            .map_err(|error| anyhow::anyhow!("cannot open {}: {error}", path.display()))?;
        let size = file.metadata()?.len();
        let reader = Mp4Reader::read_header(BufReader::new(file), size)?;

        let (track_id, width, height, fps) = {
            // HomeKit only accepts H.264 on this path, but FFmpeg transcodes the
            // source, so an H.265 file is a valid input for the same tiers.
            let track = reader
                .tracks()
                .values()
                .find(|track| matches!(track.media_type(), Ok(MediaType::H264 | MediaType::H265)))
                .ok_or_else(|| anyhow::anyhow!("{} has no H.264 or H.265 track", path.display()))?;
            let frame_rate = track.frame_rate();
            let fps = if frame_rate.is_finite() && frame_rate >= 1.0 {
                frame_rate.round().clamp(1.0, 255.0) as u8
            } else {
                30
            };
            (track.track_id(), track.width(), track.height(), fps)
        };
        let sample_count = reader.sample_count(track_id)?;
        anyhow::ensure!(sample_count > 0, "{} has no samples", path.display());
        Ok(Self {
            width,
            height,
            fps,
            sample_count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_the_checked_in_fixture() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(DEFAULT_FIXTURE);
        let source = FileVideoSource::load(&path).unwrap();
        assert_eq!((source.width, source.height), (1920, 1080));
        assert_eq!(source.fps, 30);
        assert!(source.sample_count > 0);
    }

    #[test]
    fn loads_an_h265_source_for_transcoding() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("crates/test-camera/testdata/cc-4k-640x360-h265.mp4");
        let source = FileVideoSource::load(&path).unwrap();
        assert_eq!((source.width, source.height), (640, 360));
        assert!(source.sample_count > 0);
    }

    #[test]
    fn loads_h264_samples_with_in_band_parameter_sets() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(DEFAULT_FIXTURE);
        let source = LoadedFile::open(&path).unwrap();
        assert_eq!(source.codec, VideoCodec::H264);
        assert!(!source.samples.is_empty());
        let keyframe = source
            .samples
            .iter()
            .find(|sample| sample.is_keyframe)
            .unwrap();
        let annexb = keeppeek::storage::nal::avcc_to_annexb(&keyframe.avcc);
        assert!(
            annexb
                .windows(5)
                .any(|window| window[0..4] == [0, 0, 0, 1] && window[4] & 0x1f == 7)
        );
    }
}
