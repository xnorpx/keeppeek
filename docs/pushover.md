# Pushover notifications

KeepPeek can deliver event and health notification rules through the official Pushover Messages
API. Delivery runs on the server-owned notification worker and does not block camera ingest,
recording, event storage, or browser delivery.

## Setup

1. Register a Pushover application at <https://pushover.net/apps/build> and copy its 30-character
   application token.
2. Copy the 30-character user key from the Pushover dashboard, or use a delivery-group key.
3. In **Settings > Notifications**, create or edit a rule and add a **Push** action.
4. Enter the application token and user or group key. Optionally set device names, sound, priority,
   and the public base URL used to resolve KeepPeek event links.
5. Save and activate the rule, then use its test action. The delivery history reports the provider
   request ID and status without returning either credential.

Application tokens and user or group keys are write-only. KeepPeek accepts them over the
authenticated control connection, stores them in the server-owned notification database, and
replaces them in every API response with a configured marker and an opaque reference. They are not
included in health snapshots, delivery reasons, logs, or browser-readable responses. Changing only
non-secret settings preserves the stored credentials; replacing credentials requires entering both
values again.

## Supported fields

| Setting            | Behavior                                                                              |
| ------------------ | ------------------------------------------------------------------------------------- |
| User or group key  | Sends to one Pushover user or delivery group.                                         |
| Devices            | Optional comma-separated device names; blank targets all eligible devices.            |
| Sound              | Optional built-in or account custom sound name; blank uses the account default.       |
| Priority           | Supports `-2`, `-1`, `0`, `1`, and emergency priority `2`.                            |
| Emergency retry    | Required for priority `2` and at least 30 seconds.                                    |
| Emergency expiry   | Required for priority `2`, between 1 and 10,800 seconds.                              |
| Deep-link base URL | Optional public HTTP(S) origin used to resolve relative KeepPeek event links.         |
| Attachment         | Sends one JPEG when the rule permits it and the file is within the rule's byte limit. |

Messages include the rule-rendered title and body, the source event timestamp, and a supplementary
KeepPeek deep link when it can be represented as an absolute HTTP(S) URL. Templates can use
`{{source.name}}` for the configured camera name and `{{source.id}}` for its stable source ID. Titles
are limited to 250 characters, messages to 1,024 characters, and deep links to 512 characters.
KeepPeek's rule limit for attachments is 4 MiB, below Pushover's 5 MiB provider limit. An
unavailable or oversized image is omitted unless the action requires an attachment, in which case
the attempt fails without sending metadata alone. Local attachment paths are never sent or
returned.

## Delivery and failures

Pushover requests use HTTPS, TLS certificate verification, a five-second timeout, no redirects,
and one sequential delivery worker. Successful responses must contain Pushover's success status and
request ID. Emergency responses must also contain a valid receipt.

HTTP 408, 425, 429, 5xx responses, timeouts, and connection failures enter the rule's durable,
bounded retry policy. Provider retries wait at least five seconds and remain bounded by the rule's
maximum attempts, retry interval, and outbox expiry. Other 4xx responses, malformed credentials,
invalid destinations, and malformed provider responses fail permanently. Provider response bodies
are not copied into delivery history because they may contain submitted values.

For priority `2`, KeepPeek polls the receipt API no faster than every five seconds. History shows
`pending`, `acknowledged`, `expired`, or `failed`; acknowledgement time and a hash of the
acknowledging identity are retained. The raw receipt and acknowledging user key remain server-only.
KeepPeek inbox acknowledgement is separate from Pushover acknowledgement.

Disabling a rule prevents all of its future matches. Disabling only the Push action keeps the rule
and its other actions active but creates no new Pushover outbox work. Pending or retrying work for
the disabled rule or action is expired before activation completes. Existing delivery and
acknowledgement history remains available.

Pushover applies account message quotas and may return HTTP 429 when a quota is exhausted. Consult
the provider dashboard and <https://pushover.net/api> for current quota, sound, device, and account
behavior.
