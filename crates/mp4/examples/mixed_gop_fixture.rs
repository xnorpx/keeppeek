use mp4::{
    FragmentedMp4Writer, FragmentedTrackConfig, MediaConfig, MediaType, Mp4Config, Mp4Sample,
    TrackConfig, TrackType,
};
use std::{error::Error, fs::File, path::Path};

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let h264_path = arguments.next().ok_or("missing low H.264 fixture path")?;
    let high_h264_path = arguments.next().ok_or("missing high H.264 fixture path")?;
    let h265_path = arguments.next().ok_or("missing H.265 fixture path")?;
    let output_directory = arguments.next().ok_or("missing output directory")?;
    if arguments.next().is_some() {
        return Err("unexpected fixture generator argument".into());
    }
    let output_directory = Path::new(&output_directory);
    std::fs::create_dir_all(output_directory)?;

    let (h264_config, h264_sample) = fixture(Path::new(&h264_path), MediaType::H264)?;
    let (high_h264_config, high_h264_sample) =
        fixture(Path::new(&high_h264_path), MediaType::H264)?;
    let (h265_config, h265_sample) = fixture(Path::new(&h265_path), MediaType::H265)?;
    write_initial_period(
        &output_directory.join("h264-period.mp4"),
        h264_config.clone(),
        h264_sample.clone(),
    )?;
    write_initial_period(
        &output_directory.join("h264-high-period.mp4"),
        high_h264_config,
        high_h264_sample,
    )?;
    write_initial_period(
        &output_directory.join("h265-period.mp4"),
        h265_config.clone(),
        h265_sample.clone(),
    )?;
    write_mixed_period(
        &output_directory.join("mixed-period.mp4"),
        h264_config,
        h264_sample,
        h265_config,
        h265_sample,
    )?;
    Ok(())
}

fn fixture(path: &Path, media_type: MediaType) -> Result<(MediaConfig, Mp4Sample), Box<dyn Error>> {
    let mut reader = mp4::read_mp4(File::open(path)?)?;
    let (track_id, config, sample_count) = reader
        .tracks()
        .iter()
        .find_map(|(&track_id, track)| {
            (track.media_type().ok() == Some(media_type)).then(|| {
                Ok::<_, mp4::Error>((
                    track_id,
                    track.media_config_for_description(1)?,
                    track.sample_count(),
                ))
            })
        })
        .ok_or("fixture has no requested video track")??;
    let mut sample = (1..=sample_count)
        .find_map(|sample_id| {
            let sample = reader.read_sample(track_id, sample_id).ok()??;
            sample.is_sync.then_some(sample)
        })
        .ok_or("fixture has no sync sample")?;
    sample.start_time = 0;
    sample.duration = 90_000;
    Ok((config, sample))
}

fn mp4_config() -> Mp4Config {
    Mp4Config {
        major_brand: "iso6".parse().unwrap(),
        minor_version: 1,
        compatible_brands: vec![
            "iso6".parse().unwrap(),
            "isom".parse().unwrap(),
            "mp41".parse().unwrap(),
        ],
        timescale: 1_000,
    }
}

fn write_initial_period(
    path: &Path,
    h264_config: MediaConfig,
    h264_sample: Mp4Sample,
) -> Result<(), Box<dyn Error>> {
    let track = TrackConfig {
        track_type: TrackType::Video,
        timescale: 90_000,
        language: "und".to_owned(),
        media_conf: h264_config,
    };
    let mut writer =
        FragmentedMp4Writer::write_start(File::create(path)?, &mp4_config(), &[track])?;
    writer.write_sample(1, h264_sample)?;
    writer.write_end()?;
    Ok(())
}

fn write_mixed_period(
    path: &Path,
    h264_config: MediaConfig,
    mut h264_sample: Mp4Sample,
    h265_config: MediaConfig,
    h265_sample: Mp4Sample,
) -> Result<(), Box<dyn Error>> {
    let track = FragmentedTrackConfig {
        track_type: TrackType::Video,
        timescale: 90_000,
        language: "und".to_owned(),
        sample_descriptions: vec![h264_config, h265_config],
    };
    let mut writer = FragmentedMp4Writer::write_start_with_sample_descriptions(
        File::create(path)?,
        &mp4_config(),
        &[track],
    )?;
    writer.write_sample_with_description(1, 2, h265_sample)?;
    writer.flush_fragment()?;
    h264_sample.start_time = 90_000;
    writer.write_sample_with_description(1, 1, h264_sample)?;
    writer.write_end()?;
    Ok(())
}
