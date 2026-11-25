# Bug reporting service contract

An Administrator reviews the report data and chooses whether to include screen media before confirming submission. Recorder credentials and the reporting service secret remain on the recorder. The browser receives a single-report grant only after confirmation. KeepPeek continues to offer local downloads if the service is unconfigured or unavailable.

## Service operations

The reporting service is a separate HTTPS application. Every operation is scoped to the same report identity.

| Operation | Authorization and result                                                                                                                                                                                                                                                                                                                                                                                                   |
| --------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Create    | The recorder authenticates the service request. The request includes an idempotency key, the reviewed v1 manifest, and the requested private artifact retention. The service returns an opaque report ID and a 5-minute grant scoped to the manifest's artifact types and byte counts. Repeating the same key and identical manifest returns the same logical report. A different manifest for the same key is a conflict. |
| Upload    | The browser presents the grant and uploads one artifact by kind. The service enforces the declared type and exact size while streaming. Retries with identical bytes return the existing artifact. Different bytes are a conflict. Partial transfers are discarded.                                                                                                                                                        |
| Finalize  | The grant authorizes submission once all declared artifacts have passed validation. The service binds issue creation to the report ID, including retried or ambiguous upstream responses.                                                                                                                                                                                                                                  |
| Status    | The grant authorizes reading the report's pending, complete, failed, or cancelled state and its issue URL if available.                                                                                                                                                                                                                                                                                                    |
| Cancel    | The grant authorizes cancelling a report before issue creation and removing its staged data. Repeated cancellation returns the same terminal state.                                                                                                                                                                                                                                                                        |

[The submission schema](../schemas/reporting/report-v1.schema.json) defines the manifest. The diagnostics limit is 28 MiB and the optional display recording limit is 100 MiB. The service enforces the combined 128 MiB limit, and counts data received even when a client omits or misstates Content-Length. A grant must identify one report, the permitted actions, scopes, and expiry. A service may issue a replacement grant to the authenticated recorder for a still-pending report without creating another report or extending private retention.

Service errors identify invalid manifests, missing authorization, expired grants, type mismatch, size mismatch, idempotency conflict, incomplete uploads, or an unavailable issue provider. Error responses avoid reproducing submitted fields, auth headers, artifact paths, and service internals.

The recorder creates a grant with `POST /v1/reports`, a service bearer credential, and an `application/json` body containing `idempotencyKey`, `manifest`, and `retentionDays`. The service bounds the request body to 128 KiB and returns `id`, `grant`, and `expires` as Unix milliseconds. It rejects browser-origin requests and returns no CORS headers for grant creation.

## Browser requests

The recorder supplies the configured HTTPS origin and an opaque grant for one report after confirmation. The browser uses a bearer authorization header and sends no cookies. It refuses redirects and sends no referrer.

| Request                                 | Body and response                                                                                                                |
| --------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| `GET /v1/reports/{id}`                  | Returns JSON with `state`. The complete state includes a GitHub `issueUrl`.                                                      |
| `PUT /v1/reports/{id}/artifacts/{kind}` | Accepts one binary body and the declared Content-Type. The kind is `diagnostics` or `screen`. Success returns an empty response. |
| `POST /v1/reports/{id}/finalize`        | Returns the same status representation as GET.                                                                                   |
| `DELETE /v1/reports/{id}`               | Cancels a pending report. Success returns an empty response.                                                                     |

The service permits these methods and authorization headers only for configured recorder origins. The browser bounds the response used as status to 8 KiB. Credentials for creating grants are held by the recorder and are separate from this HTTP surface.

## Issue output and retention

The issue contains the reviewed fields, evidence warnings, a scrubbed diagnostic summary, and the opaque report ID. The service checks the public text and summary again for private values and fails closed when the recorder could not gather the scrub context. Diagnostics and screen media stay private. Maintainer authentication is required to view artifacts. Operators see the retention before submitting. Expiry removes artifacts within at most 30 days. Issue text, screen media, bearer links, and service credentials stay out of request and audit logs.

The GitHub integration uses credentials owned by the reporting service. A local test fixture uses an in-memory provider, synthetic content, and a fake issue URL.

## Recorder HTTP surface

The recorder exposes three Administrator-only endpoints in [the OpenAPI specification](../api/openapi.yaml), each enforcing the existing secure remote-access policy (`426` on plaintext remote HTTP) and `Cache-Control: no-store`:

| Endpoint                 | Purpose and bounds                                                                                                                                                                                                                                                                                                                                                             |
| ------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `GET /reporting/status`  | Returns `{ configured, origin?, retention_days? }` without credentials or secret references. When `[reporting]` is omitted, returns `{ configured: false }`.                                                                                                                                                                                                                   |
| `POST /reporting/grants` | Accepts a bounded (`<= 128 KiB`) `{ idempotencyKey, manifest }` payload, validates it against the v1 schema, resolves any `{secret:KEY}` credential on the recorder, forwards `POST {origin}/v1/reports` over HTTPS with no redirects and a 10-second timeout, registers the issued grant for log redaction, audits `bug_report_submit`, and returns `{ id, grant, expires }`. |
| `POST /reporting/audit`  | Accepts a bounded (`<= 512 B`) closed-enum payload (`start`, `cancel`, `local_download`, `complete`, `failed`) with optional mode and opaque `report_id` and records a non-sensitive entry in the access audit log.                                                                                                                                                            |

`GET /logs/snapshot` also includes an additive per-process `generation` string so the report session can verify that start and stop server log cursors belong to the same recorder epoch and disclose restarts explicitly.

## Production deployment dependencies

Production operation of the external reporting service is a separate deployment responsibility outside recorder core:

- Dedicated HTTPS hosting, TLS lifecycle, and CORS origin allowlisting for configured KeepPeek origins.
- GitHub App ownership, installation, and least-privilege repository permissions (`issues:write` only); KeepPeek clients and recorders never embed GitHub tokens.
- Maintainer authentication and authorization for reading private diagnostics packages and screen recordings.
- Automated retention enforcement that deletes expired private artifacts within at most 30 days.
- Service-side abuse protection, storage quotas, monitoring, and availability commitments.
