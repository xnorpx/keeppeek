# Groups and two-way audio: current status

**Live participant groups are not operational in the current KeepPeek server and browser runtime.** This page records that unavailability boundary, distinguishes the three different uses of "group" across KeepPeek, and separates planned client-to-client participant groups from shipped camera talkback.

## Current status

| Surface / layer                                    | Current state                                                                                               |
| -------------------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| **Settings → Groups** (**Groups & two-way audio**) | Displays **Group directory unavailable**; management actions explain the missing runtime handler.           |
| Generated protobuf contract (`api/`)               | Declares `GroupCommand` (`ListGroups`, `JoinGroup`, `LeaveGroup`), `GroupCapability`, and `GroupState`.     |
| Running server & browser runtime                   | Does **not** handle `GroupCommand`; no group names, participant counts, join sessions, or recordings exist. |
| `config.toml`                                      | No `[groups]` section exists. Do not invent one or infer runtime support from generated types.              |

Consult the [configuration reference](./configuration-reference.md) for all supported `config.toml` sections.

## Three different uses of grouping

| Concept                               | Shipped today | Purpose                                                                            | What it does **not** grant                                     |
| ------------------------------------- | :-----------: | ---------------------------------------------------------------------------------- | -------------------------------------------------------------- |
| Camera group labels and access grants |      Yes      | Organize cameras via TOML namespaces (e.g. `outdoor`) and scope **User** access    | Membership in any voice, walkie-talkie, or conference session  |
| Saved dashboards and their audience   |      Yes      | Share a live-wall layout (`peek_layouts`) with selected viewers                    | Access to cameras outside the viewer's own camera grants       |
| Live participant groups               |      No       | Planned named camera collections with optional live participant audio/video fanout | Any working walkie-talkie, conference, or group admin UI today |

Use [Authentication and access control](./authentication.md#camera-and-dashboard-access) for camera group grants and [Live wall](./live-wall.md) for saved dashboards. Both work independently of live participant groups.

## Camera talkback vs live participant groups

Support for one does not imply support for the other:

| Feature                 | Direction & target                                      | Shipped today | Where to configure / read more          |
| ----------------------- | ------------------------------------------------------- | :-----------: | --------------------------------------- |
| Browser **Talk**        | Administrator microphone → physical camera speaker      |      Yes      | [Camera controls](./camera-controls.md) |
| **Broadcast talkback**  | Administrator microphone → all or camera-group speakers |      Yes      | [Camera controls](./camera-controls.md) |
| Live participant groups | Browser/client ↔ client media fanout via KeepPeek       |      No       | Planned design only (see below)         |

## Planned group design contract

The [group client scenarios](https://github.com/xnorpx/keeppeek/blob/main/docs/groups.md) document the intended protocol contract for a future implementation:

- **Server-owned definitions:** Named collections of static camera streams (never client publications), optional join password (never returned in list/state responses), and optional live audio/video capability. Group definitions are intentionally excluded from `ServerCapabilities`.
- **Full-duplex media without floor control:** All joined participants could publish simultaneously with no moderator or speaking-turn arbitration; push-to-talk would only gate local client microphone capture while keeping the publication warm.
- **Per-participant virtual sources and recording:** Each joined participant would receive a server-assigned virtual `source_id` with independent audio/video recording policy (`DISABLED`, `INHERIT`, `REQUIRED`, `PROHIBITED`) without altering static camera recording policies.

These scenarios specify intended contract behavior only—they are not evidence that the server handler, client capture path, directory, or participant recording pipeline is implemented or qualified.
