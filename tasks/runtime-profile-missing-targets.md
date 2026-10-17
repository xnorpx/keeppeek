# Missing runtime profile targets

Continues #202 from #147, based on main `5540d7c` after merged #304.

## Contract

An active profile's saved target that is absent from configured camera definitions remains visible
as degraded with `configured camera is missing`. Startup, loop completion and live camera removal
must agree with the existing server projection. Re-adding the camera replaces this placeholder;
activating Base or removing the target clears it. Camera deletion remains allowed. This does not
define stable-ID migration or change the protected API, schemas, configuration or activation paths.

Server fallback reads persisted camera settings so a retained discovery-catalog entry cannot hide
deletion. This adds one configuration/secrets load per projected activation result. On load failure,
the server warns and falls back to its catalog; deletion knowledge is then unqualified. This is
control-plane work, not per-frame media work. Complete server activation latency remains unqualified.

Use one shared missing-target projection, bounded by the active profile's 127 targets. Reconcile
once per startup/completion batch and camera removal, preserving the revision fence and existing
watch authorization. Existing configured-camera results take precedence. Measure the disabled-camera
fixture before/after; missing-target iteration must not enter per-frame media work.

## Verification

- First reproduce missing startup/completion/removal observations with focused Rust tests.
- Cover Base, inactive profiles, re-addition, supersession and the 127-target boundary.
- Run profile/watch tests, all-target Clippy, Rust/Markdown formatting and book checks.
- Review independently and publish the PR; do not poll hosted CI.
- Preserve all original #202 acceptance criteria and leave unqualified criteria open.

## Evidence

Windows 11 Pro x64, AMD Ryzen 5 5600G (6 cores / 12 threads), Rust 1.99.0,
debug build, `CARGO_INCREMENTAL=0`, 2026-10-17:

- Both startup and removal regressions failed before implementation: the saved target disappeared.
- `cargo test --locked -p keeppeek --lib profile -- --nocapture --test-threads=1`: 63 passed.
- The same compiled native test executable with `server::state_store_watch::tests::`: 16 passed.
- `cargo clippy --locked --all --all-targets -- -D warnings` and `cargo fmt --all --check`: passed.
- Markdown formatting, book build, whitespace and protected-API checks passed. The book emits the
  existing mermaid preprocessor version warning (0.5.0 versus mdbook 0.5.4).
- Independent review found no remaining blocker after correcting stale catalog precedence,
  the valid 127-target fixture and revision fences.

The isolated `runtime_profile_disabled_camera_scale` fixture runs 30 sequential transitions for
127 disabled cameras. Before/after median: 971 / 1,106 us (+135 us). Before/after p95:
1,052 / 1,146 us (+94 us). The baseline executable was also confirmed to still fail the startup
regression. These measurements exclude server configuration loading, storage, transport and browsers;
they do not qualify the original release-build budget or live-camera convergence.

Full canonical, slow post-merge, hosted CI and original feature acceptance are not claimed here.
Rollback is a code revert; no persisted format changes.
