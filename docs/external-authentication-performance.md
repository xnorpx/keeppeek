# External authentication performance evidence

## Status

All eleven final benchmark processes passed during the coordinated quiet interval
on 2026-09-27, 18:56:33.288–18:56:40.469 UTC. All three latency budgets and all
capacity/cleanup assertions passed. The runner's before/after build-load checks
passed for every process. The interrupted preliminary sample is excluded.
The final release includes the bearer-only bootstrap transport-policy correction;
its source fingerprint matched before/after rebuilding and after measurement.
This report does not replace the authentication security and browser test results.

## Results

### Historical bearer comparison

Each row contains 10,000 samples after 1,000 warmups. Values are nanoseconds.
Odd pairs ran baseline first; even pairs ran feature first.

| Pair | Baseline p50 | Baseline p95 | Baseline max | Feature p50 | Feature p95 | Feature max |
| ---- | -----------: | -----------: | -----------: | ----------: | ----------: | ----------: |
| 1    |          500 |          600 |        8,700 |         600 |         700 |       4,800 |
| 2    |          500 |          700 |       31,900 |         900 |       1,000 |       9,200 |
| 3    |          500 |          600 |        4,500 |         600 |         600 |       4,400 |
| 4    |          600 |          600 |        6,700 |         600 |         600 |      26,800 |
| 5    |          500 |          600 |       27,900 |         600 |         600 |       7,100 |

Median per-process p95: baseline 600 ns, feature 600 ns; delta 0 ns (0%).
The allowed increase is max(30 ns, 1,000,000 ns) = 1 ms: **pass**.
Per-process p95 ranges were 600–700 ns and 600–1,000 ns respectively.
The median result does not establish a speedup or identical performance for every
run; the table preserves the observed variance and higher feature p50 values.

### Feature authentication and revocation

| Workload                                                | Samples | p50 (ms) | p95 (ms) | Maximum (ms) |
| ------------------------------------------------------- | ------: | -------: | -------: | -----------: |
| Complete OIDC login, warm metadata                      |      30 |   7.2414 |   8.5354 |       9.2290 |
| Direct TLS provider exchange, including fixture signing |      30 |   7.7972 |   8.3061 |       8.3553 |
| Same-build bearer authorization                         |  10,000 |   0.0006 |   0.0007 |       0.0050 |
| Validated-cookie authorization                          |  10,000 |   0.0019 |   0.0019 |       0.0147 |
| Logout to owner/watch cleanup                           |      30 |   0.0277 |   0.1324 |       0.3279 |

Cookie p95 added 0.0012 ms over same-build bearer p95, below the 1 ms budget:
**pass**. Maximum sequential logout cleanup was 0.3279 ms, below 1 s: **pass**.
The direct exchange and complete login are independent sample sets, not additive
components; their percentile differences do not estimate application overhead.
Cold discovery/JWKS took 31.1707 ms in the login fixture and 16.7977 ms in the
separate concurrent-session fixture (one cold sample each).

All 30 logins succeeded and retained 30 authenticated browser sessions across
30 source addresses; per-identity and per-address limits were 64. The workload
had zero key rotations and zero injected outages. These timings do not measure
outage recovery or key-rotation latency.

### Rejected requests, concurrency, and capacity

The 10,000-request rejected-login flood took 32.0314 ms wall time and 32 ms of
whole-process CPU. It returned 30 HTTP 401 responses and 9,970 HTTP 429 responses.
Authenticated sessions stayed at 30. Process RSS changed from 28,127,232 to
28,401,664 bytes. This is the invalid-CSRF/rate-limit path, not 10,000 expensive
cryptographic verification attempts.

| Concurrent limit case   | Barrier workers | Accepted / rejected | Admission wall (ms) | Retained owners / transports | Sampled transient owners / transports | Logout cleanup (ms) | RSS at capacity (bytes) |
| ----------------------- | --------------: | ------------------- | ------------------: | ---------------------------- | ------------------------------------- | ------------------: | ----------------------: |
| Principal 8, address 64 |              16 | 8 / 8               |             28.5533 | 8 / 8                        | 8 / 10                                |              0.4043 |              35,962,880 |
| Principal 64, address 8 |              16 | 8 / 8               |             29.1413 | 8 / 8                        | 8 / 8                                 |              0.0876 |              36,392,960 |

