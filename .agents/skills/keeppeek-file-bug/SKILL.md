---
name: keeppeek-file-bug
description: "Investigate, draft, validate, and file a reproducible KeepPeek GitHub bug issue that satisfies the live bug.yml form. Use when: reporting, filing, opening, or creating a KeepPeek bug; converting a failure, regression, log, screenshot, or test result into issue-ready evidence; checking for duplicate bugs; or defining testable fix acceptance criteria and performance verification."
argument-hint: "[symptom, regression, failing command, or affected workflow]"
---

# File a KeepPeek Bug

Create one evidence-backed issue in `xnorpx/keeppeek` that a maintainer can reproduce,
scope, implement, and verify without guessing. Treat
[`../../../.github/ISSUE_TEMPLATE/bug.yml`](../../../.github/ISSUE_TEMPLATE/bug.yml) as
the authoritative schema on every invocation.

## Non-Negotiable Rules

1. File only a reproducible KeepPeek defect. Route setup, troubleshooting, and usage
   questions to KeepPeek Q&A. Route new behavior and feature proposals to Suggestions.
2. Never invent reproduction results, versions, logs, measurements, approval, or
   preflight attestations. Ask for missing user-owned facts or stop without filing.
3. Never publish credentials, camera addresses, stream or snapshot URLs, tokens,
   cookies, private media, host paths containing personal data, or unsanitized logs and
   screenshots. Redact at the source rather than relying on a warning in the issue.
4. Investigate read-only unless the user separately asks for a fix. Do not mutate camera,
   configuration, storage, or repository state merely to improve the report.
5. Search open and closed issues before drafting. Do not create a duplicate. Return the
   matching issue and the evidence that links it when one already exists.
6. Reproduce on the current release or identify the exact affected build. Do not check
   the corresponding preflight item without evidence.
7. Every acceptance criterion must have one observable outcome and one named,
   reproducible verification test or check. Keep completion criteria unchecked.
8. Performance-related bugs require measured numbers with units, workload, environment,
   run count, and statistic. Use `Performance: N/A` only with a specific explanation of
   why no runtime, build, memory, storage, network, or interaction path is affected.
9. Attempt to generate and attach the KeepPeek diagnostics package. Missing logs do not
   block filing, but the issue must state why they are unavailable and warn that the report
   may be closed as `needs more info`.
10. Show the complete sanitized draft and obtain explicit user approval before the public
    write. Approval of an earlier, materially different draft does not carry forward.
11. After filing, fetch the issue from GitHub and verify its title, `bug` label, body,
    required sections, and sanitization before reporting success.

## Orchestrate Repository Knowledge

Before investigating, enumerate `SKILL.md` files under `.agents/skills/`,
`.github/skills/`, and `.claude/skills/`, plus repository `*.agent.md` files and skills or
agents advertised by the host. Read their frontmatter names and descriptions, then
deduplicate compatibility wrappers by skill name. Read root `AGENTS.md`, the nearest
scoped `AGENTS.md` for affected code, and any applicable instruction files.

Load and follow every skill, agent, and instruction whose described domain overlaps the
symptom or error, affected subsystem, reproduction procedure, evidence source, or
verification method. When applicability is unclear, read the candidate's full scope
before deciding. Do not invoke unrelated workflows merely to increase the count.

- For camera discovery, authentication, streams, recordings, codecs, transports, or
  camera quality, load `keeppeek-camera-setup` and obey all credential and mutation
  safeguards. Never include its private paths or credentials in the issue.
- For code ownership, controlling behavior, existing tests, or likely regression scope,
  use the available read-only exploration subagent. In VS Code, prefer the `Explore`
  agent with medium thoroughness and a focused brief containing the observed failure.
- For frontend behavior, load the applicable Svelte 5 instructions before interpreting
  UI ownership or proposing verification.
- For Rust behavior, load the repository's Pragmatic Rust Guidelines and any nearest
  scoped `AGENTS.md` before naming tests or implementation boundaries.
- Use browser, camera, test, logging, and diagnostics capabilities only when relevant and
  safe. Separate directly observed evidence from code-informed hypotheses.

This skill owns routing, issue completeness, redaction, and publication. Other skills and
agents provide evidence; they do not waive this workflow's gates.

## Phase 1: Load the Live Form

1. Read `.github/ISSUE_TEMPLATE/bug.yml` at invocation time.
2. Parse it as YAML with a structured parser and enumerate every `body` entry with an
   `id`, label, type, options, and required validation.
3. Build a completion ledger from that result. The current form includes:
   `preflight`, `affected-area`, `bug-description`, `reproduction`,
   `expected-and-actual`, `regression-and-impact`, `environment`, `evidence`,
   `acceptance-criteria`, and `performance`.
