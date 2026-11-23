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

The GitHub integration uses credentials owned by the reporting service. A local test fixture uses an in-memory provider, synthetic content, and a fake issue URL. Production hosting, GitHub App installation, abuse limits, and retention jobs belong to the service deployment.
