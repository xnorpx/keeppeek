# Visual configuration management

KeepPeek exposes common camera configuration through the exact
`keeppeek.configuration.v1` capability. The camera fleet page remains readable when the capability
is missing, but shared-default, template, preview, import, and apply commands stay disabled with the
required capability ID.

The typed configuration service complements existing editors instead of replacing them with a
generic schema-driven form. Storage and retention remain in Settings, dashboard layouts remain in
the layout registry, notifications and integrations retain their own revisioned editors, access
remains in Access, and full backup and restore remain separate operations.

## Effective values

The configuration snapshot reports these values separately for each inheritable camera policy:

- the configured shared default, when present;
- the camera override, when present;
- the final effective value;
- the effective source: built-in, default, template proposal, or override;
- whether the current camera worker applies the persisted effective value;
- a warning when persisted and currently applied values differ.

Credentials expose only configured-state booleans. A response never contains a resolved username,
password, UID, or secret-bearing endpoint. Supported credential writes accept complete
`{secret:KEY}` references only.

Selecting **Use inherited value** removes the camera field. It does not copy the current default
into the camera. A later default change therefore continues to flow to that camera. Selecting
**Use built-in value** removes the shared default.

## Templates

Camera templates are versioned human-readable JSON documents stored in
`configuration-templates.json` beside `config.toml`. They can include camera connection and
recording-policy values that the current runtime supports. Notification rules remain in the
notification editor and are not duplicated in camera templates.

Applying a template creates explicit camera overrides. Template edits and deletion never mutate
previously applied cameras. This also means deleting a used template needs no value-preservation
choice: the explicit values already preserve every affected camera's effective configuration.

The server bounds template count, identifiers, names, descriptions, fields, and total document
bytes. Template documents are limited to 16 KiB so import and export fit the WebRTC control
message. Imports reject unsupported document versions, unknown fields, duplicate IDs or names,
inline credentials, invalid enum values, and out-of-range numbers. Import preview does not write;
apply rechecks the preview expiry and configuration revision.

## Plans and bulk changes

Bulk changes target one of these explicit sets:

- selected camera IDs;
- every ID in the current complete filtered result;
- one configured camera group;
- all configured cameras after explicit confirmation.

The server resolves the complete target snapshot and returns its authoritative count. Missing IDs
are visible as skipped targets and make the plan invalid. The UI never treats the currently
rendered virtualized rows as the full filtered set.

A plan contains the configuration revision, expiry, exact targets, redacted semantic changes,
field-addressable issues, and reconnect or restart impact. Plans expire after ten minutes. The
server retains at most 128 plans and limits one plan to 64 cameras. Snapshot pages and plan
responses are measured before send and remain below the 64 KiB control-message limit; an oversized
exact preview fails with a field-addressable instruction to select fewer cameras or fields.

Apply checks the plan ID, expiry, request revision, and plan revision while holding the shared
configuration update lock. The complete candidate is validated with the same configuration and
camera loaders used at startup, then `config.toml` is replaced with one owner-only atomic rename.
Untouched TOML values and secret references remain intact.

Camera worker activation occurs while the same update lock is held. A successful activation is
reported per camera. If runtime activation is unavailable or fails, the atomic persisted
configuration remains staged and the response reports the exact restart recovery policy. A newer
configuration write cannot interleave between commit and activation evidence.

## Conflicts and drafts

Camera update, camera removal, shared-default, template, plan, and import writes use revision
compare-and-set. A conflict returns a typed `ConfigurationError` with the current revision and
field issues when available. The browser reloads current evidence but retains the complete local
draft; retry always requires a new preview or the current revision. There is no blind-overwrite
command.

Revisions are scoped to their atomic boundary. The single-camera API returns and compares the
`config.toml` revision, while configuration snapshots and template transactions return and compare
the combined `config.toml` plus template-document revision. A template-only edit therefore does not
create a false conflict for an unrelated open camera draft.

## Advanced text editing

KeepPeek does not expose a browser TOML editor or a general secret editor. The visual forms cover
supported settings, while direct TOML remains available for source-controlled automation and
newer fields. This avoids promising comment-preserving raw edits that the structured writer cannot
prove.