4. If the form adds or changes fields, satisfy the live form rather than this cached list.
5. Verify the repository remote resolves to `xnorpx/keeppeek` and GitHub authentication
   can read issues. Do not display authentication tokens.

## Phase 2: Route and Search

Classify the report before gathering deep evidence:

- **Bug:** existing KeepPeek behavior fails, regressed, corrupts state, reports an
  incorrect state, violates a documented contract, or misses an established budget.
- **Support or configuration:** behavior is not yet shown to violate a KeepPeek contract.
  Route to `https://github.com/xnorpx/keeppeek/discussions/categories/q-a`.
- **Feature:** the requested behavior does not exist or changes product scope. Route to
  `https://github.com/xnorpx/keeppeek/discussions/categories/suggestions`.
- **Security vulnerability:** do not open a public issue. Use GitHub's private security
  reporting path or ask the maintainer for a private channel.

Derive two to five distinctive terms from the symptom, affected workflow, error, camera
source, or failing test. Search both open and closed KeepPeek issues by title and body.
Read plausible matches rather than comparing titles alone. Record the query and why each
close match is or is not the same defect.

## Phase 3: Establish Reproducible Evidence

Start from the most concrete anchor: failing command or test, exact UI workflow, error,
timestamped log event, malformed recording, API response, or regression range.

1. Record the starting state and the smallest deterministic reproduction sequence.
2. Run the cheapest safe check that could disprove the suspected defect.
3. Repeat enough times to report `always`, `intermittent`, or `N of M attempts`.
4. Capture expected and actual outcomes as observable states, values, errors, timings, or
   resource use. Do not write only "does not work."
5. Determine whether it previously worked. Identify last-known-good and first-known-bad
   versions when evidence permits; otherwise state `unknown`.
6. Record impact, data-loss or evidence-loss risk, and any proven workaround.
7. Collect only relevant diagnostics around correlated timestamps. Prefer small excerpts
   over complete logs.
8. Run focused existing tests when they can reproduce or bound the problem. Record exact
   commands, commit, environment, exit status, and important output.
9. Select the live form's affected-area option from the user-visible workflow that fails,
   not from a guessed implementation owner. When a defect crosses areas, select the
   primary failing workflow and name secondary effects in the description. Use `Other`
   only when no current option describes the defect.
10. Open **Settings → Logs & diagnostics** and use **Download diagnostics** after the
    reproduction so the package contains the relevant retained server and browser logs.
    Prefer a package generated from the exact affected build and browser session.

For intermittent, hardware-specific, destructive, or production-only defects, do not
pretend a local reproduction exists. Provide a bounded capture procedure, observed
frequency, and sanitized evidence; ask the user for facts that only they can observe.

## Phase 4: Complete the Environment

Populate every environment line from evidence, using `N/A` only when genuinely
inapplicable and `unknown` when applicable but unavailable:

- KeepPeek version, commit, and build profile
- OS and architecture
- installation method
- browser or client and version
- anonymized camera make, model, and firmware version
- camera source or integration
- stream codec, resolution, FPS, and audio
- camera count and relevant catalog or date-range scale
- storage type and free space

Gather system and repository values directly when safe. Ask the user for hardware,
firmware, deployment, or private-environment facts that cannot be inferred. Never turn a
guess from source code into an environment fact.

## Phase 5: Sanitize Evidence

Review every proposed line, attachment, screenshot, and command output before drafting.

- Replace camera IPs and hostnames with stable aliases such as `camera-a`.
- Remove user names and private filesystem prefixes where they are not diagnostic.
- Remove URL userinfo, query strings, credentials, secrets, tokens, cookies, and headers.
- Crop or redact private imagery, notification text, license plates, faces, and location
  information.
- Preserve timestamps, error types, safe model/profile metadata, and correlation IDs only
  when they help reproduce or diagnose the defect.
- Do not attach an artifact that cannot be confidently sanitized. Describe how a
  maintainer can generate an equivalent safe fixture instead.

Run a final secret-pattern and private-data review over the assembled draft. A scan is a
backstop, not proof that evidence is safe.

For an available `keeppeek-diagnostics-*.json.gz` package, decompress it locally and verify
that it contains a `keeppeek-diagnostics` manifest plus `server_logs`, `browser_logs`, and
`log_buffer`. Confirm the manifest says `privacy: scrubbed`, review free-form values again,
and keep the package compressed for attachment. Do not modify package contents and then
present it as the generated artifact.

## Phase 6: Define the Fix Contract

Write acceptance criteria around corrected behavior, not a guessed implementation.

For each criterion include:

```markdown
- [ ] AC-N: <one observable behavior or measurable result>
  - Expected outcome: <exact state, value, artifact, or threshold>
  - Verification: <existing or proposed test, command, benchmark, or exact procedure>
```

