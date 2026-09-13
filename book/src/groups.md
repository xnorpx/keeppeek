# Groups and two-way audio: current status

**Live participant groups are not operational in the current server and browser runtime.**
Settings includes **Groups & two-way audio**, but its directory reports unavailable. The generated
protocol defines list, join, and leave messages; the running implementation does not handle those
commands. There are no real group names, participant counts, join sessions, or participant
recording states to display. Creation and administration controls explain this limitation.

Do not add an invented `[groups]` section to `config.toml` or infer support from generated types.
The [configuration reference](./configuration-reference.md) lists the supported settings.

## Three different uses of grouping

| Concept                               | Current purpose                                                                                                             | What it does not grant                                                      |
| ------------------------------------- | --------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------- |
| Camera group labels and access grants | Organize cameras and define which cameras an identity may access.                                                           | Membership in a voice or conference session.                                |
| Saved dashboards and their audience   | Share a camera layout and its wall presentation settings with selected viewers.                                             | Permission to see cameras outside the viewer's grants.                      |
| Live participant groups               | A protocol and design model for named camera collections with optional participant audio/video. The runtime is unavailable. | A working walkie-talkie, conference, or group administration feature today. |

Use [Authentication and access control](./authentication.md) for camera grants and
[Live wall](./live-wall.md) for saved layouts. These implemented features work independently of
live participant groups.

## What the group design describes

The [group client scenarios](https://github.com/xnorpx/keeppeek/blob/main/docs/groups.md) describe
server-owned definitions with static camera stream members, optional passwords, and optional live
participants. Participant media would have a server-assigned identity and its own stream and
recording policy. Camera membership would not change a camera's ordinary recording policy.

The design is full duplex: it has no floor control, moderator, or server-enforced speaking turn.
Push-to-talk means a client gates its own microphone while keeping its publication ready. It is
not a server command or a promise that the current browser includes a working radio console.

Those scenarios explain the intended contract, including reconnect and per-participant recording
behavior. They are not evidence that the handler, client capture, directory, or recording path has
been implemented and qualified. A future implementation needs runtime and device evidence before
this chapter can provide operational setup instructions.

## Camera talkback is separate

Talkback sends audio to a supported physical camera. A live participant group would exchange media
between clients through KeepPeek. Support for one does not imply support for the other.

The current browser Talk control is unavailable. Consult [Camera controls](./camera-controls.md)
for supported physical camera operations and the availability reason shown for the selected device.
