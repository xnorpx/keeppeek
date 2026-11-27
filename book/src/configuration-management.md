# Visual configuration management

This chapter covers visual configuration across `/settings` and `/cameras`, including draft vs. live-apply vs. restart behavior, browser-local preferences, shared camera inheritance, camera templates, and runtime profiles. Configuration writes require **Administrator** access (Users can change only browser-local theme settings and observe active runtime-profile status); KeepPeek stores all durable settings in `config.toml` and file-backed secrets in sibling `secrets.toml` with no raw TOML editor, no general secret editor, and no blind overwrites.

## Video walkthrough

Watch how Administrators inspect shared camera inheritance, create templates with `{secret:...}` references, and preview and apply bulk fleet changes on `/cameras`.

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/cameras.desktop.fleet-config.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/cameras.desktop.fleet-config.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/cameras.desktop.fleet-config.mp4">Download the camera fleet configuration demo</a>.
</video>

## Choose the right editor

| Task                                                       | Route / surface                                      | Requires restart?                 | Details                                                                                   |
| ---------------------------------------------------------- | ---------------------------------------------------- | --------------------------------- | ----------------------------------------------------------------------------------------- |
| Edit one camera's connection, streams, or recording policy | `/camera`                                            | No (restarts camera worker)       | Uses `config.toml` revision compare-and-set.                                              |
| Change shared camera defaults, templates, or bulk policy   | `/cameras` (**Configuration** / **Manage selected**) | No (per-camera worker activation) | Requires `keeppeek.configuration.v1`.                                                     |
| Switch or edit camera runtime profiles                     | `/settings#runtime-profiles`                         | No (async worker convergence)     | Durable in `[runtime_profiles]`; WebRTC-only activation.                                  |
| Change server host/port, retention, or storage paths       | `/settings` (**Server**, **Storage & retention**)    | Yes                               | Drafts stage changes until **Apply changes** / restart.                                   |
| Manage named storage volumes or metadata relocation        | `/settings#storage` (**Named storage volumes**)      | Yes (except live drain/moves)     | See [Move storage deliberately](./upgrades-and-migrations.md#move-storage-deliberately).  |
| Configure fourth-tier local archive                        | `/settings#storage` (**Local archive**)              | Yes                               | See [Recording maintenance](./recording-maintenance.md).                                  |
| Edit dashboards, audience, or kiosk wake lock              | `/settings#dashboards`                               | No                                | See [Live wall and kiosk display](./live-wall.md).                                        |
| Manage access keys, User grants, or guest sharing          | `/settings#access`                                   | No                                | See [Authentication and access control](./authentication.md#camera-and-dashboard-access). |
| Edit notification rules, cloud push, or MQTT               | `/settings#notifications`, `/settings#integrations`  | No                                | See [Notifications and integrations](./notifications-and-integrations.md).                |
| Export or apply full settings + `secrets.toml` ZIP         | `/settings#backups`                                  | Yes (apply only)                  | See [Export and import configuration](./configuration-export-import.md).                  |
| Change UI theme or view server identity / logs             | `/settings#appearance`, `/settings/logs`             | No                                | Theme is browser-local; system fields are read-only evidence.                             |
| Configure file-only native camera events                   | Stopped `config.toml` (`[cameras.<name>.events]`)    | Yes (manual restart)              | See [Native camera events](./configuration-reference.md#native-camera-events).            |

## Persistence, drafts, and restart boundaries

Every persisted write holds the shared configuration-update lock, checks the edit-start revision, validates the complete candidate with the startup loaders, preserves unrelated TOML sections and `{secret:KEY}` / `{secret:KEY|url}` references, and atomically replaces `config.toml` with owner-only permissions. The browser never receives resolved secret values.

| Change class                       | Examples                                                                                                                                                                                                                     | When it takes effect                                                           | Failure or reload behavior                                                                                                  |
| ---------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------- |
| **Browser-local preference**       | **Settings → Appearance & time** theme (**Dark**, **Light**, **Match system**)                                                                                                                                               | Immediately in this browser only                                               | Never written to `config.toml`; video surfaces always remain dark.                                                          |
| **Live durable apply**             | Single-camera edit, `/cameras` defaults/templates/bulk plan, **Runtime profiles**                                                                                                                                            | Commits atomically, then activates workers                                     | If a worker fails to activate, persisted settings stay saved and the UI reports the per-camera reconnect or restart action. |
| **Staged save (restart required)** | **Edit server** (**Save server settings**), **Change storage** (**Continue to review** → **Stage storage changes**), **Edit volume draft**, **Edit archive settings** (**Save archive settings**), **Confirm metadata move** | Only after **Apply changes**, **Restart and move storage**, or service restart | Current runtime configuration stays active until restart; navigating away prompts before discarding unsaved drafts.         |

### Browser-local appearance and read-only system evidence

Under `/settings#appearance`, **Appearance & time** persists only your local browser theme (**Dark**, **Light**, or **Match system**). **Language** is fixed to English, **Browser reduced motion** reflects your OS/browser preference, and **Server time zone**, **Clock preference**, and **Week starts** are unavailable rather than cosmetic overrides—timeline and export timestamps stay unambiguous UTC.

Under **System & updates**, server version, host, OS, process, uptime, and camera-catalog metadata are read-only health evidence. **Update channel**, **Update check**, and **Erase all recordings** are intentionally disabled; use **Restart** to bounce the recorder cleanly, **Open logs** (`/settings/logs`) or **Download diagnostics** for scrubbed troubleshooting bundles, and [Upgrades and migrations](./upgrades-and-migrations.md) for version updates.

## Camera fleet configuration (`/cameras`)

Open **Cameras** and select **Configuration** (or select cameras and choose **Manage selected**) to manage shared defaults, templates, and bulk updates. This surface requires the `keeppeek.configuration.v1` capability; if unavailable, the camera fleet stays readable while write controls remain disabled.

### Inheritance and secret references

Inheritable camera policies resolve in strict order: **Built-in fallback → Shared default (`[camera_defaults]`) → Explicit camera override**. The **Overview** and **Defaults** tabs show the shared default, camera override, final effective value, source, and whether the live camera worker currently matches the saved value.

- **Use inherited value** deletes a camera override so future shared-default edits flow to that camera; it does not freeze the current value.
- **Use built-in value** deletes a shared default.
- Credential fields display configured-state booleans only and accept complete `{secret:KEY}` references. Define the matching key in local `secrets.toml`; never enter raw secret values in templates.

### Preview and apply a fleet change

1. Open **Configuration** on `/cameras` and choose the **Defaults** or **Bulk** tab.
2. For **Bulk** changes, select a target scope: explicit camera IDs, the complete filtered result (including rows outside the virtualized viewport), one configured group, or all configured cameras (requires explicit confirmation).
3. Choose a field patch or a saved template, then request a server preview.
4. Inspect the authoritative camera count, semantic old/new values, skipped targets, validation issues, and reconnect or restart impact. Resolve any missing targets or validation errors before continuing.
5. Apply the active preview, then verify per-camera activation status and [Camera and stream health](./camera-health.md).

**Fleet plan bounds:** A plan targets at most **64 cameras**, expires after **10 minutes**, and must fit within the **64 KiB** WebRTC control-message ceiling (oversized previews fail explicitly—split into smaller batches or fewer fields). The server retains at most **128 plans**.

### Create and exchange camera templates

In the **Templates** tab, create, edit, duplicate, delete, import, or export named templates stored in `[configuration_templates]` inside `config.toml`. Templates carry supported camera connection and recording-policy fields plus `{secret:KEY}` credential references; they do not store secret values or notification rules.

- Applying a template writes **explicit camera overrides** onto the selected targets. Later edits or deletion of the template never mutate previously configured cameras.
- Template exchange uses **versioned JSON** (not configuration ZIP or TOML), bounded to **16 KiB** and **64 templates** with case-insensitive unique names.
- Import preview validates before mutation and rejects unsupported versions, unknown fields, duplicate IDs or names, inline credentials, invalid enums, and out-of-range numbers.

## Runtime profiles (`/settings#runtime-profiles`)

Runtime profiles let an Administrator switch camera behavior between **Base** and up to **32 named profiles** stored in `[runtime_profiles]` in `config.toml`. Profiles commit atomically and converge asynchronously across camera workers without restarting the recorder.

- **Activation boundary:** Manage and activate profiles only in **Settings → Runtime profiles** or via an authorized WebRTC `StateStoreCommand` client on `service/runtime-profiles/`. No HTTP, MQTT, Home Assistant, CLI, webhook, or scheduled activation surface exists.
- **Target identity:** Up to **127 camera overrides** per profile are keyed by server-managed `profile_identity` UUIDs (`identity_schema = 1` rollback guard). Editing a camera's IP preserves its UUID; deleting and re-adding a camera creates a new UUID and leaves the old target listed as unavailable (`configured camera is missing`) until removed or **Select Base** is chosen.
- **Allowed override fields only:** Each camera entry accepts only **Availability**, **Recording** (`off`, `sub`, `main`, `both`, `event-boost`, `event-only`), **Privacy** (feeds `PrivacyRegistry`; cannot weaken an active privacy schedule), and **Notifications** (suppresses candidates and queued outbox delivery without editing saved rules). Omitted fields inherit **Base**; profiles cannot enable a disabled Base camera or Base `off` recording.
- **Convergence and recovery:** Visible cameras report `pending`, `applied`, or `degraded` with per-field provenance (`base` or `profile`). Camera stops share a 6-second deadline; failures or timeouts stay `degraded` (never silently reverting to Base) while restrictive privacy stays closed. After fixing the underlying camera or storage fault, choose **Reapply active policy** (requires no unsaved draft) or **Select Base**.

## Resolve conflicts and activation failures

| Symptom                                 | Cause                                                                                   | Next action                                                                                                                                                                                                                                                                     |
| --------------------------------------- | --------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Configuration changed while editing     | Compare-and-set revision mismatch (no blind overwrite)                                  | Review the reloaded current values against your retained local draft and request a new preview. Single-camera edits track `config.toml` revision; fleet/template edits track combined config + template revision so template changes do not conflict with single-camera drafts. |
| Preview expired                         | Plan exceeded its 10-minute lifetime                                                    | Request and review a fresh preview before applying.                                                                                                                                                                                                                             |
| Missing target or oversized preview     | Target camera deleted or response exceeds 64 KiB / 64 cameras                           | Update target selection or split into smaller explicit batches.                                                                                                                                                                                                                 |
| Validation error                        | Out-of-range value, unknown field, or malformed `{secret:KEY}`                          | Fix the highlighted field using the [configuration reference](./configuration-reference.md).                                                                                                                                                                                    |
| Saved value differs from runtime        | Camera worker activation failed or requires restart                                     | Follow the reported per-camera reconnect/restart action, then verify health and effective values.                                                                                                                                                                               |
| Runtime profile camera stays `degraded` | Worker stop timeout, missing credentials, storage write failure, or deleted camera UUID | Fix the camera/storage fault or remove stale unavailable targets, save/discard drafts, then select **Reapply active policy**.                                                                                                                                                   |
| Write controls disabled                 | Missing `keeppeek.configuration.v1` or non-Administrator role                           | Check server version and sign in as an Administrator.                                                                                                                                                                                                                           |

For the full schema, defaults, and bounds, see the [configuration reference](./configuration-reference.md) and [configuration engineering guide](https://github.com/xnorpx/keeppeek/blob/main/docs/configuration-management.md).