Include the primary regression reproduction. Add relevant failure, boundary, recovery,
compatibility, security, accessibility, or persistence criteria. Include a performance
criterion whenever timing, throughput, resource use, scale, or responsiveness can change.

Verification must identify the appropriate test layer and concrete assertion. Do not use
"tests pass" without naming the suite or behavior, and do not require private hardware
when a sanitized deterministic fixture can prove the contract. Clearly distinguish an
existing test from a test that the fix must add.

## Phase 7: Record Performance Evidence

For a performance-related defect, collect and report:

- metric and unit;
- observed baseline or current value;
- expected budget or target and its source;
- representative workload or dataset;
- hardware, OS, browser when applicable, and build profile;
- exact command or harness;
- warm-up policy, run count, and statistic such as median and p95;
- spread or raw result artifact when useful.

Use comparable runs and disclose differences. A qualitative claim such as "slow" or "no
regression" does not fulfill the form. For non-performance defects, use this structure
only when the evidence supports it:

```text
Performance: N/A — This is limited to <correctness or presentation behavior>; the
reproduction does not change <relevant timing, throughput, memory, storage, network,
build, or interaction paths>.
```

## Phase 8: Draft the Rendered Issue

Use a concise title in the form `[Bug]: <observable failure>`. Render the body in the live
form's order with a `##` heading matching each field label. Include every required field,
even when its value is a justified `N/A` or `unknown`.

Under **Before submitting**, the agent may render an item as checked only when objective
evidence or the user's explicit confirmation supports that exact attestation. Present the
attestations in the draft so approval also confirms them; never infer a check from silence.
Under **Affected area**, use exactly one current dropdown option. Keep every fix acceptance
criterion unchecked. Do not include form placeholders or private investigative notes.

Under **Diagnostics package and sanitized evidence**, include the uploaded GitHub attachment
link and correlated timestamps when a package is available. If generation or upload failed,
write `Diagnostics package: Not attached`, the exact reason, and `This report may be closed
as needs more info.` Do not treat the missing package as a filing blocker.

The draft must let a reviewer answer all of these without inference:

- What fails and what is the impact?
- What exact steps reproduce it, how often, and from what starting state?
- What should happen, and what measurably happens instead?
- Which exact build and environment exhibit it?
- What sanitized evidence proves the report?
- What outcomes make the fix complete, and what verifies each one?
- What performance numbers apply, or why is performance genuinely not applicable?

Show the complete final title and body to the user. Explicitly identify any `unknown` or
manual-only evidence. Ask for approval to publish this exact sanitized draft.

## Phase 9: Validate and File

Before the public write:

1. Compare the draft against every required live-form entry in the completion ledger.
2. Confirm the user has reviewed every preflight attestation and all checked items are
   supported by evidence or explicit confirmation.
3. Confirm there are no empty headings, template placeholders, or unsupported claims.
4. Confirm each acceptance criterion includes `Expected outcome` and `Verification`.
5. Confirm performance evidence has numbers or a justified `Performance: N/A`.
6. Validate the diagnostics package and prepare it for upload, or record a concrete reason
   why it is unavailable.
7. Confirm the duplicate search is current.
8. Repeat the redaction and secret review.

After explicit approval, prefer the GitHub issue form in a browser so the generated package
can be uploaded into the required evidence field before submission. Verify the rendered
Markdown contains the resulting GitHub attachment link. When browser file upload is not
available, ask the user to attach the package and return the resulting link. If the package
cannot be attached, preserve the unavailable reason and continue with the user's approval.

Use the available GitHub issue-write capability for the final write. If no dedicated tool
exists, use GitHub CLI against `xnorpx/keeppeek`, set the `bug` label, and pass the body
through a file or standard input rather than embedding multiline evidence in a shell
command. Do not publish local private artifact paths. If neither an authenticated write
tool nor GitHub CLI is available, stop with the validated draft and state that the issue
was not filed.

## Phase 10: Verify the Remote Issue

Fetch the created issue from GitHub and verify:

- repository is `xnorpx/keeppeek`;
- title begins with `[Bug]:`;
- label `bug` is present;
- every required live-form heading has non-empty content;
- preflight checks are present and truthful;
- the diagnostics package has a GitHub attachment link, or the body records why it is
  unavailable and warns that the report may be closed as `needs more info`;
- acceptance criteria remain unchecked and contain outcomes and verification;
- performance has measured values or a justified `N/A`;
- no placeholder or sensitive value appears in the remote body.

If validation fails, correct the issue immediately and fetch it again. Return the issue
number and URL, duplicate-search summary, reproduction status, tests or measurements run,
and any evidence still marked `unknown`. Do not report completion until the remote issue
passes this verification.
