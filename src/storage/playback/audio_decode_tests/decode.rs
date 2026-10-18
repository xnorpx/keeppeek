use super::*;
use std::process::{Command, Stdio};

#[derive(Debug)]
pub(super) struct Observed {
    pub flashes_us: Vec<i64>,
    pub pulses_us: Vec<i64>,
}

pub(super) fn media(path: &Path) -> Observed {
    let gray = path.with_extension("gray");
    let pcm = path.with_extension("pcm");
    let log = path.with_extension("decode.log");
    run(command(path, &gray, &pcm), &log);
    let gray = std::fs::read(gray).unwrap();
    let pcm = std::fs::read(pcm).unwrap();
    assert!(gray.len() <= 150 && pcm.len() <= 1_048_576);
    let times = frame_times(&std::fs::read_to_string(&log).unwrap());
    assert_eq!(
        gray.len(),
        times.len(),
        "every decoded frame needs its own presentation time; inspect {}",
        log.display()
    );
    let mut flashes_us = Vec::new();
    let mut white = false;
    for (&value, &time) in gray.iter().zip(&times) {
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

fn command(path: &Path, gray: &Path, pcm: &Path) -> Command {
    let mut command = Command::new("ffmpeg");
    command
        .args([
            "-nostdin",
            "-nostats",
            "-hide_banner",
            "-v",
            "info",
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
            "scale=1:1,format=gray,showinfo",
            "-fps_mode",
            "passthrough",
            "-frames:v",
            "150",
            "-t",
            "6",
            "-fs",
            "1048576",
            "-f",
            "rawvideo",
        ])
        .arg(gray)
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

fn frame_times(log: &str) -> Vec<i64> {
    log.split("Parsed_showinfo")
        .skip(1)
        .filter_map(|entry| {
            entry
                .split_whitespace()
                .find_map(|field| field.strip_prefix("pts_time:"))
        })
        .map(|value| {
            let seconds: f64 = value.parse().unwrap();
            assert!(seconds.is_finite() && (0.0..=6.0).contains(&seconds));
            (seconds * 1_000_000.0).round() as i64
        })
        .collect()
}

#[test]
fn frame_times_preserve_entries_split_or_joined_by_console_updates() {
    let log = "[Parsed_showinfo_2 @ decoder] n: 0 pts_time:0.021333\rframe=1\r\
               [Parsed_showinfo_2 @ decoder] n: 1 pts_time:0.061333\n\
               [Parsed_showinfo_2 @ decoder]\noutput status\n n: 2 pts_time:0.101333\n\
               [Parsed_showinfo_2 @ decoder] color_range:pc\n";
    assert_eq!(frame_times(log), vec![21_333, 61_333, 101_333]);
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
