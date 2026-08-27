# Reporting bugs

GitHub Issues are for reproducible KeepPeek defects. The
[bug report form](https://github.com/xnorpx/keeppeek/issues/new?template=bug.yml) is the canonical
submission path, and
[`.github/ISSUE_TEMPLATE/bug.yml`](https://github.com/xnorpx/keeppeek/blob/master/.github/ISSUE_TEMPLATE/bug.yml)
is the source of truth for its required fields.

## Choose the right channel

| What you need                                  | Where to go                                                                           |
| ---------------------------------------------- | ------------------------------------------------------------------------------------- |
| Existing KeepPeek behavior reproducibly fails  | [Bug report](https://github.com/xnorpx/keeppeek/issues/new?template=bug.yml)          |
| Setup help, troubleshooting, or usage guidance | [Q&A](https://github.com/xnorpx/keeppeek/discussions/categories/q-a)                  |
| New behavior or a feature proposal             | [Suggestions](https://github.com/xnorpx/keeppeek/discussions/categories/suggestions)  |
| A security vulnerability                       | [Private security report](https://github.com/xnorpx/keeppeek/security/advisories/new) |

A support request is not yet a bug until the evidence shows existing KeepPeek behavior violating
its contract. Feature requests belong in Suggestions even when they would solve a real problem.
Search open and closed Issues before filing so evidence and discussion stay on one report.

## Before submitting

Confirm all of the following:

- The same defect is not already present in an open or closed Issue.
- The report describes a reproducible KeepPeek defect, not setup help or a feature proposal.
- The defect reproduces on the current release, or the exact affected build is known.
- Relevant Health, camera diagnosis, server log, and browser console evidence has been checked.
- Credentials, private addresses, tokens, cookies, private media, and sensitive data are removed.
- A KeepPeek diagnostics package is attached, or the report explains why it is unavailable.

Reports that are duplicates, support requests, feature proposals, or missing the required
reproduction and environment details may be closed or marked `needs more info`.

## Describe one observable defect

Keep one report focused on one failure. State what fails, when it fails, and the effect on
recording, review, evidence, security, or operation. Include the affected camera, stream, time
range, or workflow only in sanitized form.

Select the primary affected area in the form:

- Recording and ingest
- Peek and live video
- Events, search, and previews
- Keep, timeline, and recorded playback
- Export
- Cameras, discovery, and configuration
- Camera controls and vendor integration
- Storage, retention, backup, and restore
- Authentication, authorization, and privacy
- Installation, upgrade, and service lifecycle
- API, WebRTC, and external integrations
- UI, mobile, and accessibility
- Other

If several areas are involved, select the user-visible workflow that fails first and describe the
secondary effects in the report.

## Provide a deterministic reproduction

Start from a known state and give the smallest sequence that reproduces the defect:

```text
Starting state: <configuration, page, camera state, or recording state>
1. Configure or open ...
2. Select ...
3. Perform ...
4. Observe ...

Frequency: <always, intermittent, or N of M attempts>
```

Separate expected and actual behavior. Make the difference observable through a state, response,
error, file, timing, resource measurement, or visible result. "It does not work" is not enough to
reproduce or verify a fix.

## Record regression and impact

State whether the behavior previously worked. Include the last known good and first known bad
versions when known, how frequently the defect occurs, its operational or data-loss risk, and any
verified workaround.

Use `unknown` when a relevant fact cannot be established. Do not guess a regression version or
claim data loss without evidence.

## Describe the environment

Include every field relevant to reproduction, using `N/A` only when it genuinely does not apply:

- KeepPeek version, commit, and build profile
- operating system and architecture
- installation method: native package, Docker, service, or other
- browser or client and version
- camera make, model, and anonymized firmware version
- camera source: Reolink, ONVIF, RTSP, `test-camera`, or other
- stream codec, resolution, frame rate, and audio
- camera count and relevant catalog or date-range scale
- storage type and free space

Hardware-specific and intermittent bugs are valid, but the report must distinguish direct evidence
from a suspected cause and provide a bounded way to collect the next observation.

## Attach sanitized evidence

After reproducing the defect, open **Settings > Logs & diagnostics > Download diagnostics**. Attach
the generated `keeppeek-diagnostics-*.json.gz` file under **Diagnostics package and sanitized
evidence** so the package contains the relevant retained server and browser logs.

If the package cannot be generated or attached, state the exact reason. Missing diagnostics do not
block submission, but a report without enough evidence may be closed as `needs more info`.

Add focused timestamps, screenshots, stack traces, protocol fixtures, or small log excerpts when
they make the defect easier to reproduce. Never publish:

- credentials, access keys, tokens, cookies, or authorization headers;
- camera IP addresses, hostnames, private filesystem prefixes, or stream and snapshot URLs;
- faces, license plates, private recordings, notification text, or location details;
- unsanitized logs, screenshots, packet captures, or diagnostics bundles.

Redact at the source. Do not attach evidence that cannot be confidently sanitized.

## Define fix acceptance criteria

Describe the observable behavior that will make the bug fixed. Keep each criterion unchecked and
name the test or procedure that will verify it:

```markdown
- [ ] AC-1: <the reported reproduction produces the correct observable outcome>
  - Expected outcome: <exact state or result>
  - Verification: <regression test, command, or reproducible procedure>

- [ ] AC-2: <the relevant failure or boundary case behaves correctly>
  - Expected outcome: <exact state or result>
  - Verification: <test name, command, or reproducible procedure>
```

Acceptance criteria should describe corrected behavior, not prescribe an unverified implementation.

## Include performance evidence

When the defect affects runtime, build time, memory, storage, network traffic, or interaction
latency, include:

- metric and unit;
- current or before value;
- expected budget or target;
- workload or dataset;
- hardware, operating system, browser, and build profile where relevant;
- exact command or harness;
- run count and statistic, such as median or p95.

For a correctness-only bug, write `Performance: N/A` and explain which performance paths the change
cannot affect. A statement such as "slow" or "no regression" is not performance evidence.

## Submit and follow up

Review the complete report once more for secrets and unsupported claims, then submit it through the
[bug report form](https://github.com/xnorpx/keeppeek/issues/new?template=bug.yml). Respond to
requests for focused evidence or clarification on the same Issue rather than opening duplicates.

A strong report lets another person answer what failed, how to reproduce it, what should have
happened, which build and environment are affected, what evidence proves it, and how the eventual
fix will be verified.
