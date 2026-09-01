# Changelog

## Unreleased

### Added

- Add typed camera defaults, effective-value evidence, versioned templates, authoritative bulk
  previews, conflict-safe atomic writes, and per-camera activation results through
  `keeppeek.configuration.v1`.

### Security

- Bound pre-authentication HTTP connections, parsing, request execution, delete bodies, and queued requests.
- Bound Baichuan frame bodies, BCUDP packet windows, and application transport queues.
- Verify the pinned CCTV Camera Database v2.8.0 archive with SHA-256 before ZIP parsing.
- Track `Cargo.lock`, constrain every direct Cargo dependency, and require locked Cargo commands in build, test, documentation, container, and release automation.
- Block release-producing CI on Rust, Bun, and Python dependency audits plus a production-container startup probe.

### Fixed

- Preserve newer BCUDP packets when cumulative acknowledgments cross the 32-bit sequence rollover.
- Reject AVC and HEVC parameter sets that cannot be represented in MP4 configuration boxes.
- Preserve unsaved MQTT settings during status refresh and expose hidden advanced-storage validation failures while the section is collapsed.
- Reject duplicate or excessive stored-media cursors before opening media.

### Changed

- Align container builds with Bun 1.4.0 and pin every container base image by immutable digest.
- Require release tags to reference a commit on `main` with a successful push CI run for that exact commit.
