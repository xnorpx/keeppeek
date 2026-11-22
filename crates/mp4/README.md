# mp4

> MP4 Reader and Writer in Rust 🦀

`mp4` is a Rust library to read and write ISO-MP4 files. This package contains MPEG-4 specifications defined in parts:

- [ISO/IEC 14496-12](https://en.wikipedia.org/wiki/ISO/IEC_base_media_file_format) - ISO Base Media File Format (QuickTime, MPEG-4, etc)
- [ISO/IEC 14496-14](https://en.wikipedia.org/wiki/MPEG-4_Part_14) - MP4 file format
- ISO/IEC 14496-17 - Streaming text format

The crate provides both the progressive `Mp4Writer` and `FragmentedMp4Writer`. The fragmented
writer emits a flushed `ftyp`/`moov` initialization range followed by caller-controlled
`moof`/`mdat` fragments, returning the exact byte range for every completed fragment.

https://crates.io/crates/mp4

[![Crates.io](https://img.shields.io/crates/v/mp4)](https://crates.io/crates/mp4)
[![Crates.io](https://img.shields.io/crates/d/mp4)](https://crates.io/crates/mp4)
[![Docs](https://img.shields.io/badge/docs-online-5023dd.svg?style=flat-square)](https://docs.rs/mp4)
[![Rust](https://github.com/alfg/mp4-rust/workflows/Rust/badge.svg)](https://github.com/alfg/mp4-rust/actions)

#### Example

```rust
use std::fs::File;
use std::io::{BufReader};
use mp4::{Result};

fn main() -> Result<()> {
    let f = File::open("tests/samples/minimal.mp4").unwrap();
    let size = f.metadata()?.len();
    let reader = BufReader::new(f);

    let mp4 = mp4::Mp4Reader::read_header(reader, size)?;

    // Print boxes.
    println!("major brand: {}", mp4.ftyp.major_brand);
    println!("timescale: {}", mp4.moov.mvhd.timescale);

    // Use available methods.
    println!("size: {}", mp4.size());

    let mut compatible_brands = String::new();
    for brand in mp4.compatible_brands().iter() {
        compatible_brands.push_str(&brand.to_string());
        compatible_brands.push_str(",");
    }
    println!("compatible brands: {}", compatible_brands);
    println!("duration: {:?}", mp4.duration());

    // Track info.
    for track in mp4.tracks().values() {
        println!(
            "track: #{}({}) {} : {}",
            track.track_id(),
            track.language(),
            track.track_type()?,
            track.box_type()?,
        );
    }
    Ok(())
}
```

#### Install

```
cargo add mp4
```

or add to your `Cargo.toml`:

```toml
mp4 = "0.14.0"
```

#### Documentation

- https://docs.rs/mp4/

## Development

#### Requirements

- [Rust](https://www.rust-lang.org/)

#### Build

```
cargo build
```

#### Lint and Format

```
cargo clippy --fix
cargo fmt --all
```

#### Run Examples

- `mp4info`

```
cargo run --example mp4info <movie.mp4>
```

- `mp4dump`

```
cargo run --example mp4dump <movie.mp4>
```

#### Run Tests

```
cargo test
```

With print statement output.

```
cargo test -- --nocapture
```

#### Run Cargo fmt

Run fmt to catch formatting errors.

```
rustup component add rustfmt
cargo fmt --all -- --check
```

#### Run Clippy

Run Clippy tests to catch common lints and mistakes.

```
rustup component add clippy
cargo clippy --no-deps -- -D warnings
```

#### Run Benchmark Tests

```
cargo bench
```

View HTML report at `target/criterion/report/index.html`

#### Generate Docs

```
cargo docs
```

View at `target/doc/mp4/index.html`

## Web Assembly

See the [mp4-inspector](https://github.com/alfg/mp4-inspector) project as a reference for using this library in Javascript via Web Assembly.

## Related Projects

- https://github.com/mozilla/mp4parse-rust
- https://github.com/pcwalton/rust-media
- https://github.com/alfg/mp4

## License

MIT

[docs]: https://docs.rs/mp4
[docs-badge]: https://img.shields.io/badge/docs-online-5023dd.svg?style=flat-square

## KeepPeek recording source clocks

New KeepPeek RTSP recordings may carry an optional `uuid` child of a `traf`.
The user type is `b66e6019-3cb1-499b-aecc-af96aac89894`. Older MP4 readers
can skip the box. MP4 sample timing, codec payloads and standard fragment addressing
are independent of this metadata. A track fragment with no measured source
timestamps uses the original box layout. Recorder retention, deletion and copies
of the original file apply to its timing metadata.

The UUID is followed by a big-endian `u32` version (currently 1), then one or
more runs. All integers use big-endian order. A run starts with the zero-based
`u32` index of its first sample in the enclosing `trun` and a nonzero `u32`
sample count. Next come the 16-byte opaque RTSP PLAY UUID, the nonzero `u32`
source clock rate, the first run sample's extended `i64` source RTP timestamp,
and `u32` flags (bit zero marks a discontinuity, all others are zero). Two
`i64` values identify the epoch anchor: its original RTP timestamp and its
presentation time in this MP4 track's timescale. The remaining `count - 1`
signed `i32` entries are differences from the first sample's original RTP
timestamp. The writer starts another run for a gap, discontinuity, session or
rate change, or an unrepresentable delta. Unlisted samples have no measured RTP
clock. Runs must be ordered, disjoint, and contained in the track sample run.

`Mp4Reader::source_rtp_time(track_id, sample_id)` uses one-based MP4 ordinals.
The low 32 bits of `timestamp` reproduce the packet header even across a wrap.
Audio access units aggregated in one RTP packet retain that same packet timestamp.
The epoch anchor is the first stored sample's extended packet timestamp paired
with its signed MP4 track presentation time. It persists across fragment
boundaries, including after reopening the recording. A source discontinuity,
change of PLAY UUID or clock rate, or a gap in measured samples starts a new
epoch. Each recording establishes its own track-relative calibration. Historical
recordings, unsupported metadata versions, unmeasured samples and non-RTP input
return `None`. Malformed recognized timing data returns an error.
