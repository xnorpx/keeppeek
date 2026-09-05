# Stream and Grid Access

## Contract

Administrators assign camera access to named User credentials. A camera grant covers that camera's
streams, recordings, events, previews, and User-level controls. Export remains Administrator-only.
A grid audience controls access to the grid, never access to a camera. Server authorization is
mandatory even for direct protocol requests.

- Keep permissions inside the existing `access_credentials` section of `config.toml`.
- Keep layout definitions and per-principal active selections inside the existing `peek_layouts` section.
- Preserve `secrets.toml` references and unrelated sections through the shared atomic writer.
- Preserve trusted-local Administrator behavior and existing remote authentication.
- Default new users and absent policies to everything; preserve existing explicit restrictions.
- Store group IDs and camera IDs on the user credential, never lists of users on camera records.
- A restricted user can access cameras in selected configuration namespaces or explicitly selected camera IDs.
- Administrator credentials retain full camera access. Only Administrators can change grants or shared grids.
- Permission changes advance the credential revision and invalidate its existing authenticated work.
- Reject stale revisions, unknown new camera grants, duplicate or malformed IDs, and oversized policies.
- Bound camera grant lists to 128 IDs of at most 256 bytes each. Do not add work to the per-frame path.
- Each saved group list is also bounded to 128 IDs. Group resolution is bounded by the configured
  camera fleet plus explicit IDs; expanded membership is not written back into the user policy.
- Hide unauthorized camera metadata and grid tiles. Empty broad queries must not turn an empty grant set into all cameras.
- Keep the protected `api/` sources unchanged. Use a capability-gated, narrowly scoped existing control envelope.

## Implementation Order

| Slice              | Responsibility                                                                         | Depends on                    |
| ------------------ | -------------------------------------------------------------------------------------- | ----------------------------- |
| Camera policy      | Credential persistence, validation, revision conflicts, and effective grants           | Existing credential lifecycle |
| Server enforcement | Source discovery, subscriptions, stored media/events, controls, and grid projection    | Camera policy                 |
| Administration     | Camera grant editor and assigned-grid workflow using current UI conventions            | Server enforcement            |
| Verification       | Adversarial tests, browser workflow, configuration documentation, and canonical checks | All slices                    |

## Acceptance and Checks

- [x] New User credentials default to everything without a new settings file.
- [x] Per-user group and camera lists combine as a union and retain their selected identities.
- [x] Runtime-added cameras inherit their persisted namespace so existing group grants apply without restart.
- [x] Grants survive reload, preserve other sections, reject stale writes, and invalidate old revisions.
- [x] Unassigned users cannot discover, subscribe to, query, preview, export, or control another camera.
- [x] Assigned grids reveal only allowed cameras, and hidden grids cannot be selected by direct requests.
- [x] Removing camera grants revokes sessions; grid selections revalidate the current audience without changing recording.
- [x] Administrators can manage camera grants through a capability-gated UI; errors preserve drafts.
- [x] Existing credentials and default local access retain their documented compatibility behavior.
- [x] Focused Rust and UI tests prove positive, negative, empty, boundary, reload, and conflict paths.
- [x] Browser tests cover an Administrator and restricted User, including direct-request denial.
- [x] The book reference and access documentation describe the implemented schema and limits.
- [x] Rerun `./check.sh`, Markdown formatting, and `mdbook build book` for the group-access revision.

Focused commands start with `cargo test --locked --lib access::tests::` and expand to the touched
server tests. UI changes use Bun and the repository's existing Vitest/Playwright fixtures. Full
validation runs from the repository root. Do not create a commit or branch without a user request.

## Paper Design Mapping

