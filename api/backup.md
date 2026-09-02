<!-- SPDX-License-Identifier: MIT -->

# Backup HTTP API

`backup.proto` defines the canonical `keeppeek.backup.v1` control model. KeepPeek carries those
messages as ProtoJSON over authenticated HTTP. ZIP artifacts use separate bounded binary transfers;
they are never base64-encoded inside JSON.

Every endpoint requires an Administrator principal. A direct local request is Administrator by the
configured network policy. A remote request sends a named Administrator credential in the
`Authorization: Bearer <UUID>` header and uses HTTPS when secure remote access is required.

## ProtoJSON

Requests use `Content-Type: application/json` and responses use `application/json`. Field names are
lower camel case, enum values use their protobuf names, 64-bit integers are JSON strings, and unknown
fields fail validation. Successful responses and error responses set `Cache-Control: no-store`.

Errors use `BackupError`. Its `code` and `field` are stable machine values; `message` is safe for an
operator; `retryable` distinguishes a transient conflict or busy server from a rejected artifact.
Unexpected filesystem and database errors return a generic `BACKUP_ERROR_CODE_INTERNAL` response
without exposing host paths or credentials.

## Endpoints

| Method | Path                                     | Request                      | Success                |
| ------ | ---------------------------------------- | ---------------------------- | ---------------------- |
| `GET`  | `/api/backups/capabilities`              | none                         | `BackupCapabilities`   |
| `GET`  | `/api/backups`                           | none                         | `ListBackupsResponse`  |
| `POST` | `/api/backups`                           | `CreateBackupRequest`        | `201 BackupRecord`     |
| `POST` | `/api/backups/uploads`                   | `BeginBackupUploadRequest`   | `201 BackupTransfer`   |
| `PUT`  | `/api/backups/transfers?transfer_id=...` | `application/zip`            | `201 BackupRecord`     |
| `POST` | `/api/backups/downloads`                 | `BeginBackupDownloadRequest` | `BackupTransfer`       |
| `GET`  | `/api/backups/download?backup_id=...`    | none                         | `application/zip`      |
| `POST` | `/api/backups/inspect`                   | `InspectBackupRequest`       | `BackupRecord`         |
| `POST` | `/api/backups/restore-plans`             | `CreateRestorePlanRequest`   | `201 RestorePlan`      |
| `POST` | `/api/backups/restores`                  | `ActivateRestoreRequest`     | `202 RestoreRecord`    |
| `POST` | `/api/backups/restores/get`              | `GetRestoreRequest`          | `RestoreRecord`        |
| `POST` | `/api/backups/rollbacks`                 | `RollbackRestoreRequest`     | `202 RestoreRecord`    |
| `POST` | `/api/backups/delete`                    | `DeleteBackupRequest`        | `DeleteBackupResponse` |

The server limits JSON control bodies to 16 MiB and error bodies to 64 KiB. Capabilities report the
current archive, expanded-content, section, member-count, retention, plan, and rollback limits.

## Create and download

Create a default reference-only bundle:

```http
POST /api/backups HTTP/1.1
Authorization: Bearer 550e8400-e29b-41d4-a716-446655440000
Content-Type: application/json
Accept: application/json

{"clientRequestId":"31d64046-51de-4fee-80af-35267db6f2aa","sections":[],"expectedArchiveBytes":"0"}
```

An empty `sections` list selects every section currently reported by `supportedSections`. Reuse the
same `clientRequestId` to replay one creation intent safely. The returned `BackupRecord` contains the
artifact size, SHA-256 digest, validated manifest, and completion evidence.

Download the returned `backupId` as ZIP:

```http
GET /api/backups/download?backup_id=2fb4dbd2-0142-49d3-9c36-a1f8665d2caf HTTP/1.1
Authorization: Bearer 550e8400-e29b-41d4-a716-446655440000
Accept: application/zip
```

## Upload and inspect

Reserve the exact upload length first. The response supplies a short-lived `transferId`, transfer
URI, maximum bytes, and expiry. Send the ZIP with the same `Content-Length`; the server streams it to
an owner-only temporary file, verifies the optional whole-archive digest, performs full archive and
section validation, and only then promotes it.

```json
{
  "clientRequestId": "05974052-a35a-4d97-86ad-4336369892b3",
  "fileName": "keeppeek-recovery.zip",
  "contentLength": "14595"
}
```

Inspection is read-only:

```json
{ "backupId": "2fb4dbd2-0142-49d3-9c36-a1f8665d2caf" }
```

## Dry run, activation, and rollback

Read capabilities immediately before planning. Use its `targetRevision` and `targetPaths` to create
an explicit mapping for every `sourcePath` in the backup manifest. The plan binds the exact artifact
digest, selected sections, canonical target paths, target revision, migrations, required external
secrets, capacity checks, warnings, restart consequences, and a ten-minute expiry.

```json
{
  "clientRequestId": "a0ec8378-c863-4690-9191-b96570cdf72a",
  "backupId": "2fb4dbd2-0142-49d3-9c36-a1f8665d2caf",
  "sections": [],
  "pathMappings": [
    {
      "kind": "BACKUP_PATH_KIND_CONFIG_DIRECTORY",
      "sourcePath": "/source/config",
      "targetPath": "/target/config"
    }
  ],
  "expectedTargetRevision": "f4e38b..."
}
```

Do not activate unless `canActivate` is true. Activation requires `confirm: true` and the plan's
exact `archiveSha256`. It stages and verifies all targets, persists a restart journal, and returns
`RESTORE_STATE_AWAITING_RESTART`. Restart KeepPeek to activate. Startup applies the journal before
opening configuration or databases and records final health checks only after HTTP and camera
workers start.

A completed restore retains its before-images for 30 minutes. Request rollback with the active
`restoreId` and `confirm: true`, then restart. Expired rollback points are deleted. A crash or failed
startup before health confirmation restores all available before-images automatically.

## Audit

KeepPeek audits create, list, upload, download, inspect, dry run, activation, restore status,
rollback, deletion, and malformed-request failures. Audit events contain the principal, action,
result, and bounded target identifier. They never contain bundle bytes, JSON bodies, credentials,
secret values, or path contents.
