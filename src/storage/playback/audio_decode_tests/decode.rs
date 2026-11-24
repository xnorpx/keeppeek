use super::*;
use std::process::{Command, Stdio};

#[derive(Debug)]
pub(super) struct Observed {
    pub flashes_us: Vec<i64>,
    pub pulses_us: Vec<i64>,
}

pub(super) fn media(path: &Path) -> Observed {
    let video = path.with_extension("framecrc");
    let pcm = path.with_extension("pcm");
    let log = path.with_extension("decode.log");
    run(command(path, &video, &pcm), &log);
    assert!(std::fs::metadata(&video).unwrap().len() <= 65_536);
    let frames = decoded_frames(&std::fs::read_to_string(video).unwrap());
    let pcm = std::fs::read(pcm).unwrap();
    assert!(pcm.len() <= 1_048_576);
    let reader = mp4::read_mp4(File::open(path).unwrap()).unwrap();
    let video_track = reader
        .tracks()
        .iter()
        .find(|(_, track)| matches!(track.track_type(), Ok(mp4::TrackType::Video)))
        .unwrap()
        .0;
    let expected = usize::try_from(reader.sample_count(*video_track).unwrap()).unwrap();
    assert!((1..=150).contains(&expected));
    assert_eq!(
        frames.len(),
        expected,
        "every video sample must decode in {}",
        path.display()
    );
    let mut flashes_us = Vec::new();
    let mut white = false;
    for (time, value) in frames {
        if value > 200 && !white {
            flashes_us.push(time);
        }
        white = value > 200;
    }
    Observed {
        flashes_us,
        pulses_us: pulses(&pcm),
    }
}

fn command(path: &Path, video: &Path, pcm: &Path) -> Command {
    let mut command = Command::new("ffmpeg");
    command
        .args([
            "-nostdin",
            "-nostats",
            "-hide_banner",
            "-v",
            "error",
            "-xerror",
            "-y",
            "-threads",
            "1",
            "-filter_threads",
            "1",
            "-copyts",
            "-i",
        ])
        .arg(path)
        .args([
            "-map",
            "0:v:0",
            "-vf",
            "scale=1:1,format=gray",
            "-enc_time_base",
            "1:1000000",
            "-fps_mode",
            "passthrough",
            "-frames:v",
            "150",
            "-t",
            "6",
            "-fs",
            "1048576",
            "-f",
            "framecrc",
        ])
        .arg(video)
        .args([
            "-map",
            "0:a:0",
            "-af",
            "aresample=48000:async=1:first_pts=0",
            "-ac",
            "1",
            "-ar",
            "48000",
            "-c:a",
            "pcm_s16le",
            "-t",
            "6",
            "-fs",
            "1048576",
            "-f",
            "s16le",
        ])
        .arg(pcm);
    command
}

fn run(mut command: Command, log: &Path) {
    command
        .stdout(Stdio::null())
        .stderr(File::create(log).unwrap());
    let mut child = command
        .spawn()
        .expect("ffmpeg is required for audio timing qualification");
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) => break Err("audio qualification decoder timed out".to_owned()),
            Err(error) => break Err(format!("audio qualification decoder failed: {error}")),
        }
    };
    if status.is_err() {
        child.kill().expect("timed out decoder must terminate");
        child.wait().expect("terminated decoder must be reaped");
    }
    assert!(
        std::fs::metadata(log).unwrap().len() <= 262_144,
        "decoder log exceeded its fixture bound"
    );
    assert!(
        status.unwrap().success(),
        "{}",
        std::fs::read_to_string(log).unwrap()
    );
}

fn decoded_frames(output: &str) -> Vec<(i64, u8)> {
    assert!(output.lines().any(|line| line == "#tb 0: 1/1000000"));
    output
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .map(|line| {
            // The framecrc muxer puts one packet's presentation timestamp, size and checksum in one row.
            let mut fields = line.split(',').map(str::trim);
            assert_eq!(fields.next(), Some("0"));
            let pts: i64 = fields.nth(1).unwrap().parse().unwrap();
            assert!((0..=6_000_000).contains(&pts));
            assert_eq!(
                fields.nth(1),
                Some("1"),
                "expected one decoded grayscale byte"
            );
            let checksum =
                u32::from_str_radix(fields.next().unwrap().strip_prefix("0x").unwrap(), 16)
                    .unwrap();
            // FFmpeg's zero-seeded Adler-32 of one byte b has b in each 16-bit sum.
            let gray = checksum.to_le_bytes()[0];
            assert_eq!(checksum, u32::from(gray) * 0x1_0001);
            (pts, gray)
        })
        .collect()
}

#[test]
fn decoded_packet_rows_keep_microsecond_timestamps_with_their_grayscale_values() {
    let frames = "#tb 0: 1/1000000\n0, 0, 21333, 40000, 1, 0x00000000\n0, 1, 61333, 40000, 1, 0x00ff00ff\n0, 2, 101333, 40000, 1, 0x007f007f\n";
    assert_eq!(
        decoded_frames(frames),
        [(21_333, 0), (61_333, 255), (101_333, 127)]
    );
}

fn pulses(bytes: &[u8]) -> Vec<i64> {
    assert_eq!(bytes.len() % 2, 0);
    let mut active = false;
    let mut times = Vec::new();
    for (index, window) in bytes.as_chunks::<960>().0.iter().enumerate() {
        let energy = window
            .as_chunks::<2>()
            .0
            .iter()
            .map(|sample| {
                let value = i64::from(i16::from_le_bytes([sample[0], sample[1]]));
                value * value
            })
            .sum::<i64>();
        let loud = energy / 480 > 36_000_000;
        if loud && !active {
            times.push(i64::try_from(index).unwrap() * 10_000);
        }
        active = loud;
    }
    times
}
