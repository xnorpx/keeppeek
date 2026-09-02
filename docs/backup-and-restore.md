# Backup and restore

KeepPeek creates versioned, checksummed recovery bundles while recording continues. The bundle is a
configuration and metadata artifact, not a recording-media archive. Operators can create, upload,
inspect, dry-run, stage, activate, verify, roll back, download, and delete bundles from Settings or
the HTTP JSON CLI.

## What a bundle contains

Format 2 stores `manifest.json` plus independently described sections. Every descriptor carries a
canonical relative path, schema version, byte length, SHA-256 digest, revision, and dependencies.
The manifest also records KeepPeek version, source platform, creation time, snapshot revision,
source paths, omitted data, capabilities, and required external secret references.

Sections are capability-gated:

| Section                 | Availability and contents                                                         |
| ----------------------- | --------------------------------------------------------------------------------- |
| Runtime configuration   | Sanitized server and storage policy from `config.toml`                            |
| Camera database         | Camera/default definitions and stable configured camera IDs                       |
| Integrations            | Sanitized MQTT and integration definitions                                        |
| Access                  | Credential identity, role, revision, enablement, expiry, and revocation metadata  |
| Layouts                 | The versioned `peek-layouts.json` registry when present                           |
| Configuration templates | The versioned template document when present                                      |
| Recording catalog       | A database-native point-in-time snapshot without MP4 bytes                        |
| Event metadata          | Event/workflow/search state bound to the catalog snapshot                         |
| Event thumbnails        | A filename, size, and SHA-256 inventory; JPEG bytes stay outside the bundle       |
| Notifications           | A database-native snapshot of rules and versions without delivery/runtime history |

Generic shared StateStore entries and server-defined groups are not advertised because KeepPeek does
not yet have durable owners for those domains. They become eligible sections when their owning
features provide a recovery contract. A request for an unsupported or unavailable section fails
instead of creating an empty claim.

The default artifact omits recording media, resolved secrets, sessions, derived caches, access audit
history, credential last-use activity, notification outbox/attempt/receipt/history state, and
provider receipts. Recording media and JPEG files need a separate filesystem or archive policy.

## Secret policy

Format 2 supports `references_only`. Existing exact `{secret:KEY}` and `{secret:KEY|url}` references
remain references. Inline camera, access, MQTT, notification, URL-userinfo, and unknown string values
are replaced with deterministic `BACKUP_...` references. Notification destinations are sanitized in
an offline database snapshot and resolved only in the staged target database.

The manifest lists every required reference. Put those values in the target's owner-only
`secrets.toml` or `KEEPPEEK_SECRET_<KEY>` environment before dry run. Missing references are blocking
plan issues but do not prevent inspection. KeepPeek never accepts a secret or passphrase in a CLI
argument, URL, query parameter, diagnostic, browser storage, or bundle.

Secrets-inclusive encrypted bundles are not implemented. Do not add `secrets.toml` to a recovery
ZIP manually.

## Limits

- Compressed archive: 1 GiB.
- Expanded archive: 1 GiB.
- One section: 512 MiB.
- Manifest: 1 MiB.
- Archive members: 64.
- Retained managed backups: 16.
- Retained in-memory restore plans: 128.
- Restore plan lifetime: 10 minutes.
- Rollback retention after activation: 30 minutes.
- Native database snapshot wait: 120 seconds.

The server reports these values through `GET /api/backups/capabilities`. Work is serialized so one
backup lifecycle operation cannot race another. Uploads and database sections stream through bounded
buffers rather than loading the archive into memory.

## Create and inspect

Open **Settings → Backup and restore** as an Administrator. **Create backup** snapshots every
currently supported section. The retained list shows timestamp, size, validated checksum, and
section status. **Upload ZIP** reserves a bounded transfer, streams the artifact, and promotes it only
after inspection succeeds.

Inspection rejects unsupported formats or schemas, traversal and absolute archive members,
backslashes and drive prefixes, symlinks, directories, encrypted members, duplicate/case-colliding
members, undeclared files, malformed manifests, invalid section schemas, bad checksums, oversized
content, malformed databases, resolved notification destinations, and retained provider/runtime
state.

Format 1 runtime-configuration bundles from the original inspector are supported through an explicit
format-1-to-format-2 migration plan. Newer formats and unsupported section schemas fail without
changing live state. KeepPeek never downgrades a bundle.

## Dry run

Select a validated backup and map every source path to an explicit target. KeepPeek canonicalizes the
mapping and binds the immutable plan to it. The dry run does not extract or replace live state. It
reports:

