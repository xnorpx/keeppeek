# Runtime profile supersession

Owner: [#202](https://github.com/xnorpx/keeppeek/issues/202), dispatched from #147.
Base: `3e6a8d0`. Existing unrelated plans remain unchanged.

## Contract

- Reject a command older than the loop's accepted registry or the shared pending status before
  changing camera, recording, or notification policy.
- Permit an identical revision retry, but reject different definitions at the same revision.
- Recheck supersession between camera operations and before publishing completion. A newer pending
  status must survive delayed completion of an older operation. An already running worker stop
  remains non-preemptible, as does an already admitted notification command after its bounded
  acknowledgement wait. This change does not qualify cancellation or the full #202 convergence
  contract.

Reuse the existing registry revision and shared status lock. No schema, dependency, configuration
format, activation surface, or protected API change is needed. Work remains bounded by the existing
camera set; added checks are constant-time except the bounded registry comparison on same-revision
retries.

## Tasks and verification

- [x] Reproduce stale camera reactivation and replacement of a newer pending revision.
- [x] Fence transition admission, per-camera work, and completion; cover identical retries and
      same-revision conflicts.

The implementing PR records focused regressions, the Windows canonical gate, documentation
format/build, independent review, and before/after measurements. Those checks must pass before the
PR is ready for review. Keep #202 open for its remaining scope.

Stable camera identity/IP reuse, live 127-camera performance, complete privacy/restart/watch
qualification, and physical-device release acceptance remain separate unfinished work.
