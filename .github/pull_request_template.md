## Summary

Describe the user-visible outcome and link the Discussion or reproducible bug issue this PR follows.

Context:

## Acceptance criteria verification

A PR is not ready for review until every acceptance criterion from the linked context has one row below. Quote the criterion or use its stable ID. The PR author, whether human or AI, must show what verifies each criterion and the observed result.

| Acceptance criterion | Testable outcome | Verification test or check | Evidence and observed result |
| -------------------- | ---------------- | -------------------------- | ---------------------------- |
| AC-1:                |                  |                            |                              |

- [ ] Every acceptance criterion from the linked context is represented above.
- [ ] Every outcome is observable and every verification method is reproducible.
- [ ] Automated tests, benchmarks, static checks, or CI artifacts are linked where available.
- [ ] Manual checks are used only when automation is impractical and include exact steps, environment, and expected result.
- [ ] All represented criteria pass. Unmet or unverified criteria remain unchecked in the linked context.

## Performance evidence

Show measured before-and-after numbers for every runtime, build, memory, storage, network, or interaction path this PR can affect. A statement such as "no regression" is not evidence without numbers.

- Baseline commit:
- Test commit:
- Environment (hardware, OS, browser, build profile):
- Workload or dataset:
- Command or harness:

| Metric and unit | Before | After | Delta | Budget or target | Runs and statistic |
| --------------- | -----: | ----: | ----: | ---------------: | ------------------ |
|                 |        |       |       |                  |                    |

For a PR that cannot affect performance, replace the table with `Performance: N/A` and explain why no runtime, build, memory, storage, network, or interaction path changes.

- [ ] Performance-affecting changes include reproducible before-and-after measurements.
- [ ] Measurements use the same environment and workload, or differences are disclosed.
- [ ] Relevant percentile or spread is reported, not only the best run.

## Validation

List every command, suite, manual procedure, and artifact used to validate this PR.

- [ ] Canonical repository checks pass (`./check.sh` on macOS/Linux or `check.bat` on Windows).
- [ ] Focused tests for the changed behavior pass.
- [ ] Failure, boundary, and regression cases are covered where relevant.

## Review evidence

Link CI runs, benchmark artifacts, sanitized logs, screenshots, recordings, or other evidence needed to reproduce the claim. Do not include credentials, camera addresses, private imagery, tokens, or other secrets.
