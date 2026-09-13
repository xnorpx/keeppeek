# Upgrades and migrations

Upgrade the application separately from moving storage or importing configuration. Each operation
has a different recovery boundary. KeepPeek is in Alpha qualification; keep a tested recovery copy
and rehearse upgrades on an isolated installation before changing a recorder you depend on.

## Prepare an upgrade

1. Record the running version, installation method, service account, listener address, and effective
   configuration, catalog, recording, and thumbnail locations. A service account can use a different
   configuration directory from your interactive login.
2. Read the target release notes and [known limitations](./release-readiness.md). Retain the exact
   previous package or container image so you can identify what was running.
3. Export a [configuration ZIP](./configuration-export-import.md). Keep it private: it contains
   plaintext file-backed secrets. Record any environment-only secrets and external dependencies in
   your existing secure operations system; the ZIP does not include them.
4. Arrange a consistent [recording archive recovery copy](./recording-archive-recovery.md), including
   the catalog and media. A ZIP alone cannot restore these after a failed disk or catalog migration.
5. Complete any already-staged configuration apply or storage move before starting a separate
   upgrade. Finish or cancel active exports and maintenance jobs where possible. Preserve reports for any
   unresolved maintenance work. Record the expected recording interruption and notify the people
   who rely on the installation through your normal process.
6. Stop the recorder through its service manager before replacing application files or taking an
   offline archive copy. Confirm it is stopped; do not run two versions against the same directories.

Do not upgrade by replacing `config.toml` with a sample. Server-managed credentials, camera grants,
layouts, templates, and notification rules live in that file alongside ordinary settings.

## Replace the application

### Docker on Linux

Keep the existing `/config` mount and any additional recording mounts. Pull the selected release
image, stop the old container, and create its replacement with the same mounts, ports, user access,
and environment. Record the image digest if you need to reproduce the installation; `latest` can
refer to a different build later.

Container replacement does not preserve data that was left only in the container's writable layer.
Do not delete a named volume or host recording directory during an application upgrade. The image
runs as user `65532`; the replacement must still be able to write its mounted directories.
See [Get started](./get-started.md#linux-with-docker) for the basic mount contract.

### macOS

Download the Apple Silicon DMG and its matching checksum file. Verify the checksum, replace
`/Applications/KeepPeek.app`, then open the new application once. The launcher reinstalls and starts
the per-user `com.keeppeek` launch agent. It starts after that user logs in; it is not a system-wide
boot daemon.

Check the loaded service and logs:

```sh
launchctl print gui/"$(id -u)"/com.keeppeek
tail -n 100 ~/Library/Logs/KeepPeek/keeppeek.log ~/Library/Logs/KeepPeek/keeppeek-error.log
```

Keep the existing configuration directory. The launcher writes its launch-agent definition again,
so preserve and recheck any deliberate local service customizations. Package-signing and
notarization details are in the
[macOS installation guide](https://github.com/xnorpx/keeppeek/blob/main/docs/macos-installation.md).

### Windows

Run the installer for the host architecture. An existing `KeepPeekService` selects the service
component by default. The installer stops and removes that registration before replacing binaries,
then recreates and starts it when the service component is selected.

Record custom service identity, startup, recovery, and environment settings before upgrading;
recreating the service is not a promise to preserve SCM customizations. Recheck them before relying
on unattended operation. Configuration paths resolve in the process account's environment.

Use an elevated prompt for service operations:

```powershell
sc.exe query KeepPeekService
sc.exe qc KeepPeekService
```

For a planned manual stop and start, use `sc.exe stop KeepPeekService` and
`sc.exe start KeepPeekService`, checking `sc.exe query KeepPeekService` between operations. A stop
request can return before the service has finished stopping. The installer manages these steps
when performing its own service upgrade.

If the installer cannot stop the old service, resolve that failure before retrying the installer.
If startup fails, inspect the service account's configuration and logs. The default destination is
`keeppeek-service.log` beside its configuration; `[logging] service = "event_log"` uses the Windows
Application log after the source has been registered. See the
[Windows service guide](https://github.com/xnorpx/keeppeek/blob/main/docs/windows-service.md).

### Standalone binary

Stop the current process, replace the executable with the intended build, and start it with the same
configuration selection and environment. The `--config <path>` option selects the TOML file and its
sibling `secrets.toml`; it does not automatically relocate omitted recording paths.

## What startup migrates

Current KeepPeek loads durable settings from `config.toml`. Older installations may still have
separate subsystem files. Startup imports their supported settings when the corresponding current
section is absent, writes the current configuration, and removes the retired file after successful
migration. Preserve an offline pre-upgrade copy if you need the original inputs.

| Legacy input                                  | Current location or behavior                                                                                                              |
| --------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| `access.toml`                                 | Credential identities, verifiers, roles, revisions, and grants move to `[access_credentials]`.                                            |
| `peek-layouts.json`                           | Layouts and per-user selection move to `[peek_layouts]`. Legacy private layouts become server-owned dashboards with restricted audiences. |
| `configuration-templates.json`                | Camera templates move to `[configuration_templates]`.                                                                                     |
| `notifications.db` and its database sidecars  | Rule drafts and active rules move to `[notifications]`; delivery history and pending runtime work are not a replay queue.                 |
| `mqtt-forwarder.db` and its database sidecars | Retired and removed. Current MQTT pending work and deduplication are in memory and are not replayed from this file.                       |

When a current configuration section already exists, it is authoritative; do not expect an old file
to merge additional settings into it. Do not recreate retired files to change settings.

The recording catalog also performs schema and index migrations as it opens. There is no supported
general-purpose downgrade command or guarantee that an older binary can read a catalog after a newer
binary has used it. Keep the pre-upgrade catalog and media together if a downgrade might be needed.

See the [configuration reference](./configuration-reference.md) for current fields and
[Backup and restore](./backup-and-restore.md#what-survives-a-restart) for durable versus runtime state.

## Move storage deliberately

Use **Settings** and its storage editor. Set the new locations, then choose whether to leave existing
recordings where they are or **Move existing storage during restart**. Review all changes and the
space assessment before saving. A leave choice is not a media transfer: retain access to the old
locations, and verify how the selected catalog will expose existing recordings.

For a move, KeepPeek writes a pending `[storage_migration]` journal into the existing configuration.
On restart it moves the selected recording roots and separately configured catalog and thumbnail
paths, including catalog sidecars when needed, and updates stored recording paths. Overlapping or
conflicting destinations are rejected. Cross-filesystem moves can require copying the data and
therefore take longer than a rename.

Reserve a maintenance window and enough destination capacity. If startup reports a different file
already at a destination, stop and inspect both copies; do not overwrite one to bypass the check.
Do not edit the migration journal or remove partial files to force progress. Preserve source,
destination, configuration, and logs before requesting recovery help. This workflow is a storage
move, not an importer for an unrelated recording archive.

## Verify and recover

After startup, check the version, expected camera count, credentials and grants, dashboards, storage
locations, and provider settings. Test live playback, a finalized new recording, and an older known
recording. Check recording integrity and any pending maintenance or export history. Reconnect remote
clients; sessions and their browser keys are not restored across a reload or restart.

An accepted configuration ZIP has its own automatic two-file startup rollback. That mechanism does
not roll back an application binary, catalog schema, or storage move. If an application upgrade
fails, stop the recorder and preserve its failed state before recovery. Restore an isolated copy of
the previous complete installation and verify it before replacing the failed deployment. Do not
run the older binary against the only copy of a possibly migrated catalog.

Record the result, interruption, recovered data, and any remaining gap in the installation's
[qualification record](./release-readiness.md#promotion-record).
