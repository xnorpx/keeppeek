# External analysis

An external analysis service consumes KeepPeek camera video, runs its own detector or model, and
publishes events back through the public API. KeepPeek stores accepted event revisions and
attachments and makes them available in Events, Keep, notifications and integrations.

KeepPeek does not install, host or select an inference product for you. Model execution, decoding,
sampling, deployment and model licensing belong to the independent service. The repository's
Python object-detection example demonstrates the integration and supports CI; it is not a
production detector product. Camera-native detections are covered separately in
[Native camera events](./native-camera-events.md).

## Prepare the integration

1. Verify that the camera is available in KeepPeek and that its configured stream can be decoded
   by the service. A low-resolution stream is usually sufficient for an initial interoperability
   check; choose it explicitly in the service.
2. Obtain the camera's stable source ID from its camera details. A display name, a current session
   ID or a Home Assistant entity ID is not a substitute.
3. Provide a dedicated KeepPeek credential through the service's supported secret mechanism.
   Event publication currently requires Administrator authority; a viewing-only User key cannot
   publish events. Apply the [access policy](./authentication.md) appropriate to that service.
4. Confirm the server advertises `keeppeek.event-publication.v1`, the intended source, a supported
   media variant and the event types the service will publish. An unadvertised event type is not
   accepted merely because a model can produce it.
5. Start the service and verify one committed event before depending on its alerts or automation.

The browser has no general model-installation or external-service deployment wizard. Configuration
for a particular service belongs to that client; do not invent extra KeepPeek TOML sections from
the conceptual computer-vision scenario.

## Try the reference example

The [object-detection example](https://github.com/xnorpx/keeppeek/tree/main/examples/object_detection_service)
includes the complete platform-specific setup and commands. It requires Python 3.12 or newer,
FFmpeg on `PATH`, an active H.264 or H.265 source and a compatible KeepPeek server. Install its
requirements into the selected interpreter directly; the repository does not use Python virtual
environments. Generate its bindings from the checked-in protocol before running it.

Use `KEEPPEEK_ACCESS_KEY_FILE` with an owner-only key file or the supported environment variable
for authentication. Keep the key out of command arguments, endpoint URLs and committed files.
The example accepts non-secret settings including `KEEPPEEK_URL`, `KEEPPEEK_SOURCE_ID`, stream,
model, confidence, cooldown and inference-frame-rate choices.

The default example loads Ultralytics `yolo11n.pt` after authentication and media subscription
succeed. Missing weights may be downloaded by Ultralytics. Review that provider's licensing and
deployment requirements independently. KeepPeek does not distribute the model weights.

## Verify the result in KeepPeek

Open **Events** and select the source and UTC date. Inspect the event kind, confidence, text,
revision and canonical image. A later model result may enrich or close the same event; it should
not be interpreted as a new detection solely because its revision increased.

Use **Open at this moment** to compare the event with recorded video, and **Back to event** to
return to the investigation. Review state and shared bookmarks work the same way for external
and native events. A JPEG, story or description is an event attachment; it does not prove that
continuous video was retained. Check [Recording and evidence](./recording-and-evidence.md).

An attachment upload becomes visible only after the publication commits. A failed or unfinished
upload must not appear as a complete event. Consumers use stable source/event IDs and revisions
to recognize updates and retries. The service must use the API rather than writing KeepPeek's
catalog or attachment directories directly.

## Failure and capacity checks

| Symptom                                   | What to check                                                                                                                  |
| ----------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| Subscription is rejected                  | Source visibility, advertised stream/codec and the service's media capability checks.                                          |
| Publication is denied                     | Administrator authority, enabled credential, current source identity and advertised event type.                                |
| Processing falls behind                   | Service decode/inference rate and bounded queues. Reduce sampling or use a suitable stream instead of accumulating old frames. |
| Events appear without images              | Publication/attachment completion and image limits; inspect the event's explicit availability state.                           |
| Recording continues while detections stop | Check the external process and its session. Inference failure does not imply recording failure.                                |
| Reconnection produces duplicates          | Preserve logical event identity and increment revisions correctly; inspect commit/retry handling in the service.               |

Source restarts change session identity. A service must rediscover the current source session and
replace its media subscription, while preserving the stable source ID used for event history.
The reference example's checks demonstrate interoperability, not long-duration operation or
certification of a third-party model deployment.

Developers should start with the [public API](https://github.com/xnorpx/keeppeek/tree/main/api)
and the [computer-vision scenario](https://github.com/xnorpx/keeppeek/blob/main/docs/computer-vision.md).
The latter explains the wider service architecture; check advertised capabilities and current
examples before treating a described workload as an installed KeepPeek feature.
