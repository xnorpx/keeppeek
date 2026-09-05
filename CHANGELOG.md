# Changelog

## Unreleased

### Added

- Add Administrator-only `GET /config/export` and `POST /config/apply` for two-TOML ZIP transfer,
  validated restart activation, and startup recovery, with `keeppeek config` CLI commands and
  Settings controls through `keeppeek.backup.v1`.
- Add typed camera defaults, effective-value evidence, versioned templates, authoritative bulk
  previews, conflict-safe atomic writes, and per-camera activation results through
  `keeppeek.configuration.v1`.

### Security

- Validate archive paths, exact `config.toml` and `secrets.toml` membership, bounds, checksums,
  schemas, and target path identity before backup activation. Bundles contain plaintext secrets but
  exclude every database, recording media, sessions, provider state, and access audit activity.
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

- Replace the managed `/api/backups` HTTP workflow with direct configuration export and apply.
- Consolidate access credential metadata, layouts, configuration templates, notification rules, and
  MQTT settings in `config.toml`, with private values in `secrets.toml`.
- Replace notification and MQTT state databases with bounded in-memory delivery, retry,
  deduplication, cooldown, rate-limit, inbox, and history state that resets on restart.
- Align container builds with Bun 1.4.0 and pin every container base image by immutable digest.
- Require release tags to reference a commit on `main` with a successful push CI run for that exact commit.
