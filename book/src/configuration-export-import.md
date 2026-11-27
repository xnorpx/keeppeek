# Export and import configuration

This chapter covers exporting and applying KeepPeek's two-file configuration ZIP through **Settings**, the `keeppeek config` CLI, and HTTP endpoints. Both operations require **Administrator** access; the ZIP contains **unencrypted plaintext secrets** and covers configuration only—never recordings or databases.

## Video walkthrough

Watch a two-file configuration ZIP export, staged apply, restart-required handoff, and validation rejection states in **Settings → Backup and restore**.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/settings.desktop.config-zip.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/settings.desktop.config-zip.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/settings.desktop.config-zip.mp4">Download the configuration ZIP export and apply demo</a>.
</video>

## Prerequisites

- Connect as an **Administrator** (see [Authentication and access control](./authentication.md)).
- Export the target recorder's current configuration ZIP first so you have an exact rollback bundle.
- Schedule a recorder restart for apply—export does not restart KeepPeek or interrupt recording, whereas apply activates only on restart.
- Verify you can authenticate with the imported access configuration after restart, because imported credentials and secrets replace the target's values.

## What the format-3 ZIP contains and preserves

Every export produces an unmodified format-3 ZIP with **exactly two root files** and a versioned manifest + checksums stored in the **ZIP comment** (no third manifest file):

| Component                                                                                                                                                                   | Included in ZIP?    | Transfer and preservation rules                                                                                                                                                                                                                                                                                                                     |
| --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `config.toml`                                                                                                                                                               | Yes                 | All durable settings: cameras (with `profile_identity`), access credential verifiers/grants, dashboards, templates, `[runtime_profiles]`, notification rules, `[cloud_notifications]`, MQTT, `[sharing]`, `[storage.archive]` (preserving raw `{secret:...}` references), and settings-backed `state_store` entries.                                |
| `secrets.toml`                                                                                                                                                              | Yes (**plaintext**) | File-backed secret values referenced by `config.toml`. **Unencrypted**—restrict access like live credentials, never share in issues/logs, and delete unneeded copies.                                                                                                                                                                               |
| Environment secrets (`KEEPPEEK_SECRET_*`)                                                                                                                                   | No                  | Never exported; configure separately on the target host.                                                                                                                                                                                                                                                                                            |
| Target storage ownership & paths                                                                                                                                            | Preserved on target | Apply keeps the target recorder's legacy recording/catalog/thumbnail paths and its `[storage.named_volumes]` and `[storage.metadata]` tables. Uploaded `secrets.toml` must resolve any secret references in those preserved roots to the same target owners, or apply rejects before mutation. Finish or cancel any pending metadata handoff first. |
| Recording catalog, MP4s, thumbnails, fourth-tier archive replicas, `state-store.db`, `guest-review/grants.db`, `cloud-notifications.db`, `log-filter`, runtime queues/audit | No                  | Excluded from the ZIP. `[storage.archive]` settings round-trip cleanly with raw `{secret:...}` references, while replica files and catalog bindings stay outside the ZIP. See [Recording archive recovery](./recording-archive-recovery.md).                                                                                                        |

Keep the exported ZIP intact. Do not edit either TOML inside it, strip the ZIP comment, or repack the archive—apply accepts unmodified current format-3 exports only.

## Export in Settings

1. Open **Settings → Backup and restore** (`/settings#backups`) on the source recorder.
2. Select **Export ZIP**.
3. Store the downloaded archive in a restricted, private location.

Export leaves live TOML files untouched and retains no server-side backup copy.

## Apply in Settings