Design source: [KeepPeek NVR Design System & Spec](https://app.paper.design/file/01M0B0VBH78TMTX40GCYYQ37SG/1-0).
Board 16 now contains the User access state strip, including the default Everything state,
selected groups and cameras, and a 390 px conflict state. The updated form uses these patterns:

- Board 16, Settings: access and roles: People-based entry point, Archivo 18 px/600 headings,
  14 px/18 px body text, 44 px rows, 18 px desktop padding, 645 px panel width, and 6 px corners.
- Board 08, dashboard grids: compact explicit audience/mode choices and separate dashboard sharing.
- Board 27, mobile Administration: 16 px padding, 52 px selection rows, fixed icon lanes,
  and a constrained bottom action area.
- Existing Paper surface, hairline, text, and accent tokens are reused. Font and style values were
  read through Paper JSX and computed-style exports, not estimated from screenshots.

The real-server E2E test checks default-everything access, explicit restriction, a group grant,
revocation, 18 px Archivo headings, 44/52 px rows, no mobile overflow, and desktop/mobile screenshots.
The tracked `ui/design/paper/keeppeek-nvr-v34/user-access.json` records the exported frames,
checksums, capability, policy fields, component, and browser tests. `bun run --cwd ui paper:check`
checks that mapping. The legacy full-page unavailable-capability fixture is historical, not a
claim that current credentials or camera grants are unavailable. No new Loki baseline is approved.

## Current Verification Evidence

The canonical `./check.sh` passed after the per-user group/default-all revision, runtime camera
membership fix, and review regressions:

- Rust: 1,706 passed, with 20 existing skips. Clippy, cargo-machete, and formatting passed.
- UI: 116 browser-component tests and 28 server-compatibility tests passed. Svelte reported no
  errors or warnings; protocol tests cover group grants and malformed policies.
- Playwright: 196 passed, with 2 existing skips. The real User workflow verifies default access,
  explicit restriction, group-based access, grid projection, revocation, and the Paper-derived
  typography and responsive row dimensions.
- The runtime-camera test verifies newly added cameras inherit their configuration namespace
  immediately. Group grants and explicit camera IDs remain separate in the saved user record.
- Paper Board 16 was updated and exported, including corrected policy text and desktop/mobile
  User access states. The source and PNG hashes, coverage report, capability catalog, and
  design-to-test mapping pass the focused integrity gate.
- Group-membership changes cancel affected sessions and their queued event/playback work. Invalid
  management inventories reject saves before policy mutation or session revocation.

The final log is `target/user-access-pr-check.log`. The test run used an isolated 64 GiB sparse APFS
filesystem for `TMPDIR`, two nextest workers, the slow-test opt-in, and backend/frontend ports 4447
and 4304. The protected `api/` contract and the operator's private configuration remain unchanged.

The reproducible performance harness is `ui/scripts/benchmark-camera-access.ts`. Three runs of
100 remote-user queries per path compare base `9829c70` with the candidate release build on the
same M5 Max/macOS/Chromium fixture. Median-of-run p95 latency changed from 1.0 to 2.0 ms for grids,
1.4 to 1.3 ms for timeline/events, 0.6 to 0.5 ms for empty notification inbox, 0.5 to 0.6 ms for empty
history, and 271.3 to 215.2 ms for coverage. Release binary size changed from 29,084,944 to 30,878,384
bytes. This live single-camera smoke workload is not fleet-capacity evidence; recordings accumulate
and coverage request maxima reached 513 ms before and 713.6 ms after. Full spread, environment,
and reproduction details belong in the PR evidence.

## Previous Verification Evidence

The canonical `./check.sh` completed on the working tree based on `9829c70`:

- Rust: 1,702 passed, 20 existing skips; Clippy, cargo-machete, and formatting passed.
- UI: quality checks, component tests, and 196 Playwright tests passed, with 2 existing skips.
- Real-server camera-access E2E before the default-all revision: a new User sees no cameras; direct requests are denied; the
  Administrator grants a camera through the editor; reauthentication exposes only the allowed
  camera and grid tiles; revocation closes access. The dialog stays visible at 320 px and 1440 px.
- Existing secret-reference, notification, storage, and layout tests remain green.
- Markdown checks and `mdbook build book` passed. mdBook reports its existing nonfatal Mermaid
  preprocessor version warning.

The macOS host filesystem is near the storage tests' fixed 85% used threshold. Tests ran with
`TMPDIR` on an isolated 64 GiB sparse APFS volume instead of weakening storage assertions or
changing the operator's configuration. Backend/frontend test ports were 4437 and 4294.

Full command output from that earlier verification is retained locally at
`target/stream-grid-access-check.log`. Playwright screenshots are in its ignored test-results
directory. No real camera settings or secrets were changed.
