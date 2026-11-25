import type { CameraUpdated, ServerCapabilities } from './proto/webrtc_pb';

export function replaceKnownCamera(
	capabilities: ServerCapabilities | null,
	update: CameraUpdated
): ServerCapabilities | null {
	if (!capabilities || !update.sourceId || update.camera?.sourceId !== update.sourceId) return null;
	const index = capabilities.cameras.findIndex((camera) => camera.sourceId === update.sourceId);
	if (index === -1) return null;
	return { ...capabilities, cameras: capabilities.cameras.with(index, update.camera) };
}
