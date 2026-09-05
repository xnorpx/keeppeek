# Backup and restore

KeepPeek transfers configuration through two Administrator-only HTTP endpoints:

| Method | Endpoint         | Result                                                                              |
| ------ | ---------------- | ----------------------------------------------------------------------------------- |
| `GET`  | `/config/export` | Download a fresh configuration ZIP as `application/zip`.                            |
| `POST` | `/config/apply`  | Upload the ZIP as `application/zip`, validate it, and stage both TOMLs for restart. |

Export never removes the live files and creates no retained server backup. Apply extracts and
validates the uploaded configuration; it does not delete files from the caller's ZIP. There are no
backup IDs, upload reservations, section selectors, path-mapping requests, or separate dry-run
requests. The retired `/api/backups` routes return `404`.

## What a bundle contains

Format 3 is a ZIP with exactly two ordinary files:

- `config.toml` contains all durable settings, including cameras, storage paths, access credential
  verifiers, layouts, configuration templates, notification rules, and MQTT settings.
- `secrets.toml` contains the secret values used by those settings.

The versioned manifest is stored in the ZIP comment, so the archive has no third manifest member.
The configuration digest and a snapshot revision covering both TOMLs detect changed archive
contents. Apply accepts current format-3 archives, including the ZIP comment produced by export.

The bundle deliberately excludes `recordings.db`, recording media, event thumbnails, sessions,
derived caches, access audit history, credential last-use activity, and all notification and MQTT
runtime work. Protect the recording tree separately when database or media recovery is required.

## Secret policy

Format 3 includes `secrets.toml` in plaintext. A backup can contain camera passwords, access keys,
MQTT credentials, webhook URLs, and notification-provider destinations. Treat the ZIP like the live
owner-only secrets file: restrict access, avoid untrusted storage and messaging systems, and delete
unneeded copies. KeepPeek does not encrypt configuration bundles.

Environment overrides are not copied into `secrets.toml`; only values stored in the file are backed
up. KeepPeek never accepts a secret or passphrase in a CLI argument, URL, query parameter,
diagnostic, or browser storage.

## Apply and restart

Open **Settings → Backup and restore** as an Administrator. **Export ZIP** downloads both TOMLs.
Select a **Configuration ZIP**, confirm replacement of both files, and choose **Apply configuration**.
The same operation is available as one HTTP POST with the ZIP body and a `Content-Length` header.

Validation rejects unsupported formats or schemas, traversal and absolute archive members,
backslashes and drive prefixes, symlinks, directories, encrypted members, duplicate/case-colliding
members, any member other than the two required TOML files, malformed manifests, invalid schemas,
bad checksums, oversized content, and invalid merged configuration.

Apply also checks target permissions and space for both files. The shared settings lock protects
export snapshots and apply planning/staging against concurrent configuration writes. The recorder
keeps its current recording, catalog, and thumbnail paths; neither database contents nor media are
restored or moved.

Success returns HTTP `202` and a ProtoJSON `RestoreRecord` whose state is
`RESTORE_STATE_AWAITING_RESTART`. This means staged, not already active. Live TOMLs remain unchanged
until a controlled restart. In Settings, choose **Restart to apply**; automation must restart the
service after receiving the successful response.

Startup replaces the pair before configuration and database owners open. Both files have exact
before-image digests and a crash-recovery journal. Startup refuses stale targets, reconciles partial
application, and restores the prior pair if configuration loading or startup health fails. Restored
access credentials and secrets take effect on restart.

An unfinished apply blocks another with HTTP `409`. After a restore is healthy, a new validated
apply supersedes its recovery point. To return to an earlier configuration, apply a previously
exported ZIP. There is no separate HTTP rollback operation.

Do not manually edit, move, or delete `.backups/restore-journal.json` or its sibling `.staged` and
`.rollback` files. If startup reports that both activation and rollback failed, preserve the complete
configuration directory and logs before manual recovery.

## Limits and errors

- Compressed and expanded archive: at most 1 GiB each.
- Each TOML document: at most 16 MiB.
- Exactly two archive members, with a manifest bounded by the ZIP comment size.
- One configuration archive operation at a time; another active operation returns `503`.
- `400`: empty, malformed, truncated, unsupported, or invalid configuration archive.
- `401` / `403`: authentication or Administrator authorization required.
- `409`: a configuration apply is already pending.
- `411`: missing or invalid `Content-Length`.
- `413`: upload exceeds the archive limit.
- `415`: request is not `application/zip`.

Responses use `Cache-Control: no-store`. Errors are typed JSON without configuration values,
credentials, or internal paths. Apply uploads use bounded buffers and are removed after processing;
only the staged pair and restart journal remain.

## Automation

The CLI uses the same endpoints as Settings. Export requires a destination and writes an owner-only
file; apply requires `--confirm`. Commands print one machine-readable JSON result to stdout and
diagnostics to stderr. Remote credentials come only from `KEEPPEEK_ACCESS_KEY` and require HTTPS
unless the server is loopback.

```sh
keeppeek config --server http://localhost:3000 export --output keeppeek-config.zip
keeppeek config --server http://localhost:3000 apply keeppeek-config.zip --confirm
```

For direct loopback HTTP use:

```sh
umask 077
curl --fail http://localhost:3000/config/export --output keeppeek-config.zip
curl --fail -H 'Content-Type: application/zip' \
  --data-binary @keeppeek-config.zip http://localhost:3000/config/apply
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

The ignored benchmark reports create, dry-run, and staging latency against their budgets. Refresh
published measurements after the format-3 workload has been measured on the release platform; old
format-2 section and database results are not comparable.
