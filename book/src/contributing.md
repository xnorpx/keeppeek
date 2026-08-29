# Contributing

KeepPeek welcomes focused contributions that strengthen recording, camera compatibility, media
routing, observability, the public API, and the first-party experience. A contribution should make
the supported product more dependable without silently turning the core into a different product.

Read [Users and design choices](./users-and-design-choices.md) before proposing a substantial
change. It describes the users, boundaries, and trade-offs that guide review.

Use [Reporting bugs](./reporting-bugs.md) for a reproducible defect. Use GitHub Discussions for
setup help, troubleshooting, usage questions, and feature proposals.

## Guard the product boundary

The feature set is deliberately and heavily guarded. Any proposal that expands product scope must
be discussed and reviewed before implementation begins. A complete implementation is not, by
itself, a reason to add a feature to the core.

Feature review asks:

1. Does this belong in a focused recorder and media gateway?
2. Can an independent service or protocol plugin provide it through the public API instead?
3. Which documented users need it, and does it make another user's core workflow worse?
4. What permanent maintenance, security, storage, resource, compatibility, and licensing costs
   does it add?
5. Can the behavior be tested without depending on one private device, account, or cloud service?

Open a [GitHub Discussion](https://github.com/xnorpx/keeppeek/discussions) before writing a feature
that changes the product boundary, public protocol, storage model, supported platform contract, or
long-term maintenance surface. Expect alternatives and non-goals to be reviewed as seriously as
the proposed implementation. This review is how KeepPeek avoids accidental scope creep.

## Contributions that fit well

Focused bug fixes, tests, documentation corrections, diagnostics, and interoperability improvements
usually have a clear path when they preserve the existing product boundary.

### Camera protocols and RTSP interoperability

Camera protocol fixes and bounded RTSP workarounds are especially welcome. They are often easier to
submit and review than feature proposals because the expected behavior can be captured in a fixture
and regression test.

A useful camera contribution includes:

- manufacturer, model, firmware version, backend, and TCP or UDP transport;
- the smallest reproducible failure with credentials, addresses, and private media removed;
- sanitized protocol, SDP, or packet evidence when it can be shared legally and safely;
- a regression fixture or deterministic fake-camera case;
- a narrow parser, transport, or adapter fix instead of a product-wide brand check;
- successful main/sub frame, keyframe, MP4 finalization, and independent decode validation when the
  change affects recording.

Never submit camera passwords, access keys, private stream URLs, identifiable recordings, or packet
captures containing secrets. If private evidence cannot be published, file the bug with only the
safe evidence and explain what is unavailable rather than attempting a speculative workaround.

One camera quirk should not weaken validation for every camera. Prefer a protocol-level correction
when the device is standards-compliant, and a bounded, evidence-backed compatibility rule when it
is not.

### Public API and external services

API contributions should preserve typed messages, stable identity, explicit capabilities, bounded
queues, and clear failure behavior. An example client or service should consume the canonical API
definitions rather than copy handwritten wire types or depend on private server internals.

New detector, transcoder, automation, and commercial-service ideas generally belong outside the
core first. A small reference implementation may demonstrate interoperability without becoming a
promise that KeepPeek will own that service as a product.

## AI-assisted contributions

AI is one of the tools used to develop KeepPeek. This section does not take a position on AI in
general; it defines only how AI-assisted contributions to this repository are reviewed.

AI-assisted contributions are acceptable. The standard is the same as for code written without an
assistant, and responsibility never transfers to the model.

The person submitting the pull request is its first reviewer. Before requesting another review,
they must:

- inspect every changed line, including generated tests and configuration;
- understand and be able to explain every line in the pull request;
- verify API, security, resource, failure, and compatibility assumptions against the repository;
- remove invented behavior, unnecessary abstractions, unrelated cleanup, and generated commentary;
- run the required validation and investigate failures rather than asking reviewers to debug raw
  model output.

"The AI wrote it" is not an explanation for a design decision and is not a reason to merge code the
submitter cannot defend. Reviewers may ask the submitter to explain control flow, ownership,
protocol behavior, tests, or individual lines. If they cannot, the contribution is not ready.

## Pull request shape

Use the repository's
[pull request template](https://github.com/xnorpx/keeppeek/blob/master/.github/pull_request_template.md)
as the canonical shape for every pull request. Complete its summary, linked Discussion, acceptance
criteria verification, performance evidence, validation, and review evidence before requesting
review. A bug-fix pull request may link its reproducible bug issue instead of a feature Discussion.

Keep pull requests small enough to review as one coherent behavioral change. State the supported
user and boundary affected, include focused regression coverage, update public documentation when a
contract changes, and leave unrelated formatting, dependency updates, and refactors out.

Use draft pull requests for early feedback when the boundary is clear but the implementation is not
ready. Do not use a large completed pull request as the first request for product-scope review.

## Review cadence

Pull request reviews are done on Sundays. If a Sunday review window passes and a pull request is
missed, leave a short, polite ping on that pull request to remind the maintainer. A reminder on the
existing pull request is enough; do not open a second Discussion or duplicate pull request.

## Development environment

Repository development uses stable Rust, Bun `1.4.0`, and Python `3.12`. The pinned Bun and Python
versions live in `ui/.bun-version` and `examples/object_detection_service/.python-version`.

Formatting and validation require the Python packages in
`examples/object_detection_service/requirements.txt`, including Black. The root `fix` and `check`
scripts resolve Python in this order:

1. `KEEPPEEK_PYTHON`, when set;
2. `python3.12`;
3. `python3`.

Do not use a Python virtual environment. `fix.sh` and `fix.bat` install the requirements into the
resolved interpreter, retrying with `--break-system-packages` when that interpreter is externally
managed.

The canonical checks also require Bun, `cargo-nextest`, and `cargo-machete`. Install UI dependencies
with Bun from the public npm registry and do not create or commit an npm, pnpm, or Yarn lockfile.

## Validation

Run the canonical repository check from the repository root before requesting review:

- macOS or Linux: `./check.sh`
- Windows: `.\check.bat`

Run the same gate with opt-in slow Rust test bodies before submitting a pull request:

- macOS or Linux: `KEEPPEEK_RUN_SLOW_TESTS=1 ./check.sh`
- Windows PowerShell: `$env:KEEPPEEK_RUN_SLOW_TESTS = '1'; .\check.bat`

Pull-request CI keeps feedback fast by splitting static UI checks, unit tests, coverage, and Linux
Playwright tests into non-overlapping jobs. The extended `Main` workflow runs after every merge and
once per hour on `main`; it owns the opt-in slow Rust tests, ARM builds and tests, Windows installer
lifecycles, Rust coverage and CodeQL, and full Playwright suites on macOS and Windows.

During development, run the narrowest relevant test first. The final check covers repository
formatting, Rust tests and lints, frontend checks, and the supported test suites. Document any test
that cannot run in the pull request and explain why.

Camera changes should also be validated against the affected device and independent MP4 tooling.
Passing a unit test does not prove that a real camera authenticates, advances frames and keyframes,
finalizes recordings, and produces decodable output.

## Licensing

See [Open source and licensing](./open-source-and-licensing.md#licensing-model) for the project-wide
license model. Contributions are accepted under the license of the area they modify. Submit only
work you have the right to provide under that license, including fixtures, generated files, and
AI-assisted output.
