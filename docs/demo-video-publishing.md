# Demo Video Publishing

KeepPeek demo videos use Azure Blob Storage or an Azure Storage static website as their canonical
self-updating host. Generated scenario assets keep stable names:

- `assets/<scenario-id>.mp4`
- `assets/<scenario-id>.vtt`
- `assets/<scenario-id>.json`
- `manifest.json`

The publishing workflow uploads all assets first and `manifest.json` last. Existing blobs are
overwritten, so consumers keep stable URLs and never see a manifest that points to an asset that
has not finished uploading.

For example, the URL for a scenario never changes:

```text
https://media.example.com/demos/assets/peek.desktop.live.mp4
```

Every successful generation overwrites that blob. The workflow publishes `manifest.json` after all
new assets exist, then deletes remote scenario files that the new manifest no longer references.
Only the latest generated ground truth remains addressable, and a failed publication leaves the
previous manifest and all of its referenced assets intact.

## GitHub Configuration

Create a `demo-videos` GitHub environment with these secrets:

- `AZURE_CLIENT_ID`
- `AZURE_TENANT_ID`
- `AZURE_SUBSCRIPTION_ID`

Add these environment variables:

- `AZURE_STORAGE_ACCOUNT`
- `AZURE_DEMO_CONTAINER` — use `$web` for an Azure static website
- `AZURE_DEMO_PREFIX` — normally `demos`
- `AZURE_DEMO_BASE_URL` — for example `https://media.example.com/demos`

## Recording Stories

Demo-capable Storybook stories expose typed action, caption, duration, viewport, Paper, and
completion metadata. Real-server stories use the same metadata contract but drive production
routes through a dedicated Playwright configuration. Both paths wait for explicit readiness before
their scenario clock begins.

From `ui/`:

```sh
bun run demo:check
bun run demo:render
```

Record only a static Storybook scenario:

```sh
bun run demo:render:storybook -- peek.desktop.rewind-to-keep
```

Record only the real camera lifecycle:

```sh
bun run demo:render:camera-lifecycle
```

That story starts the real Rust server and deterministic local H.264 RTSP camera, waits for decoded
frames, removes the configured camera through WebRTC control, re-adds the same camera through the
production Settings UI, and verifies the restored row. It does not read or mutate the user's
private KeepPeek configuration.

Generated files are written under `ui/test-results/demo-videos/assets/`. Playwright's VP8 WebM is a
temporary capture only and is deleted after transcoding. Every retained MP4 contains exactly one
H.264/yuv420p video stream; WebVTT captions remain a sidecar rather than an embedded subtitle
stream. The JSON records story/Paper IDs, action timeline, fixture SHA-256, commit SHA, measured
pre-roll, viewport, duration, codec, and stream count. Local and CI generation require Chromium,
ffmpeg, and ffprobe.

Narration remains an optional manual derivative. When a story declares Azure OpenAI narration,
`demo:narrate` and `demo:mux` create an H.264/AAC version that is intentionally outside the default
H.264-only artifact policy. Browser action timing never depends on variable narration duration.

Grant the federated Azure identity **Storage Blob Data Contributor** on the target storage account.
The workflow uses GitHub OIDC; no storage account key is stored in GitHub.

Disable **Blob versioning** and **soft delete for blobs** on this dedicated demo container or storage
account. Otherwise Azure can retain historical blob versions even though the public URL always
serves the newest one. Do not enable immutable retention for this container.

## Automatic Updates

The generator runs for matching changes on `main` and by manual dispatch. It records every entry in
the demo registry, validates media duration, prepares the hosted manifest, and uploads the
`keeppeek-demo-videos` artifact. The publisher runs after `Generate Demo Videos` succeeds on `main`. It downloads the
`keeppeek-demo-videos` artifact, uploads scenario assets with overwrite enabled, and uploads the
new manifest last. A manual dispatch can republish an artifact from a specified workflow run.

The workflow has `contents: read` only. Generated videos and the hosted manifest are never committed
to the repository, so publishing does not create commits or require Git updates.

YouTube can be added as a secondary discovery channel. It cannot replace a video's media while
preserving the same video ID, so an automatic YouTube mirror must upload changed videos as new IDs,
update a playlist, and retain the current IDs in a separate provider manifest. It should not be the
canonical URL source.
