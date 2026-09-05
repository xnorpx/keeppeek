# Backup and restore

KeepPeek can create and validate a configuration bundle while recording continues. A format-3 ZIP
contains exactly `config.toml` and plaintext `secrets.toml`; it contains no database or media.

Open **Settings → Backup and restore** as an Administrator. **Export ZIP** downloads the two files
directly. Select a **Configuration ZIP**, confirm replacement, and choose **Apply configuration**.
After the upload succeeds, choose **Restart to apply**.

The HTTP surface is `GET /config/export` for download and `POST /config/apply` for a ZIP upload.
There are no retained server backups, upload reservations, or separate dry-run requests. The old
`/api/backups` routes are retired.

## Secrets and media

The ZIP includes the complete file-backed `secrets.toml` in plaintext. It may contain camera
passwords, access keys, MQTT credentials, webhook URLs, and notification-provider destinations.
Restrict the ZIP like the live secrets file and delete copies that are no longer needed. Environment
secret overrides are not copied into the bundle.

`recordings.db`, MP4 recordings, thumbnail JPEGs, sessions, caches, audit activity, and in-memory
notification/MQTT work are excluded. The target recorder keeps its local storage paths during
restore. Protect the recording tree with a separate archive policy when it needs recovery.

## Safe activation

Apply accepts a current format-3 ZIP, including its manifest comment, and validates paths, checksums,
both TOMLs, permissions, and capacity before staging. It returns `202` with
`RESTORE_STATE_AWAITING_RESTART`; this is not an immediate replacement of the live files.

Staging writes owner-only copies beside both target files. Recovery runs before configuration or
databases open, and completion requires successful configuration, HTTP, and camera-worker startup.
KeepPeek restores the prior pair automatically if startup health fails. A pending apply blocks a
second upload from replacing it; a healthy completed restore can be superseded by the next apply.
To return to an older configuration, apply a ZIP exported before the change.

Do not edit `.backups/restore-journal.json` or its staging files manually. The detailed HTTP, limit,
crash-recovery, and CLI contract is in the
[backup and restore operator guide](https://github.com/xnorpx/keeppeek/blob/master/docs/backup-and-restore.md).

## Automation

The `keeppeek config` commands call the same Administrator-only HTTP endpoints. Commands print
machine-readable JSON to stdout. Supply remote authentication only through
`KEEPPEEK_ACCESS_KEY`; never put it in a URL or command argument.

```sh
keeppeek config --server http://localhost:3000 export --output keeppeek-config.zip
keeppeek config --server http://localhost:3000 apply keeppeek-config.zip --confirm
```
