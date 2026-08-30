# Evidence exports

KeepPeek exports indexed recording fragments into a new MP4 without modifying source recordings.
The browser submits a range of at most two minutes and can leave the page while the server finishes
the job.

## Durable history

Export history is stored in `.exports/history.json` below the configured long-term storage root.
The private file is replaced atomically and contains protocol records plus internal lifecycle,
requester, and per-attempt artifact identities. Host paths are not serialized. On load, KeepPeek
reconstructs paths only as
`.exports/<validated-job-id>/<validated-artifact-id>/<validated-file-name>`.

Metadata is retained for 30 days, bounded to 500 jobs, and read from a file capped at 8 MiB.
Completed artifact bytes expire after 24 hours, independently of metadata. Expired history remains
useful for retry until metadata retention removes it. A ready record whose artifact is missing
becomes failed and retryable.

An export that was running when the server stopped becomes failed and retryable on restart. Any
partial files in that job's isolated directory are removed during recovery.

## Duplicate ranges

The output identity consists of source, stream, requested range, effective partial-range behavior,
and timestamp burn-in behavior. Event labels and generated filenames are not identity.

- An identical running job is returned instead of starting duplicate work.
- An identical ready artifact is offered for reuse in Keep, with a separate **Create fresh export**
  action.
- Other active or ready ranges that overlap the draft are shown as advisory history.
- Partial, failed, cancelled, expired, missing, and differently configured artifacts are never
  silently substituted.

Event detail opens Keep with the main stream, the exact event identity and timestamp, 15 seconds of
pre-event and post-event context, and a return URL. The range remains editable and is capped at two
minutes. The same action is available from an event card by context-click or a 500 ms touch hold.

## Deadlines and cleanup

Running jobs have a 30-second no-progress deadline and a five-minute total runtime deadline. The
record progresses through catalog and validation, assembly, checksum, and ready phases. Reading or
writing each media sample and each checksum chunk refreshes liveness. Either deadline, an unexpected
worker exit, malformed media, or an I/O failure makes the record failed and retryable.

Cancellation is checked while selecting and writing samples and while computing the checksum. A
cancelled or failed job removes its isolated directory. Cleanup accepts only validated job IDs and
derives every deletion from the configured `.exports` root, so it cannot follow a stored path into
source recordings.

## Access and download integrity

Export commands require an Administrator session. History, lookup, cancellation, retry, and download
are additionally scoped to the credential that created the job; an unrelated requester receives the
same not-found result as an unknown job. Security-sensitive create, cancel, retry, and download
actions use the existing access audit stream.

Before download, KeepPeek recomputes SHA-256 over the exact bytes it is about to send. A mismatch
makes the job failed and retryable, removes the artifact, and returns no media bytes. Download names
are deterministic server-generated basenames and no host path is returned to the client.

The Linux media-integration gate writes the downloaded bytes from the export lifecycle fixture,
inspects the container with `ffprobe`, and decodes its video stream with `ffmpeg -v error`. Run the
same independent check locally with:

```sh
KEEPPEEK_VALIDATE_EXPORT_MEDIA=1 \
  cargo test --locked server::tests::export_job_runs_reports_gaps_and_downloads_verified_file --lib
```
