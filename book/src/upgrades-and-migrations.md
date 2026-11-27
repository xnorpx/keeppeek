# Upgrades and migrations

This chapter covers manual KeepPeek application upgrades, platform-specific service replacement, automatic startup migrations, deliberate storage relocation, and upgrade rollback. Host-operator privileges and **Administrator** access are required; KeepPeek has **no automatic update checker or self-updater**—always perform application upgrades separately from storage moves or configuration imports, and rehearse on an isolated host during Alpha qualification.

## Pre-upgrade checklist

**Settings → System & updates** shows the current version, host, and uptime, and provides **Restart**; **Update channel** and **Update check** are intentionally unavailable. Before replacing the binary or container:

1. Record the running version, install method, OS service account, listener address, and effective config, catalog, recording, and thumbnail paths (service accounts often use a different config directory than interactive logins).
2. Review target release notes and [Release readiness and known limitations](./release-readiness.md); retain the exact previous package, installer, or container image digest for rollback.
3. Export a private [configuration ZIP](./configuration-export-import.md) and record any `KEEPPEEK_SECRET_*` environment overrides and external dependencies outside the ZIP.
4. Capture a consistent offline [recording archive recovery copy](./recording-archive-recovery.md) (catalog + state stores + media). A configuration ZIP cannot recover a failed disk or catalog migration.
5. Finish any staged configuration apply, pending storage move, active exports, or maintenance jobs; save reports for unresolved work and notify users of the recording interruption.
6. Stop KeepPeek via its service manager and confirm exit—**never run two versions against the same directories**, and **never overwrite `config.toml` with a sample file** (credentials, grants, dashboards, templates, profiles, and notification rules live there).

## Replace the application

| Platform              | Package / artifact                         | Service unit                        | Upgrade behavior & watchouts                                                                                |
| --------------------- | ------------------------------------------ | ----------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| **Linux (Docker)**    | Release image (pin digest)                 | Container runtime                   | Keep `/config` and media mounts; UID `65532` must keep write access; writable container layer is discarded. |
| **macOS**             | Apple Silicon (`arm64`) `.dmg` + `.sha256` | Per-user `launchd` (`com.keeppeek`) | Opening new `KeepPeek.app` rewrites the LaunchAgent plist; recheck local service edits.                     |
| **Windows**           | Architecture installer                     | SCM (`KeepPeekService`)             | Installer stops, deletes, and recreates the service; recheck custom SCM account/recovery/env settings.      |
| **Standalone binary** | Platform binary                            | Custom / manual                     | Pass identical `--config <path>` and environment; sibling `secrets.toml` follows `--config`.                |

### Docker on Linux

