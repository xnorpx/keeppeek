# Real Home Assistant Verification

## Approved Scope

Add a Linux Docker CI test of the existing KeepPeek card in actual Home Assistant Lovelace.
Use the current locally built module; do not require a published release, HACS authorization,
production cameras, repository secrets, or changes to motion/ISAPI/server implementation.
The user approved the container, disposable onboarding, real dashboard/editor, and artifact plan.

## Test Boundary

Run official Home Assistant `2026.9.1` pinned to its multi-architecture image digest. Publish only
its HTTP port on host loopback. Keep KeepPeek, the synthetic RTSP sources, and Playwright on the
runner host so WebRTC media does not cross Docker NAT. Home Assistant serves the module from its
temporary `www/` directory and resolves a YAML dashboard. Use the supported onboarding/login UI
with a fixture-only account. Do not seed internal authentication storage or bypass authorization.

Use a fresh configuration per test, one worker, bounded Docker commands and startup polling,
two CPUs, 2 GiB memory, and no privileged/device access. Teardown stops only owned containers and
processes. Configuration uses a labeled Docker-managed volume seeded with `docker cp`, so Linux
cleanup does not encounter root-owned files in the host checkout. Never upload Home Assistant
storage, traces containing credentials, or raw configuration.
Upload screenshots, sanitized logs, and test results. Keep the normal Docker-free test command
unchanged and add an explicitly required Ubuntu CI job to the existing UI gate.

## Increments

- [x] Prove the built module displays real decoded frames inside Home Assistant after onboarding.
- [x] Verify multiple cards, the visual editor, navigation/cleanup, themes, and mobile layouts.
- [x] Add the pinned-image Ubuntu CI job and explicit local command with cleanup/evidence.
- [x] Run the container test locally, narrow checks, and canonical `./check.sh`.

## Verification

From the root: `bun run --cwd ui test:home-assistant-container` prepares test artifacts, pulls the
pinned image, and runs the suite. `bun run --cwd ui test:home-assistant-container:run` reuses the
prepared binaries and image. Finish with `./check.sh` from the root. Docker, Bun, Rust, and
Playwright Chromium must already be installed. Docker Desktop on macOS is a local dashboard-test experiment,
not Home Assistant's supported production installation. Linux CI remains the reference platform.

## Observed Results

- The real onboarding flow creates the temporary user without internal authentication seeding.
- YAML `!secret` and `/local/keeppeek.js` load through actual Home Assistant.
- Two real 640x360 H.264 streams decode at desktop and mobile sizes. Assertions require a fresh
  video-frame callback from the current live stream and no waiting overlay, not stale frame counts.
- Three cards share one active connection and media subscription. A forced disconnect rebuilds
  only one shared subscription; leaving the view returns the active-session gauge to zero.
- Theme changes retain the peer. Responsive Lovelace column replacement permits a clean
  reacquisition, as confirmed by the owning upstream layout implementation.
- The native Home Assistant visual-editor dialog discovers sources, keeps the key redacted,
  and persists edits that survive a full page load.
- Six repeated fresh-container runtime scenarios passed before the Linux storage refinement;
  both runtime scenarios passed again using Docker-managed configuration volumes.
- Logs, TypeScript, formatting, lint, function-size, and CI dependency wiring checks pass.
- The final container suite passed all three tests with zero failures or skips in 30.1 seconds
  on macOS ARM64, Docker Desktop, and the pinned Home Assistant 2026.9.1 image. Two scenarios boot
  real containers; the third verifies credential redaction without requiring one.
- The complete `test:home-assistant-container` preparation command also passed locally.
- The final `./check.sh` passed, including 204 regular Playwright tests and two existing codec
  skips. Its success marker was `KEEPPEEK_HOME_ASSISTANT_CONTAINER_FINAL_OK`.
- No test-owned containers or volumes remain after verification. The only subsequent changes
  are this evidence record; no production runtime code was changed for the container test.

Final evidence is in `ui/test-results/home-assistant-container.xml` and the corresponding
`ui/test-results/home-assistant-container/` screenshot and sanitized-log directories. The immutable
image index is `sha256:612d76760b544cb40b7ba01387fdac964c59a6a550a50a4d30b4773c822d2918`.
Desktop, mobile dark-theme, and real visual-editor screenshots were inspected. GitHub CI uses
the Linux AMD64 variant of this same index; that hosted run still awaits the normal push/PR workflow.

Home Assistant can emit one exact upstream `Connection lost` rejection while closing its temporary
onboarding connection. The suite records that setup-only result separately and still rejects all
card/editor errors. This is not a blanket console-error exclusion. HACS distribution remains
unverified, and no hosted CI execution is claimed from local checks.

## Remaining Release Work

HACS download, upgrade, and rollback against a published release remain separate. This test proves
the actual Home Assistant runtime can load the current build without a release, not HACS delivery.
