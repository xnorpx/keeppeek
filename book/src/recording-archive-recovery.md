# Recording archive recovery

A configuration ZIP restores settings and file-backed secrets. Recording recovery also needs the
recording catalog and the files that it indexes. KeepPeek does not provide a public recording-archive
importer, a button to merge another recorder's database, or automatic adoption of unknown MP4 files.

Use this chapter to plan and rehearse an operator-managed filesystem recovery. It describes the
current boundaries; it does not qualify every filesystem, backup product, or crash scenario.

## Inventory the recovery set

Open the storage settings as an Administrator and record the effective paths. Defaults can differ
from a custom `--config` directory, and catalog or thumbnail paths can sit outside recording roots.
Record the operating-system account that owns and runs the recorder.

| Item                                                                                                     | Why it matters                                                                                                                                                |
| -------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `config.toml` and sibling `secrets.toml`                                                                 | Cameras, stable identities, policies, credentials, grants, dashboards, and integration settings. A configuration ZIP can preserve this pair.                  |
| The configured recording catalog, normally `recordings.db`, and any accompanying `-wal` and `-shm` files | Recording identities, paths, indexes, event data, coverage/deletion evidence, and maintenance intent. Copy the database family consistently.                  |
| The runtime state-store database file and any accompanying sidecars                                      | Desired-state documents, leases, namespace and entry revisions. A separate consistent set; excluded from the configuration ZIP. Copy the family consistently. |
| Settings-backed state entries                                                                            | Stored in the `state_store` section of `config.toml`; already covered by the configuration ZIP. Generic runtime entries are not.                              |
| Both medium-term and long-term recording roots                                                           | The MP4 bytes referenced by the catalog. The roots may be the same directory. Preserve relative structure and file names.                                     |
| The configured event-thumbnail directory                                                                 | Event-image bytes. It may be outside the recording root.                                                                                                      |
| Hidden `.exports` under the long-term root                                                               | Export history and artifacts, if you need to retain that local history. Artifacts still have independent expiry rules.                                        |
| Service definitions, environment-only secrets, certificates, and external service configuration          | Deployment dependencies outside the two TOML files. Preserve them through your existing protected operations system.                                          |
| The existing sibling `log-filter` and retained service logs                                              | Diagnostic configuration and investigation evidence outside the configuration ZIP.                                                                            |

Copy the entire selected recording roots, including hidden and interrupted-work entries, rather than
selecting only files ending in `.mp4`. Maintenance checkpoints can depend on private staging entries.
Copying them is preservation, not permission to retry destructive work after a move: filesystem
identities can change when data is copied.

Do not include only the default paths if Settings shows overrides. Do not treat an exported evidence
clip as a full recording backup. Clips cover selected ranges and do not contain the recorder catalog.

## Make a consistent offline copy

1. Finish any pending configuration apply or storage move, then export the current configuration
   ZIP and save it privately. Finish or cancel active exports and maintenance jobs where possible;
   retain reports for unresolved work. If this is failure preservation instead, keep the unfinished
   journals with the copy and do not treat it as an ordinary clean recovery point.
2. Stop KeepPeek through its normal service manager. Confirm the process has exited and no second
   recorder is using the same paths. Recording stops for this maintenance window.
3. Copy the configuration directory and complete archive set while they remain stopped. Include any
   database sidecars that exist, for both the recording catalog and the runtime state store;
   do not manually delete them to make the copy simpler.
4. Preserve ownership and access restrictions, record the source version and paths, and verify the
   copied files using your backup tool's integrity checks. Keep the copy separately from the source
   disk.
5. Start the source recorder again and verify that new recordings finalize normally.

A live copy of just `recordings.db` can disagree with its journal or recording files. KeepPeek's
configuration export lock does not create an atomic snapshot of the catalog, media, and export tree.
If you use storage snapshots instead of a stopped copy, establish and test a consistent capture
procedure for every involved volume and writer. This book does not claim that an arbitrary live
filesystem copy is recoverable.

## Restore the runtime state store before accepting watches

Restore the state-store database family together with its counters: namespace
revisions, entry revisions, and byte counts must return as one unit before the
server accepts new watches. Restoring entries without their counters would let
a fresh write reuse a revision that a previous writer already consumed, which
breaks compare-and-set ordering. After the restore, clients establish fresh
watches and reconcile the new snapshots against current capabilities; there is
no resume across a restore. A configuration ZIP alone does not restore generic
runtime state: it carries only the settings-backed entries stored in
`config.toml`.

## Rehearse on an isolated host

1. Keep the original backup unchanged. Restore a working copy into an isolated environment with
   enough space and compatible file permissions. Start with the same KeepPeek build that created it.
