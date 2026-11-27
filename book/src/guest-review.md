# Guest review links

This page covers creating, opening, and revoking bounded guest review links on `/share`, deployment prerequisites in **Settings → Access → Guest sharing**, management in **Settings → Access → Guest links**, transport peer classification, and security/rate limits. Guest links grant temporary access to one completed export or one bounded recording range without a KeepPeek account.

## Video walkthrough

Watch enabling the guest sharing HTTPS policy, creating a bounded guest link, opening the isolated `/share` review page, and revoking the grant:

<video controls crossorigin="anonymous" preload="metadata" playsinline style="width: 100%; height: auto">
  <source src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/share.desktop.guest-review.mp4" type="video/mp4">
  <track src="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/share.desktop.guest-review.vtt" kind="captions" srclang="en" label="English">
  <a href="https://keeppeekdemos.blob.core.windows.net/demo-videos/demos/assets/share.desktop.guest-review.mp4">Download the guest review link demo</a>.
</video>

## Prepare the deployment

Guest sharing defaults **off**. Before creating links, an Administrator must configure the external HTTPS origin and storage prerequisites:

1. Open **Settings → Access → Guest sharing**, set **External HTTPS origin**, enable sharing, and save. Use a canonical HTTPS origin such as `https://review.example` or `https://review.example:8443` (no path, credentials, query, fragment, or trailing slash).
2. Supply DNS, valid HTTPS termination, reverse proxy, firewall rules, and recipient reachability. KeepPeek provides no tunnel or relay and does not verify external reachability.
3. Configure `[access].trusted_proxies` explicitly if behind a reverse proxy and preserve the expected external `Host` and `Origin` headers. See [Authentication and access control](./authentication.md) and [Guest review deployment policy](./configuration-reference.md#guest-review-deployment-policy).

Manual edits to `[sharing]` in `config.toml` apply on restart (do not edit the server-owned revision field); use Settings for live changes.

**Operational database boundary:** Grant verifiers, revocations, and usage counters live in a private operational database under the identity-bound metadata owner (separate from `config.toml`, requiring private local filesystem permissions and NTFS on Windows). Never construct or relocate this directory manually. Configuration ZIP exports do not include this database; keep sharing disabled during metadata recovery because an older backup is not current revocation evidence.

## Create a guest review link

KeepPeek supports two creation paths, both capped at `2 minutes` and `512 MiB` and requiring H.264 video with supported AAC audio:

### Option A: Share a completed export

1. In **Keep → Export**, create an export as the Administrator who will share it, wait for **Ready**, and select **Create guest link**. (Requires a retained local export with file expiry, creator/camera identity, and measured media proof; archive-only and legacy exports require creating a fresh export.)
2. Inspect camera, measured file bounds (which may start before the requested range to include a leading keyframe), gaps, byte size, and file expiry. This path shares the entire previewed MP4 without trimming.
3. Set display name, optional private note, expiry (`<= 24 hours` and `<=` export expiry), playback/download permissions (**Download** defaults **off**), and any positive usage limits, then create the link.
4. Copy and send the link while the dialog is open. KeepPeek holds the secret only in dialog memory—after closing, **Guest links** can revoke the grant but never recover the secret.

If creation times out or is unconfirmed, retain the displayed reference and check/revoke that grant in **Settings → Access → Guest links** before creating another; retrying cannot recover a lost secret.

### Option B: Share a recording range

1. Select **Share recording range** from **Keep → Export**, an event's detail drawer, or beside a saved bookmark. Event and bookmark actions use the event's exact recorded start and end without padding (open events and missing recordings cannot supply a bounded interval).
2. KeepPeek reuses a matching ready export you own or prepares a new local export for the camera and stream. Review both the selected interval and measured file bounds, gaps, and expiry.
   - KeepPeek declines the range if any keyframe, audio sample, or trailing frame extends outside the selected interval, or if the stream has a nonzero recorded-audio correction. Adjust bounds in Keep when needed.
   - In Keep, your selected stream is preserved. From events or bookmarks, KeepPeek tries the latest-expiring matching ready file first (even if the original recording expired) and falls back to an available alternate stream if the primary stream has incompatible video, crosses a boundary, or uses corrected audio.
   - Use **Reload preview** to inspect a failed preparation job, or **Prepare a fresh export** to start a new attempt (for example, to replace a legacy file lacking source proof).
3. Configure permissions, expiry, and usage limits, create the link, and copy it immediately before closing the dialog.

## What the guest receives (`/share`)

Opening the link loads the isolated `/share` page, which strips the secret URL fragment before the application initializes and exchanges it via `/guest/session` for a scoped `Secure`, `HttpOnly` cookie (`__Host-keeppeek-guest`).

| Guest capability / boundary | Behavior                                                                                                                                                                                                                                       |
| --------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Allowed actions**         | Stream and seek the shared MP4 (requires browser H.264/AAC decode), **Download MP4** when permitted, and **End review** (ends session without revoking link).                                                                                  |
| **Denied access**           | No KeepPeek account, role, control channel, live wall, other recordings, events/search, snapshots, camera controls, Talk, settings, health, or logs.                                                                                           |
| **Tab isolation**           | A second guest tab cannot switch an existing tab to different evidence via the shared cookie.                                                                                                                                                  |
| **Usage accounting**        | First delivered media bytes durably commit `1` view (seeks in that session reuse it); downloads count per started transfer. Page loads and failed checks consume `0` uses; crashes count conservatively and committed uses are never refunded. |
| **HTTP boundary checks**    | `/share` and `/guest/*` require HTTPS, exact configured `Host` and `Origin`, reject `Authorization` headers, and enforce bounded request sizes.                                                                                                |

## Revoke, invalidation, and restart behavior

Inspect usage counters and revoke links with a reason in **Settings → Access → Guest links**.

- **No retention hold:** Storage retention, manual deletion, missing storage, or a replaced export can end media availability before grant expiry.
- **Immediate invalidation triggers:** Camera removal, active camera privacy, creator issuer revision or Administrator role loss, or any sharing policy change ends active sessions and blocks delivery. Source bindings track a configured camera incarnation, not historical physical hardware identity.
- **Uncertain policy write:** Suspends guest access until an Administrator reloads the policy and saves cleanly (reloading camera settings does not clear this suspension).
- **Server restart:** Terminates active sessions and transfers while preserving grants, revocations, and committed counters. Reopening requires the original link and remaining usage allowance.

## Limits, peer classification, and rate budgets

| Scope                                                               | Hard limit                                                                                                                                                             |
| ------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Active grants                                                       | `256` globally, `32` per creator                                                                                                                                       |
| Grant history & cleanup                                             | `4,096` retained records (creation rejected when full); purged `30 days` after earlier of expiry/revocation (`256`/batch while running; all overdue purged on startup) |
| Evidence size & duration                                            | `<= 512 MiB`, `<= 2 minutes`, H.264 + supported AAC                                                                                                                    |
| Grant & session lifetimes                                           | Grant `<= 24 hours` (capped by export expiry); session `<= 5 minutes` (stops at expiry)                                                                                |
| Concurrent sessions                                                 | `8` globally, `2` per grant (`2` concurrent transfers per session)                                                                                                     |
| Egress bandwidth pacing                                             | `16 MiB/s` per classified peer address, `64 MiB/s` globally                                                                                                            |
| Request budgets (`60 s` window, up to `128` tracked peer addresses) | Exchange: `20` / address, `120` global · Status: `240` / `1,200` · Media: `120` / `480` · Download: `12` / `48`                                                        |

**Transport peer classification:** When a guest request arrives through a proxy listed in `[access].trusted_proxies`, per-address request and egress budgets use the classified client IP from `X-Forwarded-For`. For all other requests, KeepPeek uses the direct transport peer address and ignores forwarding headers.
