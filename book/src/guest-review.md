# Guest review links

Guest review gives a recipient access to a selected recording interval or one completed export
without a KeepPeek account.

## Prepare the deployment

As an Administrator, open **Settings → Access → Guest sharing**. Sharing defaults off. Set
**External HTTPS origin**, enable sharing and save. Use a canonical origin such as
`https://review.example` or `https://review.example:8443`, without a path, credentials, query,
fragment or trailing slash. The policy belongs in the existing `config.toml`:

```toml
[sharing]
enabled = true
public_origin = "https://review.example"
```

Use Settings for live changes. Manual file edits apply on restart; leave the server-owned
revision alone. See the [configuration reference](./configuration-reference.md#guest-review-deployment-policy).

The operator supplies DNS, valid HTTPS, any reverse proxy, firewall rules and recipient network
access. KeepPeek supplies no tunnel or relay and does not verify reachability. Configure
[proxy trust](./authentication.md) explicitly and preserve the expected external Host and origin.

The identity-bound metadata owner and export volume must be available. Grant verifiers and
counters live in a private operational database under that metadata owner, separate from settings.
Supported private local filesystem permissions are required; Windows requires NTFS. Do not
construct or relocate this directory manually. A configuration ZIP does not back up the database.
Keep sharing disabled during metadata recovery: an older backup is not current revocation evidence.

## Share a completed export

1. In **Keep → Export**, create an export as the Administrator who will share it. Wait for
   **Ready**, then choose **Create guest link**. Older exports without durable creator and camera
   identity or measured media proof require a new export.
2. Inspect the camera, file bounds, gaps, size and file expiry. The grant covers the complete
   previewed MP4, which can start before the originally requested range to include a keyframe.
   This workflow does not trim an exact interval. Evidence is limited to two minutes and 512 MiB;
   guest media admission accepts H.264 with supported AAC audio.
3. Set a display name, optional private note, expiry, playback/download permissions and any positive
   usage limits. Expiry is at most 24 hours and cannot exceed the export's expiry. Download defaults
   off. Review these choices, then create the link.
4. Copy the link while the dialog remains open and send it through your chosen private channel.
   KeepPeek holds the secret in the dialog's memory. After closing, the list can revoke access
   but cannot recover the link. Anyone holding the full link can use its permitted access.

This first workflow requires a retained local export with a file expiry. Archive-only exports
are outside this workflow.

If creation is unconfirmed, retain its displayed reference. Find and revoke that grant in
**Settings → Access → Guest links** before creating another. A retry cannot recover a lost secret.

## Share a recording range

As an Administrator, select **Share recording range** in **Keep → Export**, in an event's
detail, or beside a saved bookmark. Event and bookmark actions use the event's recorded
start and end, without adding surrounding footage. An open event or missing recording
cannot supply a bounded interval. Older recorders require an update to show the action.

KeepPeek prepares a locally retained export for the selected camera and stream, or
reuses a matching export you own. Review both the selected interval and the measured file
bounds. Gaps and file expiry appear before creation. If a keyframe, audio sample or
trailing frame would extend outside the interval, KeepPeek declines the range. You can
choose new bounds explicitly in Keep. The two-minute and 512 MiB evidence limits and
the playback, download and expiry settings apply to range links too.
For an event or bookmark the recorder checks which streams have footage inside the selected
interval. Keep preserves the stream you selected. A nonzero recorded-audio correction prevents
range sharing because a sample's original time can differ from its presentation time in the file.
**Reload preview** checks the same export job and displays its failure reason when preparation
failed. **Prepare a fresh export** explicitly starts another attempt using the same stream and
interval. This can replace a legacy file that lacks source proof or has an older audio snapshot.
Events and bookmarks first try the latest-expiring matching ready file, including when the
original recording has expired. When that stream has incompatible video, crosses an interval
edge or uses corrected audio, an available alternate stream can supply the same interval.
Keep preserves the stream selected by the operator.

## What the guest receives

An isolated page removes the secret fragment before the application loads and uses a separate
Secure, HttpOnly guest cookie. Guests can play and seek the file, and download it when permitted.
Browser codec support is still required. **End review** ends the session; it does not revoke the link.

Guests receive no KeepPeek role or control channel, live wall, other footage, events/search,
snapshots, camera controls, talk, settings, health or logs. A second guest tab cannot switch an
existing tab to different evidence through the shared cookie.

The first successful media-byte delivery commits a view durably; seeks in that session reuse it.
Downloads count separately per started transfer. Opening the page or failed validation consumes
no view. A crash can conservatively count delivery before bytes reach the recipient; committed
use is never refunded. Sessions last at most five minutes; media also stops at grant expiry.

## Revoke and recover

Use **Guest links** to inspect counters and revoke access with a reason. Sharing creates no
retention hold: deletion, retention, missing storage or a replaced export can end availability
sooner. Camera removal, active privacy and an invalid creator issuer revision or Administrator
role also prevent delivery. The source binding identifies a configured camera incarnation;
it does not prove historical physical hardware identity.

Policy changes end active sessions. An uncertain settings write suspends guest access: reload
the policy and save the desired settings successfully. Camera settings reload does not clear
that suspension. Restart ends sessions and transfers while preserving grants, revocations and
committed counts. Starting again requires the original valid link and remaining usage allowance.

Limits are 256 active grants, 32 per creator, eight media sessions globally and two per grant.
When a request comes through a proxy in `[access].trusted_proxies`, the per-address request and
egress budgets use the client address from `X-Forwarded-For`. For all other requests, the budgets
use the connection address and ignore forwarding headers.
KeepPeek retains at most 4,096 grant records and rejects creation when the history is full.
Expired or revoked grants disappear from lists 30 days after the earlier of expiry and revocation.
The running server deletes overdue records in batches of 256. When the server restarts after
downtime, it removes every overdue record before opening the guest store for access.