2. Prevent the rehearsal from reaching real notification providers, MQTT consumers, and cameras
   until you deliberately test those connections. Imported credentials and rules are real settings;
   a duplicate recorder can otherwise act on the same devices or send duplicate notifications.
3. Restore the catalog family and recording tree together at the original configured paths where
   practical. Restore the configuration pair and any required environment separately. Do not start
   with missing archive mounts and expect later attachment to be harmless.
4. Confirm the service account can access every path, then start KeepPeek. Preserve startup errors
   and stop if a catalog, path, or migration failure occurs.
5. Verify the expected cameras and access grants. Review a known older interval in Keep, inspect its
   coverage and event images, and play a finalized recording. Create a small evidence export and
   verify that its downloaded media decodes independently.
6. Inspect recording integrity and reconciliation, including missing, unknown, interrupted, and
   protected recordings. Check old maintenance jobs before any retry. Record discrepancies rather
   than treating successful server startup as proof that every recording was recovered.

Catalog paths are meaningful. A same-path recovery avoids inventing a path rewrite. For a deliberate
move on a working installation, use the
[validated storage migration](./upgrades-and-migrations.md#move-storage-deliberately). Do not manually
rewrite catalog SQL or assume a configuration ZIP maps old recording paths: configuration apply
keeps the target's current recording, catalog, and thumbnail paths.

Once the isolated copy is verified, plan the production cutover and repeat the checks. Keep the old
copy until the new recorder has completed the installation's recovery acceptance test.

## Understand startup recovery

Startup can reconcile specific interrupted states for recordings, automatic cleanup, export jobs,
and maintenance. It is not an exhaustive media scan or a replacement for a consistent backup.

- An interrupted recording finalization can reconnect a catalog row to its finalized sibling.
  Existing `.active` files are preserved rather than truncated or deleted merely because of their
  extension.
- Missing cataloged recordings can lose stale rows when their parent directory is available.
  When the parent itself is unavailable, startup retains metadata and reports the unavailable
  storage. Restore the complete tree before starting; an empty mounted directory is different from
  an unavailable mount.
- Export work that was running becomes failed and retryable. Missing ready artifacts also become
  failed. Partial export artifacts are cleaned within the owned export area.
- Maintenance recovery only settles outcomes supported by its retained identity and checkpoint
  evidence. Other interrupted objects remain failed and reserved for Administrator inspection or
  retry. Copying files can change their identity, so a restored checkpoint may properly refuse work.

See [Recording maintenance](./recording-maintenance.md) for exact mutation and retry boundaries, and
[Recording and evidence](./recording-and-evidence.md) for coverage and export behavior.

## Diagnose incomplete recovery

| Symptom                                                       | Next action                                                                                                                                  |
| ------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| Settings returned, recordings did not                         | Confirm that a catalog and media archive were restored. The configuration ZIP excludes both.                                                 |
| Settings returned, runtime state did not                      | Confirm that the state-store database family was restored with its counters. Re-establish watches with a fresh snapshot afterwards.          |
| Recordings appear missing after a path change                 | Check mount availability, service identity, effective paths, and the catalog's original locations before changing files.                     |
| MP4 files exist but do not appear in Keep                     | Inspect reconciliation. Unknown files are not automatically adopted; **Rebuild index** only applies to eligible existing catalog recordings. |
| A cataloged recording has a valid container but a wrong index | Use the previewed **Rebuild index** remedy if offered. It changes indexes, not media bytes.                                                  |
| Reconciliation reports corruption                             | Preserve the original file and report. Structural validation is not a full decode or an end-to-end content checksum guarantee.               |
| A copied maintenance job refuses retry                        | Preserve the job report and original checkpoint evidence. Do not delete private staging or manually clear reservations.                      |
| An old ready export is unavailable                            | Check expiry and artifact presence. Export metadata can outlive its bytes; retry requires source recordings still to exist.                  |
| Both configuration activation and rollback fail               | Preserve the complete configuration directory, its restore journal and staged files, and service logs before manual recovery.                |

Never delete unknown media, database sidecars, restore journals, or maintenance staging merely to
clear an error. A missing file cannot be recovered by retaining a tombstone; that remedy records
catalog cleanup, not restoration. Quarantine, arbitrary archive import, and automatic adoption of
unknown files remain unsupported.

## Record the recovery result

Keep the exact build, platform, filesystem, source and destination layout, backup time, interruption,
commands, checksums, recovered interval, missing data, and observed errors with the recovery report.
Include a known recording before and after the backup boundary and any interrupted work you tested.
The [Alpha qualification matrix](./release-readiness.md) still requires representative recovery and
sustained-operation evidence. One successful rehearsal does not establish every deployment's
recovery behavior.