Pull the target image (record its digest; `latest` is mutable), stop the existing container, and start the replacement with identical `/config` and media mounts, ports, user mapping, and environment variables. Do not remove named volumes or host recording directories during an upgrade; ensure UID `65532` retains write permissions on mounted directories. See [Get started](./get-started.md#linux-with-docker) for the mount contract.

### macOS

Download `keeppeek-<version>-macos-aarch64.dmg` (Apple Silicon `arm64` only) and its `.sha256` file, verify with `shasum -a 256 -c`, drag the new `KeepPeek.app` into `/Applications`, and open it once. The launcher reinstalls and starts the per-user `com.keeppeek` launch agent (starts at user login, not a system-wide boot daemon) while keeping `~/Library/Application Support/keeppeek`.

Verify service state and logs, and reapply any deliberate local LaunchAgent customizations:

```sh
launchctl print gui/"$(id -u)"/com.keeppeek
tail -n 100 ~/Library/Logs/KeepPeek/keeppeek.log ~/Library/Logs/KeepPeek/keeppeek-error.log
```

See the [macOS installation guide](https://github.com/xnorpx/keeppeek/blob/main/docs/macos-installation.md) for signing and notarization details.

### Windows

Run the installer for your host architecture. When `KeepPeekService` exists, the service component is selected by default: the installer stops and removes the existing registration, replaces `keeppeek.exe` and `keeppeek-service.exe`, and recreates and starts `KeepPeekService`. Record custom SCM identity, startup, recovery, and environment settings before running the installer and verify them afterward, because service recreation does not preserve SCM customizations.

From an elevated PowerShell prompt, inspect or manually control the service (poll `sc.exe query` between `stop` and `start`, because `sc.exe stop` returns before exit completes):

```powershell
sc.exe query KeepPeekService
sc.exe qc KeepPeekService
sc.exe stop KeepPeekService
sc.exe start KeepPeekService
```

Resolve any failure to stop the old service before retrying the installer. Service logs default to `keeppeek-service.log` beside the service account's `config.toml`; `[logging] service = "event_log"` writes to the Windows Application event log once the source is registered. See the [Windows service guide](https://github.com/xnorpx/keeppeek/blob/main/docs/windows-service.md).

### Standalone binary and slim supervisor

Stop the running process, replace the executable, and start it with the same `--config <path>` flag and environment. `--config` selects both the TOML file and sibling `secrets.toml`. A fresh config initializes named storage beside itself; an existing metadata binding keeps its recorded owner.

KeepPeek also provides the `keeppeek-supervisor` crate and three slim parent hosts (`keeppeek-supervisor` CLI, `keeppeek-service.exe` on Windows, and opt-in managed local server mode in `keeppeek-desktop`). The supervisor stores immutable content-addressed executables under `<supervisor_root>/versions/<slot_id>/keeppeek[.exe]` (and optional independent `<supervisor_root>/vision/<slot_id>/keeppeek-vision[.exe]`) alongside an atomic `supervisor-state.json` journal and `<supervisor_root>/runtime/` live control files (`supervisor.lock`, `supervisor-status.json`, `control-<component>.json`):

- **Three explicit update channels:**
  - `release`: latest compatible SemVer release on GitHub Releases (`xnorpx/keeppeek`). Enforces Ed25519 Manifest v2 signatures pinned to the release channel key set.
  - `main`: rolling `main` branch builds published under the `master` prerelease tag. Uses Manifest v2 signatures pinned to a separate `main`-channel key set (or explicit dev-only `--allow-unsigned-sha256` + `--allow-unsigned-main` opt-in).
  - `local`: local developer builds staged directly from disk (`target/debug/keeppeek[.exe]` or `target/release/keeppeek[.exe]`). Requires explicit runtime opt-in (`--allow-local`) and is never persisted in `config.toml`.
- **SemVer major-version fresh-install policy:**
  - Pre-1.0 (`0.x.y`): breaking API/schema changes may land within `0.x.y`, and in-place updates are allowed across `0.x.y`.
  - Post-1.0 (`>= 1.0.0`): minor and patch updates are additive and allowed in place on the same major line.
  - Any major version increment (`0.x -> 1.0`, `1.x -> 2.0`, etc.): blocked in place (`MajorUpgradeRequiresFreshInstall`) because the supervisor executable itself must be updated via a fresh install. `keeppeek-supervisor check` surfaces both the latest compatible in-place version and any newer major version requiring a fresh install.
- **Security and lifecycle guarantees:** Every supervisor launch validates owner-only ACLs on `<supervisor_root>` (when `--enforce-root-acl` or `--allow-local` is active, and unconditionally in `keeppeek-service.exe`), re-verifies Manifest v2 Ed25519 signatures and binary digests at launch time (without expiry bricking), automatically rolls back across probation failures, and garbage-collects unreferenced slots down to the 2 most recent.

```sh
# Check for compatible updates (and surface newer major versions requiring fresh install)
keeppeek-supervisor check --root <dir> --channel release --json

# Stage and live-apply a signed release or main update into a running supervisor
keeppeek-supervisor update --root <dir> --channel release --apply --wait --json
keeppeek-supervisor update --root <dir> --channel main --apply --wait --json

# Fast local developer build hot-swap loop (requires --allow-local on the running supervisor)
keeppeek-supervisor run --root <dir> --config <path/to/config.toml> --allow-local --probation-ms 500
keeppeek-supervisor update --root <dir> --channel local --binary target/debug/keeppeek --apply --wait --json

# Inspect persisted + live state, or manually roll back
keeppeek-supervisor status --root <dir> --json
keeppeek-supervisor rollback --root <dir> --component core --apply --wait
```

Background auto-polling remains disabled by default (`[updates].mode = "notify"`). On Linux `systemd` deployments, retain the default `KillMode=control-group` so hard supervisor termination also cleans up supervised child processes.

## What startup migrates

Startup automatically imports supported legacy sidecar files into `config.toml` when the corresponding current section is absent, writes `config.toml` atomically, and removes the retired input on success. Keep an offline pre-upgrade copy if you need original inputs.

| Legacy / pre-upgrade input               | Current location or startup behavior                                                                                                                                                                         |
| ---------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `access.toml`                            | Moves identities, verifiers, roles, revisions, and grants into `[access_credentials]`; removes `access.toml`.                                                                                                |
| `peek-layouts.json`                      | Moves layouts and per-user selections into `[peek_layouts]` (legacy private layouts become server-owned restricted dashboards); removes `peek-layouts.json`.                                                 |
| `configuration-templates.json`           | Moves templates into `[configuration_templates]`; removes `configuration-templates.json`.                                                                                                                    |
| `notifications.db` + sidecars            | Moves rule drafts and active rules into `[notifications]` (delivery history and pending runtime work are **not** replayed); removes retired DB files.                                                        |
| `mqtt-forwarder.db` + sidecars           | Retired and removed; current MQTT queues and deduplication live in memory and do not replay from this file.                                                                                                  |
| Legacy camera runtime-profile IP targets | Atomically assigns missing camera `profile_identity` UUIDs, converts targets (`missing-ip:<address>` if absent), preserves raw `{secret:...}` IPs, and writes `identity_schema = 1` in `[runtime_profiles]`. |
| Recording catalog SQLite schema          | Applies schema, index, and playable-summary migrations in place on open.                                                                                                                                     |

**Migration boundaries:** When a `config.toml` section already exists, it is authoritative—legacy files never merge into it, and recreating retired files has no effect. There is **no general-purpose downgrade command**: older binaries reject `identity_schema = 1` fail-closed and may be unable to open a migrated recording catalog. Keep pre-upgrade config, catalog, and media together if rollback might be required. See [Configuration reference](./configuration-reference.md) and [What survives a restart](./backup-and-restore.md#what-survives-a-restart).

## Move storage deliberately

Use **Settings → Storage & retention → Named storage volumes** to relocate media or metadata safely without manual SQL edits.

### 1. Add a destination volume

1. Create a private destination directory owned by the recorder account on a supported local filesystem (on Windows, local **NTFS** only; ReFS, FAT, and network shares are rejected for owned storage).
2. Open **Settings → Storage & retention → Named storage volumes → Edit volume draft**, add the volume's stable ID, path, roles, byte limits, and placement rules, and save.
3. Restart KeepPeek to activate the draft. Placement rules govern **future** objects only; existing objects keep their catalog owner.

If migrating from separate legacy capture and archive roots and startup reports a pending journaled transfer, boot with the saved pre-migration configuration and both original roots mounted so KeepPeek completes the transfer, then reschedule the move.

### 2. Drain and move stored media

1. Select **Stop new writes** on the source volume (operator drain survives restart; active fragments finish writing normally).
2. Under **Move stored objects**, select the source volume and choose **Load stored objects**, then select a compatible destination volume and role.
3. Choose **Preview move** (one object) or **Preview next 16 objects** (batch up to 16), verify file count and total bytes, then select **Confirm this move** or **Confirm batch of N moves**.
4. Track progress or cancel before authoritative publication under **Move jobs**. **Stop after current request** halts further batch confirmations while leaving already-admitted jobs running.

Each move copies into a confined temporary file, verifies byte length and SHA-256, publishes the new catalog owner, and retires the source copy only after active readers finish. Interrupted jobs resume from their durable journal, and lost confirmation replies reconcile against the original job ID. An expired or stale preview requires a fresh review. Removing a volume definition requires stopped writes, zero owned/reserved bytes, no unfinished jobs or cleanup receipts, and no active archive-policy references; removing a definition leaves its directory and unrelated files untouched.

### 3. Relocate metadata (catalog and export history)

1. Create and save a `disabled` named volume assigned **only** the `metadata` role.
2. Under **Catalog and export history**, select that destination and choose **Preview metadata move** to review downtime and required bytes.
3. Choose **Confirm metadata move** (locks storage settings until restart or **Cancel pending metadata move**), then trigger **Restart and move storage** or restart the service.

Startup copies and verifies both the catalog snapshot and export history before media workers resume, retaining the previous files for recovery. A missing bound root stays offline and is **never** recreated. If activation fails, preserve both copies and the pending configuration—never edit journals, overwrite a conflicting destination, or clear fencing records. See [Named storage volumes](./configuration-reference.md#named-storage-volumes).

## Verify and roll back

After startup:

1. Check server version, camera count, credentials/grants, dashboards, storage roots, and integration status.
2. Test live playback, one newly finalized recording, and one known older recording; check `/recordings` integrity and any retained export/maintenance history.
3. Reconnect remote clients (sessions and browser keys do not survive restart).

**Rollback boundary:** Configuration ZIP automatic rollback covers only `config.toml` and `secrets.toml`—never application binaries, catalog schema upgrades, or storage moves. If an upgrade fails, stop KeepPeek, preserve the failed directory state, restore an isolated complete pre-upgrade copy (binary + config + catalog + media), and verify it before replacing the failed deployment. **Never run an older binary against the sole copy of a potentially migrated catalog.** Record results and gaps in the [qualification record](./release-readiness.md#promotion-record).