Both cases rejected overflow with HTTP 429 and removed all eight owners, active
transport registry entries, and 64 watches on logout. Maximum concurrent cleanup
was 0.4043 ms, below 1 s: **pass**. The observed transient transport peak of ten is
not a retained-session count; this workload does not demonstrate that transport
allocation itself stays below the accepted-session limit of eight.

The browser registry retained exactly 4,096 entries and rejected overflow. Process
RSS changed from 34,914,304 to 37,322,752 bytes. The cache workload retained four
active providers plus four candidate providers, with eight shared issuer leases,
and rejected overflow; RSS changed from 35,119,104 to 35,762,176 bytes.
The sampled whole-process RSS high-water mark was 37,322,752 bytes (35.594 MiB).
These deltas include process/runtime effects and are not per-entry allocation costs.

### Retained evidence

[Sanitized metric rows](external-authentication-performance.json) preserve all
emitted values and artifact hashes. Raw stdout/stderr for all eleven processes,
the runner, and the executable copies remain in
`E:/src/keeppeek-123-performance-20260927`; final logs are in `final-quiet/`.
The final runner log SHA-256 is
`5E268FE04E522DDAAF48063649CC1498769E2563DDC125283E9F55E35557A2F0`.
The full authentication test took 3.51 s; total measured process interval was
7.181 s. No source changes were made during measurement.

## Workloads and budgets

The release-only, opt-in workloads live in
`src/server/bearer_benchmarks.rs` and `src/server/authentication_benchmarks.rs`.
They assert authorization, capacity, and cleanup outcomes as well as timing them.
All reported percentiles use nearest rank. All clocks are real; the workloads do
not reset rate-limit windows or advance a synthetic clock.

- Historical bearer comparison: five alternating baseline/feature process pairs,
  each with 1,000 warmup calls and 10,000 measured calls to `api_principal`.
  The same single synthetic bearer credential and HTTPS request are used in both
  builds. Compare the median of the five per-process p95 values. The allowed
  increase is the larger of 5% of baseline p95 or 1 ms.
- Validated-cookie authorization: 1,000 warmup calls per method followed by
  10,000 samples per method, interleaving bearer and cookie call order. Cookie p95
  must be within 1 ms of same-build bearer p95. This comparison is separate from
  the historical bearer comparison.
- OIDC login: 30 complete bootstrap/start/callback/session handler flows, with
  actual local provider TLS, signed Ed25519 tokens, PKCE, and identity persistence.
  Discovery/JWKS is measured cold and separately from the login samples. Another
  30 direct provider exchanges measure TLS, fixture signing, and token validation
  without application session issuance. The measured dataset uses one identity,
  30 source addresses, no key rotation, and no injected provider outage.
- Rejected-login flood: 10,000 requests from one address with invalid CSRF and no
  login cookie. Report rejection categories, wall time, process CPU time, and RSS;
  assert that the authenticated session count does not grow.
- Revocation: create 30 real server WebRTC sessions through the create handler,
  attach eight watches to each, and time HTTP logout through synchronous owner,
  active-session registry, and watch cleanup. Maximum cleanup must be at most 1 s.
- Concurrent admission: 16 barrier-started create requests against a limit of
  eight, first by principal and then by source address. Assert eight successes,
  eight quota rejections, eight retained owners/transports, and removal of all
  owners/transports and 64 watches on logout within 1 s. Report sampled transient
  peaks separately from retained counts.
- Component capacity: fill the browser registry to 4,096 entries and the active
  and candidate provider caches to four entries each with eight shared issuer
  leases. Assert overflow rejection and report process RSS around each workload.

## Measurement boundaries

HTTP ingress uses synthetic `rouille` HTTPS requests, not a browser or a listening
HTTP/TLS server. Only provider discovery/JWKS/token exchanges use real localhost
TLS. Provider signing is part of the fixture's measured exchange latency.
These results are not internet identity-provider latency or browser UX timings.

The WebRTC workload performs actual server-side SDP acceptance and creates UDP
session workers. It does not connect remote peers or decode media. Revocation
timing includes synchronous dependent cleanup and registry removal, not the time
until every transport thread has exited. The capacity components do not represent
4,096 simultaneous network connections.

RSS is whole-process resident memory sampled after logins/cache insertions and at
phase boundaries, not an allocator peak. The concurrent registry monitor requests
1 ms intervals and takes separate registry snapshots; it can miss shorter peaks.
Flood CPU is the operating system's cumulative whole-process CPU counter at
millisecond resolution. No CPU affinity or power-plan changes are applied.

