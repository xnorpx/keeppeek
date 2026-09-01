# Visual configuration management

The Cameras page can inspect and change shared camera defaults, effective inherited values,
versioned templates, and explicit camera sets when the server advertises
`keeppeek.configuration.v1`.

Every inheritable policy shows its configured default, camera override, final effective value,
source, and current runtime state. **Use inherited value** removes an override so future default
changes continue to flow to that camera.

Template and bulk operations always produce a server-owned preview before apply. The preview lists
the exact authoritative cameras, semantic old and new values, skipped targets, validation issues,
and reconnect or restart consequences. Applying a template creates explicit overrides; later
template edits or deletion do not silently change cameras.

Configuration writes use the edit-start revision, validate the complete candidate, preserve
unrelated fields and secret references, and replace the configuration atomically. A conflict
reloads current evidence without discarding the local draft. If a committed change cannot activate
on one camera worker, KeepPeek reports that camera and the required restart recovery action.

Templates exchange versioned JSON with secret references only. KeepPeek validates an import and
shows its complete contents before mutation. General browser secret editing and raw TOML editing
are not available.

The detailed contract, limits, and ownership boundaries are in
[Visual configuration management](https://github.com/xnorpx/keeppeek/blob/master/docs/configuration-management.md).
