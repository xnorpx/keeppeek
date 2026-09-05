# Export and import configuration

Use a configuration ZIP to back up your settings, return to an earlier configuration, or transfer
settings to another KeepPeek recorder. KeepPeek calls the import operation **Apply configuration**.
You download one ZIP, upload that same ZIP to the target recorder, and restart to activate it.

This is a configuration backup, not a backup of your recordings or database.

## Before you start

- Connect to the source or target recorder as an **Administrator**. See
  [Authentication and access control](./authentication.md) for access setup.
- Export the target recorder's current configuration before importing a different one. This gives
  you a ZIP you can apply later if you want to return to those settings.
- Plan for a recorder restart when importing. Export does not require a restart or stop recording.
- If you use remote access, make sure you can authenticate with the imported access configuration
  after restart. Its access credentials and camera/integration secrets replace the target's values.

## What the ZIP contains

Every export contains exactly two files at the root of the archive:

| File | Contents |
| --- | --- |
| `config.toml` | Durable settings, including cameras, storage policy, access credential metadata, layouts, templates, notification rules, and MQTT settings. |
| `secrets.toml` | The file-backed secret values used by the configuration. |

**The ZIP contains plaintext secrets and is not encrypted.** Restrict access to it as you would to
camera passwords or access keys. Do not attach it to a public issue or share it in logs, and delete
copies you no longer need. Values supplied only through environment variables are not exported;
configure those separately on the target recorder.

The ZIP excludes `recordings.db`, recording media, thumbnails, sessions, caches, and notification or
MQTT runtime work. Import keeps the target's recording, catalog, and thumbnail paths. It neither
restores nor moves the source recorder's database or media. Other storage settings, such as retention
policy, are part of the imported configuration and should be reviewed afterward.

Keep the ZIP as exported. The current format stores its versioned manifest and checksums in the ZIP
comment, so there is no third file. Do not edit the TOMLs inside it, remove the comment, or repack the
archive before importing. Apply accepts current format-3 exports, not arbitrary two-file ZIPs or older
backup formats.

## Export in Settings

1. Open **Settings**, then **Backup and restore** on the source recorder.
2. Choose **Export ZIP**.
3. Keep the downloaded ZIP in a location with restricted access.

The download is the backup artifact. Export does not delete or replace either live TOML file, and
KeepPeek does not retain a server-side copy or add it to a backup list.

## Import in Settings

1. Open **Settings**, then **Backup and restore** on the target recorder.
2. Under **Configuration ZIP**, select the original exported ZIP. Do not extract it first.
3. Select **Replace config.toml and secrets.toml on restart.**
4. Choose **Apply configuration** and wait for the staged confirmation.
5. Choose **Restart to apply** when you are ready for the recorder to restart.

Apply validates the archive, both TOML documents, target permissions, and available space before
staging. A successful upload leaves the running configuration and live TOML files unchanged. The
replacement takes effect during startup, before the configuration and database owners open.

Only one apply can be pending. Finish its restart and health verification before uploading another
configuration. To return to earlier settings, use the same import steps with the ZIP you exported
before the change.

## Verify after restart

1. Reconnect to the target recorder, using the imported access configuration if it changed.
2. Confirm the expected camera definitions, notification rules, integrations, and storage policy.
3. Check camera and stream health and confirm that the target's existing recordings are available.
4. Check the logs if startup or an integration fails. Environment-only secrets and external services
   must still be available on the target.

KeepPeek uses before-images and a crash-recovery journal to restore the prior configuration pair if
configuration loading or startup health fails. Do not manually modify its journal or staging files.
See [Backup and restore](./backup-and-restore.md#safe-activation) for recovery details.

## Use the CLI

The CLI performs the same export and apply operations as Settings. These examples connect to a
recorder on the local machine:

```sh
keeppeek config --server http://localhost:3000 export --output keeppeek-config.zip
keeppeek config --server http://localhost:3000 apply keeppeek-config.zip --confirm
```

Export requires an output path and creates a new, owner-only file; choose a different filename if it
already exists. Apply requires `--confirm`. Commands write machine-readable JSON to standard output
and diagnostics to standard error.

Check the command's exit status and returned state. A successful apply reports
`RESTORE_STATE_AWAITING_RESTART`; restart the service through your normal service manager, or use
**Restart to apply** in Settings. The CLI does not restart the recorder automatically.

For a remote recorder, use HTTPS and provide its Administrator credential through
`KEEPPEEK_ACCESS_KEY` using your normal secure environment setup. Do not put the key in a URL or CLI
argument.

## Use HTTP

| Request | Result |
| --- | --- |
| `GET /config/export` | A fresh `application/zip` download. |
| `POST /config/apply` | Validate and stage a raw ZIP body; return `202` and `RESTORE_STATE_AWAITING_RESTART`. |

For a local recorder, a POSIX shell example is:

```sh
umask 077
curl --fail http://localhost:3000/config/export --output keeppeek-config.zip
curl --fail -H 'Content-Type: application/zip' \
  --data-binary @keeppeek-config.zip http://localhost:3000/config/apply
```

Send the ZIP as the request body, not as a multipart form. Apply requires `Content-Length`; the curl
command above supplies it for the file upload. Both endpoints require Administrator access and use
`Cache-Control: no-store`.

HTTP `202` means **staged**, not active. Restart the service only after checking the successful apply
response. There is no separate public dry-run, section selection, or path-mapping step, and the old
`/api/backups` endpoints are not supported.

## If an import fails

| Result | What to do |
| --- | --- |
| Invalid or unsupported ZIP (`400`) | Use an unmodified current KeepPeek export. A hand-made ZIP, missing comment, changed checksum, or invalid TOML is rejected. |
| Authentication or permission denied (`401` / `403`) | Connect with Administrator access to the target recorder. |
| Apply already pending (`409`) | Complete the staged apply and restart before submitting another. Do not delete its journal to bypass this check. |
| Missing length or wrong media type (`411` / `415`) | Send a raw ZIP with `Content-Length` and `Content-Type: application/zip`. |
| Upload too large (`413`) | Verify that you selected the configuration export, not a media archive. Each TOML is limited to 16 MiB; compressed and expanded ZIP totals are each limited to 1 GiB. |
| Archive operation busy (`503`) | Let the active export or apply finish, then retry. |

A rejected upload does not replace the live configuration. In Settings, the selected filename remains
available after a validation error so you can correct the selection and retry. Do not restart in
response to a failed upload as though it had been accepted.