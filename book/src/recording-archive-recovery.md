# Recording archive recovery

This chapter is the host-operator and **Administrator** runbook for backing up, rehearsing, restoring, and diagnosing KeepPeek's complete recording archive (catalog, state databases, media roots, and configuration). KeepPeek provides **no recording-archive importer, no database merge tool, and no automatic adoption of unknown MP4 files**—a configuration ZIP restores settings and file-backed secrets only, and arbitrary live filesystem copies are not guaranteed recoverable.

## Inventory the recovery set

Sign in as an Administrator, open **Settings → Storage & retention → Named storage volumes** (and **Local archive** if enabled), and record every configured root, its stable ID, and the OS account that runs KeepPeek. Do not rely on default paths if Settings defines overrides, and never treat exported evidence clips as a recording archive backup.

| Asset                                                | Default / configured location                                                | Covered by config ZIP?                 | Recovery requirement                                                                                                                            |
| ---------------------------------------------------- | ---------------------------------------------------------------------------- | -------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| `config.toml` + sibling `secrets.toml`               | Config directory (`--config`)                                                | Yes (**plaintext secrets**)            | Cameras, policies, grants, dashboards, templates, `[runtime_profiles]`, `[storage.archive]`, settings-backed `state_store`.                     |
| Recording catalog + SQLite sidecars (`-wal`, `-shm`) | Metadata volume (`catalog-<uuid>.db`) or legacy `recordings.db`              | No                                     | Recording identities, owned paths, playback indexes, events, coverage/deletion ledger, maintenance intent. Copy as one stopped family.          |
| Runtime state store + sidecars                       | Long-term storage (`state-store.db`)                                         | No (only `state_store` config entries) | Desired-state docs, leases, namespace/entry revisions, byte counters. Copy and restore as one unit.                                             |
| Guest review grant store + sidecars                  | Metadata volume (`guest-review/grants.db`)                                   | No                                     | Guest link verifiers, revocations, view/download counters. Follow [Recover guest links](#recover-guest-links) when restoring historical images. |
| Cloud mobile push state + sidecars                   | Long-term storage (`cloud-notifications.db`)                                 | No                                     | Event aliases, pending/claimed intents, rolling rate windows, monotonic clock floor. Copy as one stopped family.                                |
| Active & archive media volume roots                  | `storage/media` or configured named volumes                                  | No                                     | Copy the **entire** root (including hidden/staging and `.active` files), not just `*.mp4`.                                                      |
| Fourth-tier local archive root (optional)            | `[storage.archive] root`                                                     | No (`[storage.archive]` config only)   | Verified replica files and private staging; preserve paired with catalog manifests and target bindings.                                         |
| Thumbnail volume roots                               | `storage/images` or configured roots                                         | No                                     | Event-image bytes referenced by catalog.                                                                                                        |
| Export volume roots + export history                 | `storage/exports` + metadata volume (`exports-<uuid>.json`)                  | No                                     | Export artifacts (independent expiry) and durable export history.                                                                               |
| Host & operational dependencies                      | Service unit/SCM, `KEEPPEEK_SECRET_*`, TLS certs, sibling `log-filter`, logs | No                                     | Preserve separately in your protected operations store.                                                                                         |

Copying maintenance private staging preserves forensic evidence, **not** permission to retry destructive work on another disk: filesystem device/inode identities can change across copies.

## Create a consistent offline copy

1. Finish pending configuration applies or storage moves, export a private [configuration ZIP](./configuration-export-import.md), and finish or cancel active exports/maintenance jobs (retaining reports for unresolved work; if preserving a failure state instead, keep unfinished journals and mark the copy as forensic, not a clean recovery point).
2. Stop KeepPeek through its service manager and confirm process exit so no writer touches the files. Recording pauses during this window.
3. Copy the config directory and every inventoried volume root while stopped, including all SQLite sidecar files (`-wal`, `-shm`) for the catalog, `state-store.db`, `guest-review/grants.db`, and `cloud-notifications.db`. Never delete sidecars to simplify copying.
4. Preserve ownership and owner-only permissions, record the exact KeepPeek build and source paths, verify copy integrity off the source disk, then restart the source recorder and confirm new recordings finalize normally.

A live copy of a SQLite file alone can disagree with its WAL or MP4 files, and KeepPeek's configuration export lock does **not** snapshot databases or media. If you rely on filesystem/storage snapshots instead of stopping the service, design and test a multi-volume consistent snapshot procedure across every writer.

## Recover guest links

Guest link verifiers, revocations, and consumed view/download counters live in `guest-review/grants.db` on the metadata volume (`[sharing]` in `config.toml` holds sharing policy). Restart terminates active guest sessions and transfers.

Restoring a historical metadata volume image can resurrect revoked grants or reset consumed counters. Always neutralize old guest grants before exposing a restored recorder:

1. Keep the original backup unchanged offline.
2. Before starting the restored working copy, set `enabled = false` in `[sharing]` in `config.toml` and keep the public guest origin offline.
3. Delete the entire `guest-review/` directory (including `grants.db` and all SQLite sidecars) from the **working copy** of the metadata volume. Startup initializes a fresh, empty guest store.
4. Start KeepPeek in isolation, verify recovered recordings and exports, then re-enable `[sharing]` and confirm the Administrator's all-creators guest-link list is empty before restoring public access.
5. Issue fresh guest links only for recordings and completed exports an Administrator still authorizes.

## Restore the runtime state store before accepting watches

Restore `state-store.db` and its SQLite sidecars together so namespace revisions, entry revisions, and byte counters return as one consistent unit **before** KeepPeek accepts watches. Restoring entries without their counters lets new writes reuse previously consumed revisions, breaking compare-and-set ordering. After restore, clients open fresh watches and reconcile snapshots against current `ServerCapabilities` (no resume token crosses a restore). Restore `cloud-notifications.db` and its sidecars as a matching stopped family when mobile push signaling is enabled.

## Rehearse recovery on an isolated host

1. Leave the original backup read-only; restore a working copy onto an isolated host with matching KeepPeek version, compatible local filesystem (NTFS on Windows), sufficient capacity, and service-account permissions.
2. Block outbound access to real cameras, notification providers, and MQTT brokers until intentionally tested—imported credentials and rules are live and a duplicate recorder can otherwise trigger duplicate alerts or device contention.
3. Restore the catalog family, state databases, and all media/archive/thumbnail/export roots together at their **original configured paths** (do not start with missing archive mounts, and do not edit catalog SQL—use [Move storage deliberately](./upgrades-and-migrations.md#move-storage-deliberately) for intentional relocation). Restore `config.toml`, `secrets.toml`, and `KEEPPEEK_SECRET_*` env vars separately.
4. Start KeepPeek and inspect startup logs immediately; stop and preserve logs on any catalog, path, or migration error.
5. Verify cameras and access grants, open a known older interval in Keep, check coverage and event thumbnails, play a finalized recording, and create a small evidence export whose downloaded MP4 decodes independently (for example with FFmpeg).
6. Run **Recording integrity** (`/recordings`) and **Recording maintenance → Reconciliation** (`Inspect catalog`) to verify missing, unknown, interrupted, and protected items before touching old maintenance jobs.

Repeat these checks on production cutover and retain the old copy until acceptance passes.

## Understand startup recovery

Startup reconciles narrow interrupted states automatically; it is **not** a full media scan or substitute for a consistent backup.

| Area                                   | Automatic startup behavior                                                                                                                                                      | Operator boundary                                                                                                                                       |
| -------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Interrupted recording finalization** | Reconnects eligible catalog rows to finalized siblings; preserves all existing `.active` files untouched.                                                                       | Inspect preserved `.active` files rather than deleting by extension.                                                                                    |
| **Missing bound named/archive root**   | Retains catalog ownership and marks the bound volume/target offline; **never** recreates a missing directory.                                                                   | Restore the original root identity; an empty replacement directory is rejected.                                                                         |
| **Interrupted or missing exports**     | Marks running exports and missing ready artifacts as `failed` (retryable); cleans partial artifacts only inside owned export storage.                                           | Retry requires source recordings to still exist.                                                                                                        |
| **Missing event thumbnails**           | Preserves catalog references and unindexed/temp images; quota cleanup removes only catalog-referenced images in the configured root.                                            | Restore thumbnail volume root to recover missing JPEGs.                                                                                                 |
| **Interrupted maintenance jobs**       | Runs a 2-second recovery pass that settles completion only when staging-directory identity and absence checks prove unlink finished; leaves other claims `Failed` and reserved. | Copied files may change filesystem IDs, causing restored checkpoints to refuse retry properly. See [Recording maintenance](./recording-maintenance.md). |

## Diagnose incomplete recovery

| Symptom                                                       | Next action                                                                                                                                           |
| ------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| Settings returned, recordings did not                         | Restore the catalog and media roots together; a configuration ZIP excludes both.                                                                      |
| Settings returned, generic runtime state did not              | Restore `state-store.db` with its sidecars/counters, then have clients open fresh watches.                                                            |
| Recordings appear missing after a path change                 | Verify mount availability, service-account permissions, effective paths, and original catalog paths before modifying files.                           |
| MP4 files exist on disk but are absent in Keep                | Run **Inspect catalog** in Reconciliation. Unknown files are never auto-adopted, and **Rebuild index** applies only to existing cataloged recordings. |
| Cataloged recording has a valid MP4 but broken playback index | Use the previewed **Rebuild index** remedy if offered (rebuilds indexes without altering media bytes).                                                |
| Reconciliation reports container corruption                   | Preserve the original file and downloaded report; structural fMP4 checks are not a full decode or content checksum.                                   |
| Copied maintenance job refuses retry                          | Preserve the job report and checkpoint evidence; never delete private staging or clear catalog reservations manually.                                 |
| Old ready export is unavailable                               | Check export expiry and artifact presence; export metadata can outlive bytes, and retry needs surviving source recordings.                            |
| Both configuration activation and rollback fail               | Preserve the complete configuration directory (`.backups/restore-journal.json`, `.staged`, `.rollback`) and service logs before manual recovery.      |

**Hard prohibitions:** Never delete unknown media, SQLite sidecars, restore journals, or private maintenance staging to clear an error. **Retain tombstone** records catalog cleanup of missing media—it does **not** recover bytes. Quarantine, arbitrary archive import, and automatic adoption of unknown files are unsupported.

## Record the recovery result

Archive a recovery report capturing: exact KeepPeek build, OS/filesystem, source and destination layouts, backup timestamp, downtime window, commands run, verification checksums, tested pre- and post-backup recording intervals, interrupted-work tests, and any discrepancies. Representative recovery and sustained-operation evidence remains part of the [Alpha qualification matrix](./release-readiness.md).
