use anyhow::{Context, bail};
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

struct PlaybackTrack {
    source_id: u32,
    config: mp4::TrackConfig,
}

pub fn browser_compatible_recording(source: &Path) -> anyhow::Result<PathBuf> {
    let cached = source.with_extension("mp4.browser");
    if cached.is_file() {
        return Ok(cached);
    }

    let temporary = source.with_extension(format!("mp4.browser.{}.active", rand::random::<u64>()));
    let result = remux_recording(source, &temporary).and_then(|()| {
        if cached.exists() {
            std::fs::remove_file(&temporary)?;
        } else {
            std::fs::rename(&temporary, &cached)?;
        }
        Ok(cached.clone())
    });
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

fn remux_recording(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let mut reader = mp4::read_mp4(File::open(source)?)?;
    let mut tracks = reader
        .tracks()
        .iter()
        .filter_map(|(&source_id, track)| match playback_track_config(track) {
            Ok(Some(config)) => Some(Ok(PlaybackTrack { source_id, config })),
            Ok(None) => None,
            Err(error) if track.track_type().ok() == Some(mp4::TrackType::Audio) => {
                tracing::warn!(path = %source.display(), track_id = source_id, %error, "omitting invalid audio track from browser compatibility recording");
                None
            }
            Err(error) => Some(Err(error)),
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    tracks.sort_unstable_by_key(|track| track.source_id);
    if !tracks
        .iter()
        .any(|track| track.config.track_type == mp4::TrackType::Video)
    {
        bail!("recording has no supported video track");
    }

    let config = mp4::Mp4Config {
        major_brand: "isom".parse().unwrap(),
        minor_version: 512,
        compatible_brands: vec![
            "isom".parse().unwrap(),
            "iso2".parse().unwrap(),
            "mp41".parse().unwrap(),
        ],
        timescale: 1_000,
    };
    let output = BufWriter::new(File::create(destination)?);
    let mut writer = mp4::Mp4Writer::write_start(output, &config)?;
    for track in &tracks {
        writer.add_track(&track.config)?;
    }
    for (output_index, track) in tracks.iter().enumerate() {
        let sample_count = reader.sample_count(track.source_id)?;
        for sample_id in 1..=sample_count {
            let sample = reader
                .read_sample(track.source_id, sample_id)?
                .with_context(|| {
                    format!("track {} sample {sample_id} is missing", track.source_id)
                })?;
            writer.write_sample(output_index as u32 + 1, &sample)?;
        }
    }
    writer.write_end()?;
    let mut output = writer.into_writer();
    output.flush()?;
    Ok(())
}

fn playback_track_config(track: &mp4::Mp4Track) -> anyhow::Result<Option<mp4::TrackConfig>> {
    let media_conf = match track.media_type()? {
        mp4::MediaType::H264 => mp4::MediaConfig::AvcConfig(mp4::AvcConfig {
            width: track.width(),
            height: track.height(),
            seq_param_set: track.sequence_parameter_set()?.to_vec(),
            pic_param_set: track.picture_parameter_set()?.to_vec(),
        }),
        mp4::MediaType::H265 => {
            let sample_entry = track
                .trak
                .mdia
                .minf
                .stbl
                .stsd
                .hev1
                .as_ref()
                .or(track.trak.mdia.minf.stbl.stsd.hvc1.as_ref())
                .context("HEVC track has no sample entry")?;
            let configuration = sample_entry.hvcc.configuration()?;
            mp4::MediaConfig::HevcConfig(mp4::HevcConfig {
                width: track.width(),
                height: track.height(),
                vps: configuration.vps.first().cloned().unwrap_or_default(),
                sps: configuration.sps.first().cloned().unwrap_or_default(),
                pps: configuration.pps.first().cloned().unwrap_or_default(),
            })
        }
        mp4::MediaType::AAC => {
            let frequency = track.sample_freq_index()?;
            return Ok(Some(mp4::TrackConfig {
                track_type: mp4::TrackType::Audio,
                timescale: frequency.freq(),
                language: track.language().to_owned(),
                media_conf: mp4::MediaConfig::AacConfig(mp4::AacConfig {
                    bitrate: track.bitrate().max(64_000),
                    profile: track.audio_profile()?,
                    freq_index: frequency,
                    chan_conf: track.channel_config()?,
                }),
            }));
        }
        _ => return Ok(None),
    };
    Ok(Some(mp4::TrackConfig {
        track_type: mp4::TrackType::Video,
        timescale: track.timescale(),
        language: track.language().to_owned(),
        media_conf,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatibility_remux_repairs_audio_timescale_and_is_cached() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-playback-remux-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&directory).unwrap();
        let source = directory.join("recording.mp4");
        write_malformed_recording(&source);

        let cached = browser_compatible_recording(&source).unwrap();
        let size = cached.metadata().unwrap().len();
        assert_eq!(browser_compatible_recording(&source).unwrap(), cached);
        assert_eq!(cached.metadata().unwrap().len(), size);

        let reader = mp4::read_mp4(File::open(&cached).unwrap()).unwrap();
        let video = reader
            .tracks()
            .values()
            .find(|track| track.track_type().unwrap() == mp4::TrackType::Video)
            .unwrap();
        let audio = reader
            .tracks()
            .values()
            .find(|track| track.track_type().unwrap() == mp4::TrackType::Audio)
            .unwrap();
        assert_eq!(video.duration(), std::time::Duration::from_secs(1));
        assert_eq!(audio.timescale(), 16_000);
        assert_eq!(audio.duration(), std::time::Duration::from_millis(64));
        assert_eq!(reader.duration(), std::time::Duration::from_secs(1));

        std::fs::remove_dir_all(directory).unwrap();
    }

    fn write_malformed_recording(path: &Path) {
        let config = mp4::Mp4Config {
            major_brand: "iso6".parse().unwrap(),
            minor_version: 1,
            compatible_brands: vec!["iso6".parse().unwrap(), "mp41".parse().unwrap()],
            timescale: 1_000,
        };
        let tracks = [
            mp4::TrackConfig {
                track_type: mp4::TrackType::Video,
                timescale: 90_000,
                language: "und".to_owned(),
                media_conf: mp4::MediaConfig::AvcConfig(mp4::AvcConfig {
                    width: 320,
                    height: 240,
                    seq_param_set: vec![1],
                    pic_param_set: vec![2],
                }),
            },
            mp4::TrackConfig {
                track_type: mp4::TrackType::Audio,
                timescale: 16,
                language: "und".to_owned(),
                media_conf: mp4::MediaConfig::AacConfig(mp4::AacConfig {
                    bitrate: 64_000,
                    profile: mp4::AudioObjectType::AacLowComplexity,
                    freq_index: mp4::SampleFreqIndex::Freq16000,
                    chan_conf: mp4::ChannelConfig::Mono,
                }),
            },
        ];
        let mut writer =
            mp4::FragmentedMp4Writer::write_start(File::create(path).unwrap(), &config, &tracks)
                .unwrap();
        writer
            .write_sample(
                1,
                mp4::Mp4Sample {
                    start_time: 0,
                    duration: 90_000,
                    rendering_offset: 0,
                    is_sync: true,
                    bytes: vec![1].into(),
                },
            )
            .unwrap();
        writer
            .write_sample(
                2,
                mp4::Mp4Sample {
                    start_time: 0,
                    duration: 1_024,
                    rendering_offset: 0,
                    is_sync: true,
                    bytes: vec![2].into(),
                },
            )
            .unwrap();
        writer.write_end().unwrap();
    }
}
