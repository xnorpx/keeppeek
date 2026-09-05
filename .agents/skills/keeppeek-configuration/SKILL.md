---
name: keeppeek-configuration
description: >-
  Inspect, document, validate, and extend KeepPeek configuration. Use when working
  with config.toml, secrets.toml, settings persistence, configuration structs,
  TOML sections or fields, password references, camera defaults, templates,
  layout persistence, permissions, or configuration import, export, and recovery.
---

# KeepPeek Configuration

## Storage Contract

- Use the existing application configuration file, named `config.toml` by default.
  Do not create a second settings file, a layout file, or a permission catalog.
- Keep passwords and tokens in the existing companion `secrets.toml`, or in the
  environment when the supported resolver permits it. This companion is not a
  second settings store.
- Persist new layout definitions, revisions, selections, and permission
  assignments in the application configuration through its existing writer.
- Preserve the protected `api/` contracts. Do not implement an unsupported option
  merely by adding a key that the loader ignores.
- Do not read or print an operator's private configuration or secrets just to
  discover the schema. Use source code, tests, and synthetic fixtures.

## Sources of Truth

Read the relevant source before proposing a setting:

- [Canonical section and field reference](../../../book/src/configuration-reference.md)
- [Configuration loader and writer](../../../src/config.rs)
- [Camera configuration](../../../src/cameras/mod.rs)
- [Access identities and persisted state](../../../src/access.rs)
- [Layout registry](../../../src/server/peek_layouts.rs)
- [Camera templates](../../../src/server/configuration.rs)
- [Notification model](../../../src/notifications/model.rs)
- [Notification persistence](../../../src/notifications/store.rs)
- [MQTT configuration](../../../src/event_forwarder/config.rs)
- [Configuration management](../../../book/src/configuration-management.md)
- [Configuration exchange](../../../book/src/configuration-export-import.md)
- [Secret handling](../../../docs/secrets.md)

Follow nested types, defaults, serde attributes, validation functions, and section
readers. The root `Config` struct alone does not enumerate all persisted sections.
Distinguish operator-editable settings, server-managed records, transient runtime
state, and legacy migration inputs.

## Workflow

1. Identify the requested section and its owning loader, validator, writer, and
   nearest tests. Confirm the actual TOML spelling, not just the Rust field name.
2. Verify the effective value, default, units, limits, inheritance, and restart or
   live-apply behavior. Treat absent values differently from explicit values
   wherever the implementation does.
3. For passwords, use `{secret:KEY}` for direct substitution and
   `{secret:KEY|url}` for a credential embedded in a URL component. Verify that the
   section's loader resolves references before recommending them for that field.
4. Preserve secret references and unrelated configuration sections during writes.
   Reuse the shared configuration-update lock and atomic writer. A whole-file
   rewrite from a partial struct can erase settings owned by another subsystem.
5. Validate the complete candidate before mutation. On failure, keep the current
   file and runtime state unchanged. Never write resolved credentials back into
   configuration or return them through a settings response.
6. Update the book reference with every supported section, struct, serialized
   field, type, default, bound, secret-reference rule, and ownership distinction
   affected by the change. Link to source rather than duplicating the field
   inventory in this skill.
7. Run focused tests and documentation checks. Report unsupported behavior
   explicitly instead of documenting planned features as current functionality.

## Access and Layout Boundaries

- Camera authorization and layout visibility are separate decisions. Access to a
  grid must never grant access to its cameras.
- Enforce camera restrictions on the server for media, events, thumbnails,
  exports, and controls. Hidden tiles are not authorization.
- Distinguish viewing a layout from editing or sharing it. Preserve ownership and
  stale-write detection when importing, duplicating, or changing a layout.
- Check the existing trusted-local Administrator policy before making per-user
  guarantees. Do not silently change that policy or invent new identity flows.
- Document these controls as supported only after the implementation and tests
  enforce them. Until then, retain the limitation in the operator reference.

## Verification

- Round-trip changed settings through TOML using the real loader and writer.
  Check reload, default values, unknown-section preservation, and failed writes.
- Use synthetic secret values. Check missing keys, environment overrides,
  URL-escaped credentials, redaction, and preservation of reference tokens.
- When changing persisted sections, check configuration export/import and backup
  restoration. An ordinary layout export must not include unrelated identity
  state, credentials, or resolved secret values.
- For Rust changes, follow the repository's Pragmatic Rust and TigerStyle rules.
  For changes under `ui/`, use Bun and run `./check.sh` from the repository root.
- For book or skill changes, run `bun run --cwd ui format:markdown:check` and
  `mdbook build book`. Validate skill frontmatter and relative links.

Never log secret values, credential verifiers, full credential-bearing URLs, or
private configuration contents as verification evidence.