1. Open **Settings → Backup and restore** on the target recorder.
2. Under **Configuration ZIP**, choose the unmodified exported ZIP (do not extract it).
3. Confirm the upload using the flow that matches your Administrator session:
   - **Local Administrator session:** Select **Replace `config.toml` and `secrets.toml` on restart.** and choose **Apply configuration**. (Local restore retains the server's lockout guard.)
   - **Remote Administrator session:** Choose **Inspect restore archive** to stream the ZIP over your live control session without mutating configuration. Review the candidate authentication plan; if the archive replaces remote sign-in methods, complete fresh replacement Administrator verification, then confirm to stage the exact inspected ZIP. Changing the control session or file invalidates inspection.
4. Wait for **Configuration staged. Restart required.**
5. Choose **Restart to apply** when ready for the restart window.

Apply validates the archive, manifest checksums, both TOMLs, target permissions, preserved storage owners, and free space before staging. Live TOML files stay unchanged until startup activation runs before configuration and database owners open. Only one apply may be pending at a time; to revert later, apply a ZIP exported before the change.

## Verify after restart

1. Reconnect to the target recorder (using the imported credentials if access settings changed).
2. Confirm expected cameras, storage/retention policy, `[storage.archive]` settings, notification rules, and integrations.
3. Verify camera/stream health and confirm existing recordings remain playable.
4. Check `/settings/logs` if any camera or integration fails (especially for missing `KEEPPEEK_SECRET_*` overrides or external services).

If configuration loading or startup health fails, KeepPeek automatically restores the prior `config.toml` and `secrets.toml` pair using its crash-recovery journal. See [Safe activation](./backup-and-restore.md#safe-activation).

## Use the CLI

`keeppeek config` calls the same Administrator-only HTTP endpoints, writes machine-readable JSON to stdout, and prints diagnostics to stderr:

```sh
keeppeek config --server http://localhost:8081 export --output keeppeek-config.zip
keeppeek config --server http://localhost:8081 apply keeppeek-config.zip --confirm
```

| Requirement                     | Rule                                                                                                                                                                                                                                                               |
| ------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `--server`                      | Always pass explicitly. New recorders listen on `8081` by default, while `keeppeek config` currently defaults to `http://localhost:3000`.                                                                                                                          |
| Remote authentication           | Use HTTPS for non-loopback hosts and supply the Administrator key only through `KEEPPEEK_ACCESS_KEY`. Never place keys in URLs or CLI arguments.                                                                                                                   |
| Export destination (`--output`) | Must be a new file (no overwrite). Created with private permissions before any ZIP bytes are written: `0600` on Unix; verified protected DACL (current user, `SYSTEM`, `Administrators`) on Windows, rejecting alternate data streams and unverified destinations. |
| Apply confirmation              | Requires `--confirm`. Returns `RESTORE_STATE_AWAITING_RESTART` on success and **does not restart** the recorder—restart through your service manager or **Restart to apply** in Settings.                                                                          |
| Exit codes                      | `0` success, `1` output failure, `2` invalid CLI usage, `3` server 4xx rejection, `4` transport, protocol, or 5xx error.                                                                                                                                           |

## Use HTTP directly

| Method & path        | Request / response contract                                                                                                                                                                  |
| -------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `GET /config/export` | Returns `200` with fresh `application/zip` body and `Cache-Control: no-store`.                                                                                                               |
| `POST /config/apply` | Accepts raw `application/zip` body (not multipart) with required `Content-Length`; returns `202` ProtoJSON `RestoreRecord` (`RESTORE_STATE_AWAITING_RESTART`) and `Cache-Control: no-store`. |

Loopback POSIX shell example:

```sh
umask 077
curl --fail http://localhost:8081/config/export --output keeppeek-config.zip
curl --fail -H 'Content-Type: application/zip' \
  --data-binary @keeppeek-config.zip http://localhost:8081/config/apply
```

HTTP `202` means **staged**, not active; inspect the response before restarting. Browser downloads and `curl` use client-side destination permissions—restrict those copies separately. There is no HTTP dry-run, section-selection, or path-mapping endpoint, and retired `/api/backups` routes return `404`.

## Validation limits and failure handling

**Hard bounds:** `config.toml` and `secrets.toml` are each limited to **16 MiB**; compressed and expanded ZIP totals are each limited to **1 GiB**; one archive operation runs at a time. Validation rejects traversal/absolute/backslash/drive paths, symlinks, directories, encrypted or duplicate/case-colliding members, extra files, malformed manifests, bad checksums, invalid merged TOML, and conflicting storage-owner secret references. Error responses are redacted JSON without configuration values, secrets, or internal host paths.

| HTTP status   | Meaning                                                             | Next action                                                                                                                               |
| ------------- | ------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| `400`         | Malformed, modified, unsupported, or invalid ZIP/TOML               | Use an unmodified current format-3 KeepPeek export; verify target named-volume secret references and finish any pending metadata handoff. |
| `401` / `403` | Missing auth, non-Administrator, or unverified remote lockout guard | Authenticate as an Administrator; for remote sign-in changes, complete the remote restore inspection and verification flow first.         |
| `409`         | Another configuration apply is already pending                      | Restart and verify the pending apply before submitting a new ZIP. Never delete the restore journal to bypass this guard.                  |
| `411` / `415` | Missing `Content-Length` or wrong `Content-Type`                    | Send a raw `application/zip` body with an explicit `Content-Length`.                                                                      |
| `413`         | Upload exceeds size limit                                           | Confirm you chose a configuration ZIP (≤1 GiB total, ≤16 MiB per TOML), not a media archive.                                              |
| `503`         | Another export/apply is active or backup service unavailable        | Wait for the active operation to complete, then retry.                                                                                    |

A rejected upload leaves live files untouched; never restart after a failed apply as though it succeeded. If both startup activation and automatic rollback fail, preserve the entire configuration directory (including `.backups/restore-journal.json` and staged/rollback files) and service logs before manual recovery. See [Safe activation](./backup-and-restore.md#safe-activation), [Upgrades and migrations](./upgrades-and-migrations.md), and [Recording archive recovery](./recording-archive-recovery.md).
