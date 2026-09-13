# Release readiness and known limitations

KeepPeek is in Alpha qualification. POC and MVP gates are complete; Alpha implementation issue
closure does not establish the early-adopter matrix or a feature freeze. Passing automated tests
is necessary, but it is not enough to call a camera recorder production-ready. The promotion
decision belongs to the tested release build and representative deployment recorded in the active
[Alpha gate #145](https://github.com/xnorpx/keeppeek/issues/145). The
[roadmap](https://github.com/xnorpx/keeppeek/issues/147) describes the subsequent release phases.

## What automated validation proves

The repository gate builds the Rust workspace and browser application, runs Rust, TypeScript,
Svelte, browser, real-media, and Playwright tests, checks formatting and dependencies, and validates
the versioned Paper scenario and visual-harness manifests.

Focused benchmarks cover latency and memory budgets for recording coverage, operational events,
notifications, MQTT enqueue, storage, and event lookup. Some are ignored tests or standalone Cargo
benchmarks and require separate explicit runs; a green ordinary check or CI run does not establish
their results. Record the exact benchmark command, build, workload, host, and measurements with the
qualification evidence.

Those checks prove deterministic contracts and regression fixtures. They do not prove that a
particular camera firmware, network, disk, browser, reverse proxy, notification account, or broker
will behave correctly under sustained load.

Paper reference integrity tests check the exported design bundle, scenario identities, assets,
dimensions, hashes, and shared tokens. They do not by themselves compare every production page
with every Paper artboard. Story-based visual checks and route-level browser checks cover their
declared fixtures. Treat the accepted mobile Keep proposal as a design reference and verify the
implemented workflow on the target device; a reference file is not deployment evidence.

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
- monitor memory, recording databases, thumbnails, export jobs, the in-memory notification outbox, MQTT outbox, logs,
  threads or tasks, file descriptors, sessions, and browser object URLs for bounded growth;
- restart and verify that notification runtime state, the MQTT outbox and deduplication history,
  access audit/activity, and active sessions reset; credentials and grants, durable operational
  events, export jobs, and recording catalog state recover according to their documented contracts;
- rerun the complete workflow without manual database or recording-file edits.

Stop qualification on any silent recording-loss path, remote authentication bypass, secret leak,
indefinitely running job, or open release-blocking defect.

### MQTT restart rehearsal

Use a disposable broker and synthetic event on an isolated installation. Interrupt broker access,
publish the event, and observe the pending count before restarting KeepPeek. After restart,
verify that the pending outbox is empty. Restore broker access and confirm that the old pending
publication is not replayed. Emit a new event and verify delivery and consumer deduplication by
`(instance_id, event_id, revision)`. Confirm that the original durable event is still available
in KeepPeek and that recording continued independently. QoS does not make this outbox durable;
downstream recovery must not assume replay across a KeepPeek restart.

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

KeepPeek provides direct ZIP export and validated application of `config.toml` and plaintext
`secrets.toml`, with restart activation and automatic startup recovery. `recordings.db`, recording MP4s, and thumbnail
JPEGs remain a separate archive responsibility. A recovery rehearsal must test both the sensitive
KeepPeek configuration bundle and the recording archive. See
[Backup and restore](./backup-and-restore.md) and the
[recording archive recovery procedure](./recording-archive-recovery.md). There is no public importer
for an unrelated recording archive, no general catalog downgrade command, and no automatic adoption
of unknown MP4s. A validated index rebuild applies only to eligible existing catalog recordings.

### Restart resets runtime work

Notification inbox/history, retry work and delivery counters; MQTT pending publications and
deduplication; and access sessions, audit history and last-use activity are not durable. Restart
does not replay those queues. Capture required operational evidence before restarting and verify
the intended behavior afterward. Durable recording events and settings have separate recovery
contracts; see [What survives a restart](./backup-and-restore.md#what-survives-a-restart).

### Maintenance has filesystem and recovery boundaries

Manual maintenance is destructive and uses confirmed whole-recording selections. Native Windows
mutation targets NTFS with persistent ACLs; ReFS is rejected. Structural container inspection does
not certify complete media decodability or immutable content. Unknown files are not adopted or
deleted automatically. Review [Recording maintenance](./recording-maintenance.md) for cooperative
deadlines, same-account interference limits, interrupted-work handling, and unqualified scenarios.

### Access roles are intentionally fixed

Remote access supports Administrator and User; custom roles are not available. Administrators can
restrict a User credential to selected camera groups and individual cameras in **Settings → Access
& roles → User access**. The server enforces these grants for media, events, coverage, and camera
controls. Dashboard audiences remain separate and do not grant camera access. Saving a grant
change closes the affected User's existing sessions; stale saves retain the draft and report a
conflict. The default grant includes all current and future cameras.

Trusted-local clients remain Administrators. Exclude an address from `access.local_networks` and
require a User credential when that client needs camera restrictions. Hidden navigation is not
an authorization boundary. See [Camera and dashboard access](./authentication.md#camera-and-dashboard-access).

An access change during dashboard initialization can leave an expired-session error visible until
the page is reloaded. The invalid session remains denied by the server. Follow
[the reconnect steps](./authentication.md#session-expired-after-a-permission-change) and verify
the new grants after sign-in; this does not establish that the UI race has been resolved.

### Detection remains external

KeepPeek records and presents camera-native or externally published events. It does not own object,
face, or license-plate detection, model management, zones, masks, or training. Recording and health
must remain useful when an external analysis service is absent.

## Promotion record

The owner records the exact release commit, platform and browser versions, camera and codec matrix,
network topology, storage configuration, test commands, soak duration, observed limits, known
workarounds, and a promote or reject decision. Unverified items stay explicit; a green build never
silently checks a deployment-specific criterion.

Use a record with the following concrete results:

| Evidence                  | Record                                                                                                                           |
| ------------------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| Build and deployment      | Exact version/commit or image digest, host architecture, filesystem, service identity, browser versions, and proxy/VPN topology. |
| Camera matrix             | Camera model and firmware, transport, main/sub codecs, finalized recording and independent decode results.                       |
| Access                    | Allowed and denied camera checks, dashboard audiences, expired/revoked sessions, and local versus remote classification.         |
| Sustained operation       | Start/end times, workload, recording gaps and explanations, memory/storage growth, cleanup and reconnect results.                |
| Recovery                  | Configuration ZIP apply, stopped catalog/media copy, restart, upgrade rehearsal, and recovered/missing intervals.                |
| Integrations and evidence | Provider/broker outage and restart behavior, completed/failed/cancelled exports, and verified downloaded bytes.                  |
| Decision                  | Remaining issues and workarounds, evidence location, and an explicit promote or reject decision.                                 |

Use [Upgrades and migrations](./upgrades-and-migrations.md) to plan a version change, and
[Reporting bugs](./reporting-bugs.md) for reproducible failures. This guide does not close the Alpha
gate, its feature-freeze decision, or the installation's long-running qualification work.
