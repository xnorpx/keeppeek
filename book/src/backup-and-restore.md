# Backup and restore

KeepPeek can create and validate a configuration bundle while recording continues. A format-3 ZIP
contains exactly `config.toml` and plaintext `secrets.toml`; it contains no database or media.

For a step-by-step walkthrough, see
[Export and import configuration](./configuration-export-import.md).

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

## What survives a restart

| State                                                                                                                   | Persistence and recovery boundary                                                                                        |
| ----------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| Camera settings, access credentials and grants, dashboards and selections, templates, notification rules, MQTT settings | Durable configuration in `config.toml`; included in a configuration ZIP.                                                 |
| File-backed secrets                                                                                                     | Durable sibling `secrets.toml`; included in plaintext. Environment overrides remain outside the ZIP.                     |
| Recording metadata, events, coverage and maintenance intent                                                             | Recording catalog data; recover with a consistent catalog and media archive.                                             |
| MP4 recordings and event images                                                                                         | Files in configured storage locations; not part of a configuration ZIP.                                                  |
| Evidence-export history and artifacts                                                                                   | Stored in `.exports` under the long-term recording root, with separate history and artifact expiry; not part of the ZIP. |
| Diagnostic log filter                                                                                                   | The existing sibling `log-filter` file persists separately and is not in the two-TOML ZIP.                               |
| Notification delivery jobs, retries, inbox/history, cooldowns and delivery counters                                     | Runtime memory; reset on restart. Saved rule drafts and active rules remain.                                             |
| MQTT pending publications, retries and deduplication history                                                            | Runtime memory; reset on restart. Stored source events are not an automatic replay queue.                                |
| Access audit, credential last-use activity and active sessions                                                          | Runtime memory; reset on restart. Durable identities and grants remain.                                                  |
| Browser sign-in key, media handles and active wake lock                                                                 | Page/browser runtime state; reload or reconnect as required. Saved dashboard wake-lock intent remains configuration.     |

Restart is therefore a recording interruption and a runtime-state reset, even when every saved
setting returns correctly. Collect required audit and delivery evidence before restart. A queue
that was pending before shutdown is not proof of later provider delivery.

See [Recording archive recovery](./recording-archive-recovery.md) for consistent offline copies and
recovery rehearsals, and [Upgrades and migrations](./upgrades-and-migrations.md) for version changes.

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
[backup and restore engineering guide](https://github.com/xnorpx/keeppeek/blob/main/docs/backup-and-restore.md).

## Automation

The `keeppeek config` commands call the same Administrator-only HTTP endpoints. Commands print
machine-readable JSON to stdout. Supply remote authentication only through
`KEEPPEEK_ACCESS_KEY`; never put it in a URL or command argument.

CLI exports create a new private file before writing archive bytes. On Unix, the file mode is
`0600`. On Windows, a protected access list permits the current user, SYSTEM, and Administrators;
the CLI verifies that protection before writing. A destination that cannot provide this protection
is rejected. Existing files and Windows alternate data streams are not export destinations.
Browser downloads use the browser's destination permissions; restrict those copies separately.

```sh
keeppeek config --server http://localhost:8081 export --output keeppeek-config.zip
keeppeek config --server http://localhost:8081 apply keeppeek-config.zip --confirm
```

Use your recorder's configured port if it differs from the default `8081`. Export does not overwrite
an existing file. Apply does not restart the recorder automatically; check its exit status and
`RESTORE_STATE_AWAITING_RESTART` result before restarting. Exit code `2` means invalid CLI usage,
`3` means a stable server 4xx rejection, and `4` means a transport, protocol, or server failure.

Keep `--server` explicit in automation: the config CLI currently uses port `3000` if it is omitted,
which differs from a new recorder's default listener port.
