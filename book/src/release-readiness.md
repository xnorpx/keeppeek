# Release readiness and known limitations

KeepPeek has completed its proof-of-concept gate and is undergoing MVP qualification. Passing
automated tests is necessary, but it is not enough to call a camera recorder production-ready. The
MVP decision belongs to the tested release build and representative deployment recorded in
[issue #144](https://github.com/xnorpx/keeppeek/issues/144).

## What automated validation proves

The repository gate builds the Rust workspace and browser application, runs Rust, TypeScript,
Svelte, browser, real-media, and Playwright tests, checks formatting and dependencies, and validates
the versioned Paper scenario and visual-harness manifests. Focused benchmarks enforce latency and
memory budgets for recording coverage, operational events, notifications, MQTT enqueue, storage,
and event lookup.

Those checks prove deterministic contracts and regression fixtures. They do not prove that a
particular camera firmware, network, disk, browser, reverse proxy, notification account, or broker
will behave correctly under sustained load.

## Release and dependency integrity

`Cargo.lock` is a tracked release input. Direct Cargo requirements name a compatible version, and
repository build, test, benchmark, container, and release commands use `--locked`. CI runs
`cargo audit --deny warnings` without advisory exceptions.

The UI and visual harness intentionally do not track JavaScript lockfiles. Every direct Bun
dependency uses an exact version, while transitive dependencies resolve from the public npm
registry on each clean install. CI creates a separate temporary lockfile for each manifest with
dependency scripts disabled, runs `bun audit`, and rejects high or critical advisories. This policy
detects current ecosystem breakage but does not make JavaScript transitive resolution reproducible
between runs.

The object-detection example intentionally tests the newest compatible Python packages. CI uses
Python 3.12 and resolves `requirements.txt` through `pip-audit` without installing the full model
runtime in the audit job.

The embedded CCTV Camera Database comes from the v2.8.0 release archive. The build verifies its
SHA-256 digest before opening the ZIP. Container builds use Bun 1.4.0 and immutable base-image
digests, then CI starts the production image and probes its HTTP listener. A version tag can create
release artifacts only when it points to a commit on `main` and that exact commit has a successful
push CI run.

## Representative deployment matrix

Before promotion, record exact evidence for the deployment that will rely on KeepPeek:

- at least one direct RTSP or ONVIF camera and the established native camera path;
- main and sub streams, including browser-compatible and incompatible codec profiles;
- desktop and mobile browser workflows;
- local access and remote access through the intended VPN or reverse proxy;
- healthy, stale, reconnecting, recording-failed, storage-pressure, and recovered states;
- events with and without images, revised events, and a dense event day;
- Pushover success, failure, and retry plus MQTT success, outage, and recovery;
- normal, partial, failed, cancelled, retried, and independently decoded exports.

This matrix documents tested configurations. It is not a universal camera, browser, broker, or
provider support claim.

## Continuous recording soak

Run the final release build long enough to cross normal segment finalization, catalog maintenance,
camera reconnect, provider retry, and storage cleanup cycles. During the soak:

- account for every unexpected recording gap with camera, writer, storage, or catalog evidence;
- verify primary recorded playback remains at source rate while timelines refresh and exports run;
- monitor memory, database, thumbnails, export jobs, notification outbox, MQTT outbox, logs,
  threads or tasks, file descriptors, sessions, and browser object URLs for bounded growth;
- restart and verify open operational events, cooldowns, outboxes, export jobs, coverage summaries,
  and sessions recover according to their documented contracts;
- rerun the complete workflow without manual database or recording-file edits.

Stop qualification on any silent recording-loss path, remote authentication bypass, secret leak,
indefinitely running job, or open release-blocking defect.

## Current limitations

### Camera support is evidence-based

Discovery does not prove authentication, stable media, keyframes, or valid recordings. Validate
each requested stream through a finalized MP4 and independent decode before relying on it. Prefer
`reo-proto` over TCP for a Reolink camera only after that path passes; use RTSP over TCP for a
generic ONVIF camera unless measured evidence supports another transport.

### Browser codec support varies

KeepPeek stores camera media without re-encoding it. A browser may therefore record or index a main
stream that it cannot decode. Keep a broadly compatible H.264 substream for live view and review,
and treat a truthful incompatibility message as different from missing footage. Automatic
transcoding remains separate work.

### Configuration bundles do not archive recording media

KeepPeek provides validated reference-only configuration and metadata backups, dry-run restore,
staged activation, and bounded rollback. Recording MP4s and thumbnail JPEGs remain a separate
archive responsibility. A recovery rehearsal must test both the KeepPeek bundle and the mapped
media archive. See [Backup and restore](./backup-and-restore.md).

### Access roles are intentionally fixed

Remote access supports Administrator and User. Custom roles and per-camera permissions are not
available. Use network separation or separate servers when a person must not see every configured
camera; hiding a control in the browser is not an authorization boundary.

### Detection remains external

KeepPeek records and presents camera-native or externally published events. It does not own object,
face, or license-plate detection, model management, zones, masks, or training. Recording and health
must remain useful when an external analysis service is absent.

## Promotion record

The owner records the exact release commit, platform and browser versions, camera and codec matrix,
network topology, storage configuration, test commands, soak duration, observed limits, known
workarounds, and a promote or reject decision. Unverified items stay explicit; a green build never
silently checks a deployment-specific criterion.
