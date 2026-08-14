use clap::Parser;
use mp4::TrackType;
use std::{
    fs::File,
    path::{Path, PathBuf},
};

#[derive(Debug, Parser)]
#[command(
    name = "validate-recordings",
    about = "Read every sample in finalized MP4 recordings and verify their indexes"
)]
struct Cli {
    /// MP4 file or directory tree to validate.
    #[arg(short, long)]
    input: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let mut files = Vec::new();
    collect_mp4s(&cli.input, &mut files)?;
    files.sort_unstable();
    if files.is_empty() {
        anyhow::bail!("no finalized MP4 files found under {}", cli.input.display());
    }

    let mut failures = Vec::new();
    for file in &files {
        match validate_recording(file) {
            Ok(summary) => println!(
                "MP4_OK file={} tracks={} video_samples={} audio_samples={} duration_ms={}",
                file.display(),
                summary.tracks,
                summary.video_samples,
                summary.audio_samples,
                summary.duration_ms,
            ),
            Err(error) => {
                eprintln!("MP4_FAIL file={} error={error:#}", file.display());
                failures.push(file);
            }
        }
    }

    println!(
        "MP4_VALIDATION_SUMMARY files={} failures={}",
        files.len(),
        failures.len()
    );
    if !failures.is_empty() {
        anyhow::bail!("{} recording(s) failed validation", failures.len());
    }
    Ok(())
}

struct ValidationSummary {
    tracks: usize,
    video_samples: u64,
    audio_samples: u64,
    duration_ms: u128,
}

fn validate_recording(path: &Path) -> anyhow::Result<ValidationSummary> {
    let mut reader = mp4::read_mp4(File::open(path)?)?;
    let tracks = reader
        .tracks()
        .iter()
        .map(|(&track_id, track)| {
            Ok((
                track_id,
                track.track_type()?,
                reader.sample_count(track_id)?,
            ))
        })
        .collect::<mp4::Result<Vec<_>>>()?;
    if tracks.is_empty() {
        anyhow::bail!("recording has no tracks");
    }

    let mut video_samples = 0u64;
    let mut audio_samples = 0u64;
    for (track_id, track_type, sample_count) in &tracks {
        if *track_type == TrackType::Video && *sample_count == 0 {
            anyhow::bail!("video track {track_id} has no samples");
        }
        for sample_id in 1..=*sample_count {
            let sample = reader
                .read_sample(*track_id, sample_id)?
                .ok_or_else(|| anyhow::anyhow!("track {track_id} sample {sample_id} is missing"))?;
            if sample.duration == 0 {
                anyhow::bail!("track {track_id} sample {sample_id} has zero duration");
            }
            if sample.bytes.is_empty() {
                anyhow::bail!("track {track_id} sample {sample_id} has no media bytes");
            }
        }
        match track_type {
            TrackType::Video => video_samples += u64::from(*sample_count),
            TrackType::Audio => audio_samples += u64::from(*sample_count),
            _ => {}
        }
    }
    if video_samples == 0 {
        anyhow::bail!("recording has no video samples");
    }

    Ok(ValidationSummary {
        tracks: tracks.len(),
        video_samples,
        audio_samples,
        duration_ms: reader.duration().as_millis(),
    })
}

fn collect_mp4s(input: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if input.is_file() {
        if input.extension().and_then(|extension| extension.to_str()) == Some("mp4") {
            files.push(input.to_path_buf());
        }
        return Ok(());
    }
    for entry in std::fs::read_dir(input)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_mp4s(&path, files)?;
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("mp4") {
            files.push(path);
        }
    }
    Ok(())
}
