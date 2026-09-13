# Native camera events

Native events are detections and status changes reported by the camera itself. KeepPeek consumes
Reolink notifications, Hikvision/Annke ISAPI events, generic ONVIF PullPoint notifications and
metadata carried by RTSP. These do not require an external inference service.

Supported observations enter the same event catalog as other events and can appear in **Events**,
Keep's history, notification rules and MQTT. Event processing is independent of media ingest: an
event-service failure does not restart healthy video or audio. Compare its status with
[camera and stream health](./camera-health.md) instead of treating them as one signal.

## Enable and verify events

1. Add the camera and verify its video and recording independently.
2. In the camera's own interface, enable the intended detection rule, schedule, region and event
   notification linkage. KeepPeek discovery does not configure these on the device.
3. Use the default native event mode, `auto`, unless there is a reason to select a specific path.
4. If the camera reports only unclassified motion, enable **Store generic motion events** in its
   KeepPeek settings. The default is off; explicit person or vehicle detections remain distinct.
5. Trigger one known event, then open **Events**, choose that camera and the correct UTC date, and
   inspect its kind, source, timestamp and available image. Verify the corresponding recording in
   Keep before enabling an alert rule.

A detector-enabled switch is not an activity indicator. A camera reporting a motion rule with a
human-only filter also does not prove that every notification contains a person classification.
KeepPeek uses the explicit evidence in each notification.

## Select the event transport

The optional `[cameras.<name>.events]` table belongs in the existing `config.toml`. These transport
fields are file-only; ordinary camera edits preserve them. Apply file changes through the existing
configuration/restart workflow. For example, to force generic ONVIF for an existing camera:

```toml
[cameras.front.events]
mode = "onvif-pullpoint"
metadata_stream = "auto"
snapshots = true
```

Replace `front` with the existing camera entry's name. Do not add a second entry for the same
device merely to change its event path.

| Mode              | Behavior                                                                                                                                                          |
| ----------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `auto`            | Prefer an eligible Reolink or ISAPI adapter; otherwise use generic ONVIF and permit RTSP metadata. Unsupported ISAPI HTTP 404/405 can hand over to generic ONVIF. |
| `vendor`          | Use the eligible vendor adapter without generic fallback.                                                                                                         |
| `onvif-pullpoint` | Use generic ONVIF independently of the video backend; metadata remains permitted unless disabled.                                                                 |
| `rtsp-metadata`   | Consume metadata from the existing RTSP workers without contacting an ONVIF Event Service.                                                                        |
| `disabled`        | Disable native events while leaving camera media independent.                                                                                                     |

`metadata_stream` accepts `auto`, `enabled`, or `disabled`; it does not override `vendor` or
`disabled` mode. Authentication failure is reported rather than bypassed through another
authentication policy. The full field bounds and secret-reference rules are in the
[Configuration reference](./configuration-reference.md).

Multi-channel devices need exact channel evidence. ISAPI main/sub URLs ending in `101/102` select
input 1; `201/202` select input 2. Both URLs must agree. Generic ONVIF `source_tokens` are opaque
camera-supplied identifiers, not profile indexes. Topic filters use expanded XML names, with
exclusions taking precedence. Use the detailed protocol guides when narrowing either field.

## Understand event times and images

Repeated active observations update a logical event instead of creating one event for each
notification. An explicit clear closes the matching observation. Generic ONVIF initialization
establishes a baseline without notifying the user; an already-active baseline must clear before
opening a new interval. Matching PullPoint and metadata evidence is deduplicated.

Timestamp rules depend on the source. Generic ONVIF uses valid camera UTC from the preceding five
minutes; invalid, missing or future notification times use receipt time with an explanation in the
event metadata. ISAPI's recording timeline uses server receipt time and retains the original
camera time as metadata. Correct the device clock when investigating inconsistent event timing.

ISAPI disconnects or a 30-second active-observation timeout close an observed span at its last
evidence. That is not proof of the instant physical motion stopped. Unknown topics and heartbeats
do not become invented motion events.

An event can be useful without an image. KeepPeek attaches explicitly associated snapshots and
retains metadata when image capture is unavailable. Boxes are drawn only on their matching image;
an unavailable canonical image is reported instead of replaced with unrelated footage.

## Troubleshooting

| Symptom                         | What to check                                                                                                                            |
| ------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| Video works, events do not      | Native event status, the device event service, credentials, rule schedule and channel selection. Video and event protocols are separate. |
| Only heartbeats arrive          | Enable the desired camera rule and notification linkage. Check generic-motion retention for unclassified VMD.                            |
| Generic service is unsupported  | Confirm the camera exposes ONVIF events or RTSP metadata; an ONVIF video profile alone is insufficient.                                  |
| Repeated authentication failure | Correct the saved credentials and device permissions. Do not weaken authentication or repeatedly guess passwords.                        |
| Events have no thumbnail        | Inspect the attachment state and snapshot support; metadata delivery can succeed independently.                                          |
| Duplicate-looking cards         | Compare source, event ID, rule, object and revision. Similar text can describe independent observations.                                 |

For protocol details, see [ONVIF events](https://github.com/xnorpx/keeppeek/blob/main/docs/onvif-events.md)
and [Hikvision ISAPI](https://github.com/xnorpx/keeppeek/blob/main/docs/hikvision-isapi.md).
For model-generated detections, see [External analysis](./external-analysis.md). For delivery,
see [Notifications and integrations](./notifications-and-integrations.md).
