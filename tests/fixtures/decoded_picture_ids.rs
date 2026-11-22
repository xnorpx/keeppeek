use std::{path::Path, process::Command};

const WIDTH: usize = 64;
const HEIGHT: usize = 32;

/// Reads the four luma markers in each presented fixture picture.
pub fn decoded_picture_ids(path: &Path) -> Vec<u8> {
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-xerror", "-threads", "1", "-i"])
        .arg(path)
        .args([
            "-fps_mode",
            "passthrough",
            "-pix_fmt",
            "gray",
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (frames, remainder) = output.stdout.as_chunks::<{ WIDTH * HEIGHT }>();
    assert!(remainder.is_empty(), "incomplete decoded picture");
    frames
        .iter()
        .map(|pixels| {
            (0..4).fold(0_u8, |id, bit| {
                id | (u8::from(pixels[16 * WIDTH + bit * 16 + 8] > 128) << bit)
            })
        })
        .collect()
}