- source version, platform, sections, sizes, checksums, omissions, and capabilities;
- selected-section dependencies and migrations;
- target revision conflicts and artifact digest;
- missing external secrets;
- canonical target paths, writability, available capacity, and required bytes;
- merged target-configuration validity;
- external recording and thumbnail path consequences;
- missing, changed, or unverified external thumbnail files;
- server restart impact and plan expiry.

Any blocking issue sets `canActivate` to false. Warnings remain visible and require operator review.
A stale target revision, changed bundle digest, expired plan, changed canonical path, or changed
ordinary target file is checked again during staging.

## Stage, activate, and verify

Activation requires explicit confirmation. KeepPeek writes every selected target to an owner-only
same-filesystem staging name, verifies its digest and schema, transforms mapped catalog paths,
resolves notification references against target secrets, compacts databases into self-contained
snapshots, and persists a versioned restart journal. Live files remain unchanged while staging.

Restart KeepPeek after the record reaches `AWAITING_RESTART`. Recovery runs before configuration and
database owners open. Ordinary files use exact before-image digests. Mutable databases receive a
native point-in-time before-image after the prior process has stopped, so normal writes between
staging and restart are retained for rollback. The journal records each transition before the next
unsafe step and reverses partially applied targets in reverse order.

The restore is complete only after the HTTP server and camera workers start and the restored
configuration loads with target secrets. Settings retains and displays those health checks. If
configuration loading or startup health fails, KeepPeek restores the previous state during the same
failed launch. A crash while preparing or applying is reconciled on the next start before restored
state can open.

## Rollback

A completed restore retains owner-only before-images for 30 minutes. Select **Stage rollback**, then
restart. Rollback removes the activated targets and restores the prior files and database snapshots
in reverse order. The request is idempotent and requires the active restore ID plus explicit
confirmation. After expiry, KeepPeek deletes the rollback files and journal.

Do not manually edit, move, or delete `.backups/restore-journal.json` or its sibling `.staged` and
`.rollback` files. If startup reports that both activation and rollback failed, preserve the complete
configuration directory and logs before manual recovery.

## CLI automation

The CLI uses the same Administrator-only ProtoJSON HTTP API as Settings. It prints one machine-readable
ProtoJSON object to stdout and diagnostics to stderr. Remote credentials come only from
`KEEPPEEK_ACCESS_KEY` and require HTTPS unless the server is loopback.

```sh
keeppeek backup --server http://localhost:3000 capabilities
keeppeek backup --server http://localhost:3000 create --output keeppeek-backup.zip
keeppeek backup --server http://localhost:3000 list
keeppeek backup --server http://localhost:3000 inspect <backup-id>
keeppeek backup --server http://localhost:3000 dry-run <backup-id> \
  --map config-directory=/target/config \
  --map recording-catalog=/target/recordings.db \
  --map long-term-media=/archive/recordings \
  --map event-thumbnails=/archive/event-thumbnails \
  --map notification-database=/target/notifications.db
keeppeek backup --server http://localhost:3000 restore <plan-id> <archive-sha256> --confirm
keeppeek backup --server http://localhost:3000 status <restore-id>
keeppeek backup --server http://localhost:3000 rollback <restore-id> --confirm
keeppeek backup --server http://localhost:3000 delete <backup-id>
```

Exit code `2` means CLI usage failed, `3` means the server rejected the request with a stable 4xx
error, and `4` means transport, protocol, or server failure. Scripts must inspect both the process
status and the returned enum/state before continuing to restart.

## Performance evidence

Run the full-domain release benchmark with:

```sh
cargo test --release --locked --lib \
  backup::restore::tests::backup_restore_performance_benchmark \
  -- --ignored --exact --nocapture
```

On macOS 26.6.2 arm64, Apple M5 Max, Rust 1.97.1, 10 runs, and a 14,595-byte bundle containing every
currently supported domain, the stopped-service raw-copy baseline measured p50/p95 `4.794/22.918 ms`.
The validated online bundle measured p50/p95 `187.041/583.204 ms` against a `2,000 ms` p95 budget.
Its p95 cost increased by `560.286 ms` (`2444.7%`) relative to raw copying. Dry run measured
`76.984/162.267 ms` against `500 ms`; staging measured `449.072/677.259 ms` against `2,000 ms`.
The raw copy is faster but does not provide a consistent live snapshot, sanitization, validation,
migration planning, or rollback.
