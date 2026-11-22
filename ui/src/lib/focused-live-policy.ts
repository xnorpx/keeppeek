import type { FocusedLivePreference } from './playback-preferences';
import type { CameraListItem } from './types';
import type { LivePeerTrack } from './stream-peer.svelte';
import {
	browserSupportsLiveEncoding,
	selectRecordedStream,
	type RecordedStreamId
} from './recorded-playback-policy';
import { videoResolutionMatches } from './video-resolution';

type FocusedAutoOptions = {
	preference: FocusedLivePreference;
	previewPresented: boolean;
	fallbackStream: RecordedStreamId | null;
	isEncodingSupported?: (encoding: string) => boolean;
};

export function canUseFocusedNetworkAuto(
	camera: Pick<CameraListItem, 'profiles'> | null,
	options: FocusedAutoOptions
): boolean {
	if (
		!camera ||
		!options.previewPresented ||
		options.preference !== 'auto' ||
		options.fallbackStream !== null
	)
		return false;
	const profiles = camera.profiles;
	if (
		profiles.length === 0 ||
		profiles.some(
			(profile) =>
				(profile.stream !== 'main' && profile.stream !== 'sub') || !profile.encoding?.trim()
		)
	)
		return false;
	const advertised = new Set(profiles.map((profile) => profile.stream));
	if (advertised.size !== profiles.length) return false;
	const compatible = selectRecordedStream(camera, {
		isEncodingSupported: options.isEncodingSupported ?? browserSupportsLiveEncoding
	});
	return (
		compatible.selectedStream !== null &&
		compatible.rejectedStreams.length === 0 &&
		compatible.fallbackStreams.length + 1 === advertised.size
	);
}

export type FocusedLiveFrame = {
	width: number;
	height: number;
	stream: RecordedStreamId;
	status: LivePeerTrack['status'];
};

export function matchesFocusedLiveFrame(
	track: Pick<LivePeerTrack, 'requestedVariantId' | 'activeStream' | 'pendingStream'>,
	frame: FocusedLiveFrame,
	expected: { automatic: boolean; stream: RecordedStreamId; resolution: string | null | undefined }
): boolean {
	return (
		track.requestedVariantId === (expected.automatic ? null : expected.stream) &&
		frame.status === 'live' &&
		track.pendingStream === null &&
		track.activeStream === expected.stream &&
		frame.stream === expected.stream &&
		videoResolutionMatches(expected.resolution, frame.width, frame.height)
	);
}
