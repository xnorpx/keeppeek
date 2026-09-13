# Visual configuration management

Use **Cameras** to inspect and change shared camera defaults, effective inherited values, versioned
templates, and explicit camera sets. These operations require Administrator access and the server's
`keeppeek.configuration.v1` capability. If it is unavailable, the fleet remains readable and the
unsupported actions stay disabled.

## Choose the right editor

| Task                                                                 | Location                                                                                                        |
| -------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- |
| Change one camera's connection, streams, or recording policy         | Open its Camera page and edit its configuration.                                                                |
| Change inherited policy, manage templates, or preview a fleet change | Open the Cameras fleet configuration controls.                                                                  |
| Change storage, retention, or listener settings                      | Settings. Use the storage editor's move/restart review when changing paths.                                     |
| Change a dashboard or its display and audience                       | The [live-wall layout controls](./live-wall.md).                                                                |
| Change User camera grants or credentials                             | [Settings > Access & roles](./authentication.md#camera-and-dashboard-access).                                   |
| Change notification rules or MQTT                                    | The [notification and integration editors](./notifications-and-integrations.md).                                |
| Transfer the complete settings and file-backed secrets               | [Configuration ZIP export and apply](./configuration-export-import.md).                                         |
| Configure file-only native event policy                              | Stop the server and edit the [supported camera event table](./configuration-reference.md#native-camera-events). |

The browser has no general secret editor or raw TOML editor. Use the
[configuration reference](./configuration-reference.md) for supported file fields. Preserve unrelated
sections and existing secret references when editing a stopped installation.

## Inspect inheritance before changing it

Every inheritable policy shows its configured default, camera override, final effective value,
source, and current runtime state. **Use inherited value** removes an override so future default
changes continue to flow to that camera.

A built-in value is the fallback shipped by KeepPeek. A shared default changes the fallback for
cameras without that override. An explicit camera value takes precedence. **Use built-in value**
removes a shared default. Removing an override does not freeze the current value into the camera.

For example, changing a shared recording mode affects cameras that inherit it. A camera with an
explicit `main` override keeps that policy. Check both the effective value and runtime status after
applying a change, especially if a camera is disconnected.

## Preview and apply a fleet change

1. Open the shared-default or bulk-change view and choose the fields to change.
2. For bulk changes, select explicit camera IDs, the complete filtered result, one configured group,
   or all configured cameras. The all-cameras scope requires explicit confirmation.
3. Request a preview. Confirm the server's authoritative camera count, old and new values, skipped
   targets, validation issues, and reconnect or restart consequences.
4. Resolve issues before applying. A missing target makes the plan invalid; an oversized preview
   requires a smaller selection or fewer changed fields.
5. Apply the current preview, then check the per-camera activation result and camera health.

The filtered scope means the complete filter result, including rows outside the visible viewport.
One plan includes at most 64 cameras and expires after ten minutes. The server retains at most 128
plans and bounds control responses to 64 KiB. A preview that cannot fit fails explicitly.

Template and bulk operations always produce a server-owned preview before apply. The preview lists
the exact authoritative cameras, semantic old and new values, skipped targets, validation issues,
and reconnect or restart consequences. Applying a template creates explicit overrides; later
template edits or deletion do not silently change cameras.

Configuration writes use the edit-start revision, validate the complete candidate, preserve
unrelated fields and secret references, and replace the configuration atomically. A conflict
reloads current evidence without discarding the local draft. If a committed change cannot activate
on one camera worker, KeepPeek reports that camera and the required restart recovery action.

## Create and reuse a camera template

In the templates view, create a named template with supported connection or recording-policy fields
you want to reuse. Templates can be edited, duplicated, deleted, imported, and exported. They do not
include notification rules; manage those in their own editor.

Credential fields accept complete references such as `{secret:CAMERA_PASSWORD}`. Create the
corresponding secret in the existing companion `secrets.toml` through your protected local workflow.
A template does not carry the secret value to another recorder.

To use a template, choose it for a bulk target set, review the server preview, then apply. Applying
creates explicit camera overrides. Later template edits or deletion do not change cameras that
previously used it. To return a camera to shared defaults, remove the relevant overrides with
**Use inherited value**.

Template exchange uses versioned JSON, not a configuration ZIP or raw TOML. Export the template
document, choose it for import on the target, review its validated contents, and apply the preview.
Unsupported versions, unknown fields, duplicate IDs or names, inline credentials, invalid values,
and oversized documents are rejected before mutation. Documents are limited to 16 KiB and contain
at most 64 templates. Template names are unique ignoring case.

## Resolve conflicts and activation failures

| Result                                           | What to do                                                                                                               |
| ------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------ |
| The configuration changed while you were editing | Read the refreshed evidence, compare it with the retained draft, and request a new preview. There is no blind overwrite. |
| A preview expired                                | Request and review a new preview. A ten-minute-old target set may no longer be valid.                                    |
| A target is missing or the preview is too large  | Correct the target selection or split it into smaller explicit batches.                                                  |
| A value fails validation                         | Correct the indicated field. Check the reference for units, bounds, inheritance, and supported secret syntax.            |
| Saved values differ from runtime                 | Follow the reported per-camera reconnect or restart action, then verify health and effective values again.               |
| The capability is unavailable                    | Check the connected server version. The visible fleet does not imply configuration writes are supported.                 |

Conflicts retain the complete local draft while reloading current evidence. Single-camera edits use
the configuration revision; template transactions also account for the template document. A
template-only edit therefore does not create a false conflict for an unrelated camera draft.

The detailed contract, limits, and ownership boundaries are in
[configuration engineering guide](https://github.com/xnorpx/keeppeek/blob/main/docs/configuration-management.md).
Use [configuration exchange](./configuration-export-import.md) for full-recorder settings recovery.
