# Backup and restore

KeepPeek can create and validate a recovery bundle while recording continues. The bundle contains
configuration and critical metadata, not recording media or resolved secrets.

Open **Settings → Backup and restore** as an Administrator. From there you can:

- create and download a versioned backup;
- upload and inspect a backup without changing live state;
- map source storage paths to this recorder;
- run a compatibility, secret, capacity, conflict, and restart dry check;
- stage an accepted restore for restart;
- inspect post-restart health evidence;
- stage rollback during the retained 30-minute recovery window;
- delete a retained server backup.

Each backup shows its byte size, SHA-256 verification, included sections, schema versions, source
platform, omitted data, and required external secret references. KeepPeek retains at most 16 managed
backups. Restore plans expire after ten minutes.

## Secrets and media

Default bundles preserve exact `{secret:KEY}` references and replace inline private values with new
external references. Put every required value in the target recorder's owner-only `secrets.toml` or
matching `KEEPPEEK_SECRET_<KEY>` environment variable before dry run. The bundle never includes
`secrets.toml`, sessions, resolved camera credentials, access keys, MQTT passwords, notification
provider destinations, or private key material.

MP4 recordings and thumbnail JPEGs remain in their configured archive. The backup carries catalog
metadata and a thumbnail inventory, then requires explicit target path mappings. Protect and copy
media with a separate archive policy.

## Safe activation

Dry run is non-mutating. It validates every selected section and reports blocking issues, warnings,
path/capacity evidence, migrations, and restart consequences. Activation is unavailable until all
blocking issues are resolved and requires explicit confirmation.

Staging writes and verifies owner-only files beside their targets but leaves live state untouched.
Restart KeepPeek to activate. Recovery runs before configuration or databases open, and completion
requires successful configuration, HTTP, and camera-worker startup. KeepPeek retains the prior state
for 30 minutes and automatically restores it if startup health fails. A requested rollback also
activates on restart.

Do not edit `.backups/restore-journal.json` or its staging files manually. The detailed section,
limit, migration, crash-recovery, CLI, and benchmark contract is in the
[backup and restore operator guide](https://github.com/xnorpx/keeppeek/blob/master/docs/backup-and-restore.md).

## Automation

The `keeppeek backup` commands call the same Administrator-only HTTP ProtoJSON API. Commands print
machine-readable JSON to stdout. Supply remote authentication only through
`KEEPPEEK_ACCESS_KEY`; never put it in a URL or command argument.

```sh
keeppeek backup --server http://localhost:3000 create --output keeppeek-backup.zip
keeppeek backup --server http://localhost:3000 inspect <backup-id>
keeppeek backup --server http://localhost:3000 dry-run <backup-id> \
  --map config-directory=/target/config
keeppeek backup --server http://localhost:3000 restore <plan-id> <archive-sha256> --confirm
```

See the public [backup HTTP API](https://github.com/xnorpx/keeppeek/blob/master/api/backup.md) for
ProtoJSON messages, binary transfer endpoints, errors, and audit behavior.
