# Guest review links

Guest review gives a recipient access to one completed export without a KeepPeek account.
This chapter describes the first workflow under development. End-to-end HTTPS, resource and
performance qualification remains open; full guest-sharing release readiness is not established.

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
   identity evidence require a new export.
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

If creation is unconfirmed, retain its displayed reference. Find and revoke that grant in
**Settings → Access → Guest links** before creating another. A retry cannot recover a lost secret.

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
Retained history is capped at 4096 records; creation is rejected when full. Expired/revoked metadata
leaves lists after 30 days, with bounded physical cleanup. Exact-range derivatives, all four
creator entry points, measured revoke latency and full HTTPS/performance qualification remain
tracked implementation work.
