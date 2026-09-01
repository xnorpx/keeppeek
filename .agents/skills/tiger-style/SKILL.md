---
name: tiger-style
description: >-
  Applies TigerBeetle-inspired engineering discipline to safety, performance, and developer
  experience. Use when designing, implementing, debugging, testing, reviewing, securing, or
  optimizing production code, state machines, data pipelines, asynchronous flows, and
  resource-bounded systems.
argument-hint: "[design|implement|debug|test|review|optimize] [scope]"
---

# TigerStyle

## KeepPeek Repository Integration

Before applying this skill:

- Treat KeepPeek repository instructions as authoritative when they differ from this skill.
- For Rust work, read and follow the
  [Pragmatic Rust Guidelines](../../../.github/instructions/rust_pragmatic_guidelines.md). Every
  applicable `M-*` rule is required.
- For Svelte, SvelteKit, or frontend TypeScript work under `ui/`, read and follow the
  [Pragmatic Svelte 5 Guidelines](../../../.github/instructions/svelte5_pragmatic_guidelines.md).
  Every applicable Svelte `M-*` rule is required. Use Bun under `ui/` and finish UI changes by
  running `./check.sh` from the repository root.
- Preserve the protected `api/` contract and all narrower repository instructions.

This workflow adapts
[TigerBeetle's TigerStyle](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/TIGER_STYLE.md)
to KeepPeek. Safety comes first, performance second, and developer experience third. When these
goals compete, protect them in that order and search for a simpler design that advances all three.

## Explicit Exclusion

TigerBeetle's static-allocation policy is not part of this skill. Do not infer a prohibition on
dynamic allocation from the upstream document. Follow KeepPeek's language-specific memory, capacity,
reuse, and performance rules instead.

## When to Use

Apply this skill when work changes executable behavior or the design that controls it:

- Architecture, APIs, state machines, protocols, and persistence
- Production implementation and refactoring
- Bug diagnosis and error recovery
- Tests, simulations, fuzzing, and verification
- Security, reliability, and performance work
- Code review of any of the above

Do not use it to constrain early ideation, requirements interviews, prose-only documentation,
release coordination, or generated and vendored code. Apply it to the handwritten code and decisions
around those artifacts.

## Safety

### Bound Work and State

- Put an explicit upper bound on loops, queues, buffers, batches, retries, concurrency, payloads,
  and time spent waiting.
- For intentionally perpetual event loops, document that they do not terminate and bound the work
  performed by each iteration.
- Reject or shed work when a bound is reached. Do not silently convert a finite design into an
  unbounded one.
- Prefer iteration to recursion. If recursion is necessary, prove and enforce a maximum depth.

### Keep Control Flow Explicit

- Use straightforward branches and a minimum of domain-appropriate abstractions.
- Split compound conditions when separate branches make the positive and negative cases easier to
  verify.
- State invariants positively. Prefer `index < count` over a negated or reversed equivalent when it
  expresses the model directly.
- Consider every relevant `else` case. Handle it or assert why it is impossible.
- Keep branching in an owning function and move branch-free calculations or iteration into focused
  helpers. Push decisions up and repetitive work down.
- Keep leaf helpers pure when practical. Let the owning function apply state changes.

### Use Types That Preserve Meaning

- Use fixed-width integer types for persisted, serialized, protocol, and domain values.
- Use `usize` only where Rust collection indexing or capacity APIs require it. Make conversions
  checked and local.
- Put units and qualifiers in names, such as `latency_ms_max`, `frame_count`, and
  `buffer_size_bytes`.
- Use distinct types for values with distinct semantics when mixing them would be dangerous.

### Assert Invariants and Handle Expected Errors

- Treat invalid external input, unavailable resources, timeouts, and other operating conditions as
  expected errors. Return, report, retry, or recover according to the contract.
- Treat violated internal invariants as programmer errors. Assert them at the point where corruption
  first becomes observable.
- Check arguments, results, preconditions, postconditions, and state transitions in non-trivial
  correctness-critical code.
- Assert both the valid space and the invalid space. Tests must cover valid values, invalid values,
  and transitions across the boundary.
- Pair important checks across independent paths, such as before persistence and after loading, or
  before encoding and after decoding.
- Split unrelated assertions so a failure identifies one violated property.
- Assert relationships between constants and type sizes at compile time when the language supports
  it.
- Maintain high assertion density in invariant-heavy code. Target at least two meaningful checks per
  non-trivial function on average across the touched module, but never add tautologies or assert
  expected operating errors to satisfy a quota.

Assertions strengthen understanding; they do not replace it. Build a precise model first, encode it
in checks, and then use tests, fuzzers, or simulations to challenge the model.

### Limit Scope and Function Size

- Declare values at the smallest useful scope and as close as possible to their use.
- Keep each new or materially changed handwritten function within 70 source lines.
- Do not expand an existing over-limit function. Split it along responsibilities when that can be
  done without obscuring control flow.
- Minimize live mutable state and avoid aliases or duplicate sources of truth.

### Control External Interaction

- Validate external events at the boundary, then process them under application-controlled
  scheduling.
- Batch high-frequency network, disk, media, and event work instead of reacting with unbounded work
  per event.
- Revalidate assumptions after suspension points. External state may change while an async function
  is awaiting.
- Pass safety-relevant library options explicitly at the call site. Do not rely on defaults whose
  future change could alter correctness.

### Refuse Hidden Failure

- Enable the strictest practical compiler, linter, and type-checker warnings.
- Handle every error path. Do not discard results or suppress diagnostics without a narrow,
  documented reason.
- Do not knowingly introduce technical debt in the changed surface. If a requirement cannot be met
  safely, surface the conflict instead of hiding a stub, skipped test, unchecked fallback, or
  open-ended TODO.

## Performance

### Sketch Before Building

For non-trivial paths, make a rough resource sketch before implementation:

1. Estimate network, disk, memory, and CPU demand.
2. Consider both bandwidth and latency for each resource.
3. Multiply cost by expected frequency; a cheap operation in a hot loop can dominate an expensive
   rare operation.
4. Record the governing limit or budget in the spec, benchmark, test, or nearby enduring
   documentation.

Use measurement to validate the sketch, not as a substitute for thinking before the design hardens.

### Shape Predictable Work

- Optimize constrained resources in context, usually network before disk, memory, and CPU after
  accounting for frequency.
- Separate control-plane decisions from data-plane repetition.
- Amortize costs with bounded batches.
- Keep hot loops predictable and isolate them in focused functions with simple inputs so redundant
  work is visible to both the compiler and reviewer.
- Benchmark measured hot paths and verify that an optimization improves the governing resource
  rather than moving cost elsewhere.

## Developer Experience

### Name the Model Precisely

- Choose exact nouns and verbs that expose the domain model.
- Follow each language's repository naming convention. Do not import Zig casing rules into Rust,
  TypeScript, or Svelte.
- Avoid abbreviations except established domain terms and conventional short indices in tightly
  scoped mathematics.
- Put units and qualifiers last, ordered from more significant to less significant:
  `latency_ms_max`, not `max_latency_ms`.
- Give related concepts parallel names, but never distort meaning merely to align text.
- Do not overload one name with context-dependent meanings.
- Name helpers and callbacks so their relationship to the owning operation is visible. Put callbacks
  last when local API conventions permit it.

### Reduce State and API Dimensionality

- Keep one canonical representation of mutable state. Derive secondary views instead of
  synchronizing copies.
- Calculate and validate values near their use.
- Prefer the smallest return shape that expresses the contract. Do not return optional values,
  flags, or error variants that callers cannot act on.
- Use named option structures when same-typed positional arguments can be confused or when a literal
  such as `null` or `None` would be ambiguous.
- Avoid accidental copies of large values. Follow Rust ownership conventions; in languages with
  implicit copies, pass values larger than 16 bytes by read-only reference when ownership is not
  intended to change.
- Construct large or immovable state in place when pointer stability matters.

### Prevent Boundary Mistakes

- Treat indexes, counts, sizes, offsets, and capacities as different concepts even when they share a
  primitive representation.
- Make rounding intent explicit for division and test exact, floor, and ceiling boundaries as
  applicable.
- Initialize or clear unused buffer regions before data crosses a trust or determinism boundary.
- Acquire and release resources in visibly paired scopes. Prefer language-native lifetime management
  such as Rust RAII and Svelte effect cleanup.

### Keep Source Easy to Audit

- Put the most important entry points and concepts early, subject to repository and language
  conventions.
- Run the repository formatter. Keep lines at or below 100 columns unless a repository formatter
  defines another limit.
- Use braces for multi-line conditionals where the language permits unbraced forms.
- Write comments only for non-obvious rationale, constraints, or test methodology. Follow KeepPeek's
  ban on redundant comments, decorative separators, and comment section headers.
- Keep comments as complete, clear sentences when they are not short end-of-line labels.

### Keep Dependencies and Tools Deliberate

- Prefer the standard library, existing dependencies, and established repository tools when they
  solve the problem well.
- Add a dependency only when its correctness, maintenance, supply-chain, performance, and
  operational value justify its cost.
- Do not introduce a new tool when the repository's current toolchain can perform the task clearly
  and portably.

## Workflow

1. **Frame:** Identify trust boundaries, state transitions, failure modes, and which of safety,
   performance, or developer experience governs each tradeoff.
2. **Bound:** Write concrete limits for work and resources. Define expected errors separately from
   impossible internal states.
3. **Model:** Choose precise types, names, invariants, and ownership. Remove duplicate state before
   adding behavior.
4. **Sketch:** Estimate network, disk, memory, and CPU costs for non-trivial paths.
5. **Implement:** Keep control flow explicit, state local, functions short, options visible, and
   external work scheduled under application control.
6. **Challenge:** Test valid, invalid, boundary, transition, timeout, cancellation, and recovery
   behavior. Pair critical checks across independent paths.
7. **Verify:** Run the narrowest falsifying check first, then the repository's required full
   validation.
8. **Review:** Reject unbounded work, hidden errors, unexplained defaults, duplicate state,
   ambiguous units, oversized functions, and unjustified dependencies.

## Coordination With Other Skills

- `spec-driven-development`: Put limits, invariants, failure semantics, and performance budgets in
  acceptance criteria.
- `api-and-interface-design`: Encode units, ownership, bounds, and expected errors in contracts.
- `incremental-implementation`: Complete one bounded, end-to-end behavior before widening the
  change.
- `test-driven-development`: Prove positive, negative, boundary, and transition behavior.
- `debugging-and-error-recovery`: Convert the root cause into a narrow invariant and regression
  check.
- `security-and-hardening`: Treat every trust boundary as bounded, validated, and explicit about
  failure.
- `performance-optimization`: Start from a resource sketch, then measure the governing limit.
- `code-review-and-quality`: Apply the priority order and verification checklist before merge.
- `code-simplification`: Simplify structure without deleting meaningful bounds, assertions, error
  handling, or observability.

## Verification Checklist

- [ ] Safety, performance, and developer experience were considered in that order.
- [ ] Loops, queues, retries, concurrency, payloads, and waits have explicit bounds.
- [ ] Expected operating errors are handled; internal invariant failures are asserted.
- [ ] Positive, negative, boundary, and transition cases are tested.
- [ ] Critical invariants are checked through independent paths where practical.
- [ ] New or materially changed functions fit within 70 lines.
- [ ] Domain and wire values use meaningful types, units, and checked conversions.
- [ ] Async suspension does not preserve stale assumptions.
- [ ] Non-trivial resource costs were sketched and measured where risk warrants it.
- [ ] Mutable state has one canonical owner.
- [ ] Compiler, linter, formatter, focused tests, and required repository checks pass.
- [ ] No diagnostic suppression, skipped test, hidden fallback, or unjustified dependency weakens
      the quality bar.
