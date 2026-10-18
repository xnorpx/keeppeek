//! Splits the fixed calibration recording without changing its encoded media or clock.

use super::*;
use std::io::Cursor;

struct Packet {
    track: u32,
    timescale: u32,
    sample: mp4::Mp4Sample,
}

pub(super) fn render(path: &Path) -> anyhow::Result<Vec<Prepared>> {
    ensure!(
        std::fs::metadata(path)?.len() <= 65_536,
        "calibration source exceeds 64 KiB"
    );
    let bytes = std::fs::read(path)?;
    ensure!(
        bytes.as_slice() == include_bytes!("../../../testdata/flash-tone-lead-200ms.mp4"),
        "calibration requires the checked-in flash/tone fixture"
    );
    let mut reader =
        mp4::Mp4Reader::read_header(Cursor::new(bytes.clone()), u64::try_from(bytes.len())?)?;
    let mut tracks = reader
        .tracks()
        .iter()
        .map(|(&id, track)| {
            Ok((
                id,
                mp4::TrackConfig {
                    track_type: track.track_type()?,
                    timescale: track.timescale(),
                    language: track.language().into(),
                    media_conf: track.media_config_for_description(1)?,
                },
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    tracks.sort_unstable_by_key(|(id, _)| *id);
    ensure!(
        tracks.len() == 2 && tracks[0].0 == 1 && tracks[1].0 == 2,
        "calibration requires two canonical tracks"
    );
    let packets = packets(&mut reader, &tracks)?;
    let config = mp4::Mp4Config {
        major_brand: "iso6".parse()?,
        minor_version: 1,
        compatible_brands: vec!["iso6".parse()?],
        timescale: 1_000,
    };
    let mut writer = mp4::FragmentedMp4Writer::write_start(
        Cursor::new(Vec::new()),
        &config,
        &tracks
            .into_iter()
            .map(|(_, track)| track)
            .collect::<Vec<_>>(),
    )?;
    let initialization = writer.initialization();
    let fragments = append(&mut writer, packets)?;
    ensure!(fragments.len() == 4, "calibration requires four GOPs");
    let bytes = writer.into_writer().into_inner();
    ensure!(bytes.len() <= 65_536, "calibration render exceeds 64 KiB");
    let initialization_len = usize::try_from(initialization.size)?;
    fragments
        .chunks(2)
        .map(|group| split(&bytes, initialization_len, group))
        .collect()
}

fn packets(
    reader: &mut mp4::Mp4Reader<Cursor<Vec<u8>>>,
    tracks: &[(u32, mp4::TrackConfig)],
) -> anyhow::Result<Vec<Packet>> {
    let mut packets = Vec::new();
    for (id, track) in tracks {
        let count = reader.sample_count(*id)?;
        ensure!(
            count > 0 && count <= 256,
            "calibration sample count is invalid"
        );
        for index in 1..=count {
            packets.push(Packet {
                track: *id,
                timescale: track.timescale,
                sample: reader
                    .read_sample(*id, index)?
                    .context("missing calibration sample")?,
            });
        }
    }
    packets.sort_by(|left, right| {
        (u128::from(left.sample.start_time) * u128::from(right.timescale))
            .cmp(&(u128::from(right.sample.start_time) * u128::from(left.timescale)))
            .then(left.track.cmp(&right.track))
    });
    Ok(packets)
}

fn append(
    writer: &mut mp4::FragmentedMp4Writer<Cursor<Vec<u8>>>,
    packets: Vec<Packet>,
) -> anyhow::Result<Vec<Fragment>> {
    let mut fragments = Vec::new();
    let mut start = 0;
    let mut end = 0;
    for packet in packets {
        if packet.track == 1 {
            if packet.sample.is_sync && writer.has_pending_samples() {
                let info = writer
                    .flush_fragment_before_sample(1, packet.sample.start_time)?
                    .context("calibration GOP is empty")?;
                fragments.push(Fragment {
                    info,
                    start_ms: millis(start)?,
                    end_ms: millis(packet.sample.start_time)?,
                });
                start = packet.sample.start_time;
            }
            end = packet
                .sample
                .start_time
                .checked_add(u64::from(packet.sample.duration))
                .context("calibration sample end overflow")?;
        }
        writer.write_sample(packet.track, packet.sample)?;
    }
    fragments.push(Fragment {
        info: writer
            .write_end()?
            .context("last calibration GOP is empty")?,
        start_ms: millis(start)?,
        end_ms: millis(end)?,
    });
    Ok(fragments)
}

fn millis(ticks: u64) -> anyhow::Result<i64> {
    Ok(i64::try_from(
        ticks
            .checked_mul(1_000)
            .context("calibration time overflow")?
            / 90_000,
    )?)
}

fn split(bytes: &[u8], initialization_len: usize, group: &[Fragment]) -> anyhow::Result<Prepared> {
    let mut output = bytes[..initialization_len].to_vec();
    let mut fragments = Vec::new();
    for fragment in group {
        let offset = u64::try_from(output.len())?;
        let start = usize::try_from(fragment.info.range.offset)?;
        let end = usize::try_from(fragment.info.range.offset + fragment.info.range.size)?;
        output.extend_from_slice(&bytes[start..end]);
        let keyframe = fragment
            .info
            .video_keyframe
            .context("missing calibration keyframe")?;
        fragments.push(Fragment {
            info: mp4::Mp4FragmentInfo {
                sequence_number: fragment.info.sequence_number,
                range: mp4::Mp4ByteRange {
                    offset,
                    size: fragment.info.range.size,
                },
                video_keyframe: Some(mp4::Mp4SampleLocation {
                    offset: keyframe
                        .offset
                        .checked_sub(fragment.info.range.offset)
                        .and_then(|relative| relative.checked_add(offset))
                        .context("calibration keyframe overflow")?,
                    size: keyframe.size,
                }),
            },
            start_ms: fragment.start_ms,
            end_ms: fragment.end_ms,
        });
    }
    Ok(Prepared {
        bytes: output,
        initialization_len: u64::try_from(initialization_len)?,
        fragments,
    })
}