## Environment and baseline

Reference host: Windows 11 Pro 10.0.26200, AMD Ryzen 5 5600G, six physical cores,
12 logical processors, 16,087,244 KiB visible RAM, Balanced power plan. Compiler:
Rust 1.98.1 (`48a229cea`, LLVM 22.1.8), `x86_64-pc-windows-msvc`. Both builds use
the repository release profile and locked dependencies. Browser version is not
applicable to these handler/provider workloads.

Baseline main commit: `26ef5225c368c9c5440a6d1cd0187a90fbc10456`.
Its isolated worktree contains only the bearer harness and its test-module
declaration in addition to that commit. The baseline executable SHA-256 is
`39B78465A40F15495273A27AC1EE34A5F12B69DC35301A0300970B682BF03738`.

The compiled baseline harness SHA-256 is
`D4EE491C0E1087F58106F54D2B54A6CCED55B30AC720D82A52464FD2829A5DF7`.
The final source in both worktrees has SHA-256
`46140C5FF36F0DDEB52A314902E9DED06FAC109A6224861D0282B9E2FE8161C1`.
The sole difference is `!cfg!(debug_assertions)` becoming
`!black_box(cfg!(debug_assertions))` in the one-time release-mode assertion, before
setup, warmup, and timed samples. Reversing only that replacement reproduces the
compiled baseline harness hash exactly. The measured code and input are identical.

The feature release includes conditional stale-session-cookie cleanup,
the bearer-only bootstrap transport-policy correction, the OIDC TLS fixture
migration, and the benchmark page-size correction. Its
executable SHA-256 is
`C4AD216B8F4C1EDBCC3DA7FB290EA6057D3384192BDEFB51E10B3D0DFFCEDB6C`.
The build completed in 5m40s. The 803-file
source fingerprint was identical immediately before and after compilation:
`F830FED5EBD1868DAE664DCE534F60F6571524638A33B682950A8804C9010F93`.
This identifies the uncommitted issue123 build on the baseline commit above.

After measurement, CI required multiline formatting for the `jsonwebtoken` and
`rustls` feature arrays in `Cargo.toml`. Parsing the manifest before and after
that whitespace-only change produced identical values. Subsequent CI corrections
change only the crash-recovery test oracle and run the authentication fixture
build in a separate parallel job. Production Rust sources, benchmark harnesses,
dependency versions, features, and the lockfile are unchanged. The measured
executable is therefore an identified equivalent build for runtime measurements;
the final source-tree fingerprint differs because of formatting and test changes.

The fingerprint is SHA-256 of LF-joined, sorted, unique git-visible paths ending
in `.rs`, `.proto`, or `.toml`, plus `Cargo.lock`; each line contains the relative
path, one space, and the file's uppercase SHA-256. It covers tracked and untracked
non-ignored source. The executable hash additionally identifies the built artifact.

## Reproduction

Build each worktree separately, then run the emitted executables directly so no
compilation overlaps measurement:

```powershell
cargo test --locked --release --lib --target-dir <isolated-target> issue123_ --no-run
& <baseline-test-executable> issue123_bearer_comparison --ignored --nocapture --test-threads=1
& <feature-test-executable> issue123_bearer_comparison --ignored --nocapture --test-threads=1
& <feature-test-executable> issue123_authentication_benchmark --ignored --nocapture --test-threads=1
```

Repeat the bearer commands for five pairs, alternating which executable runs
first. Set `KEEPPEEK_BENCH_BUILD` and `KEEPPEEK_BENCH_RUN` to identify each row.
The tests emit sanitized JSON prefixed by `ISSUE123_BEARER` or `ISSUE123_BENCH`.
Retain p50, p95, maximum, sample count, executable hash, and all failure output.

Coordinate the quiet interval with all host users. The reference runner rejects
active Cargo, rustc, Clippy, or nextest processes before and after each invocation,
refuses to overwrite logs, and bounds each benchmark process to 180 seconds.
Background OS activity is not disabled, so repetition and disclosed run variance
remain necessary even when these checks pass.

## Attempt history

A preliminary run completed one baseline process, but an unrelated release build
started during the interval. The post-run load check stopped the runner. That
sample is excluded in its entirety, rather than selected based on its latency.
Its stdout/stderr and runner output are retained separately. No feature or full
authentication result from that attempt is claimed.
