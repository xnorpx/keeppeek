import type { CameraListItem } from './types';

export type CameraControlPresentation = {
	showPtz: boolean;
	commandAvailable: boolean;
	reason: string | null;
};

export function presentCameraControl(
	camera: CameraListItem,
	commandTransportAvailable: boolean
): CameraControlPresentation {
	if (camera.capabilities?.ptz !== true) {
		return { showPtz: false, commandAvailable: false, reason: null };
	}
	if (!commandTransportAvailable) {
		return {
			showPtz: true,
			commandAvailable: false,
			reason: 'PTZ reported · browser control transport unavailable'
		};
	}
	return { showPtz: true, commandAvailable: true, reason: null };
}
