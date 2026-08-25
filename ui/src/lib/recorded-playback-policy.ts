import type { CameraListItem, ProfileSummary } from '$lib/types';

export type RecordedStreamId = 'main' | 'sub';
export type RecordedQualityPreference = 'auto' | 'high' | 'low' | RecordedStreamId;
export type RecordedSelectionReason = 'explicit' | 'preference' | 'automatic' | 'unavailable';

export type RejectedRecordedStream = {
	stream: RecordedStreamId;
	encoding: string;
};

export type RecordedStreamSelection = {
	selectedStream: RecordedStreamId | null;
	fallbackStreams: RecordedStreamId[];
	rejectedStreams: RejectedRecordedStream[];
	reason: RecordedSelectionReason;
};

export type RecordedStreamSelectionOptions = {
	requestedStream?: RecordedStreamId | null;
	preference?: RecordedQualityPreference | null;
	availableStreams?: Iterable<RecordedStreamId>;
	isEncodingSupported?: (encoding: string) => boolean;
};

type RankedProfile = {
	stream: RecordedStreamId;
	profile: ProfileSummary | null;
};

export function selectRecordedStream(
	camera: Pick<CameraListItem, 'profiles'> | null,
	options: RecordedStreamSelectionOptions = {}
): RecordedStreamSelection {
	const profiles = camera?.profiles ?? [];
	const profilesByStream = new Map(profiles.map((profile) => [profile.stream, profile]));
	const availableStreams = uniqueStreams(
		options.availableStreams ??
			(profiles.length > 0 ? profiles.map((profile) => profile.stream) : ['main'])
	);
	const rejectedStreams: RejectedRecordedStream[] = [];
	const candidates = availableStreams
		.map((stream): RankedProfile => ({ stream, profile: profilesByStream.get(stream) ?? null }))
		.filter((candidate) => {
			const encoding = candidate.profile?.encoding?.trim();
			if (!encoding) return true;
			const supported = (options.isEncodingSupported ?? isDefaultEncodingSupported)(encoding);
			if (!supported) rejectedStreams.push({ stream: candidate.stream, encoding });
			return supported;
		})
		.toSorted(compareQualityDescending);

	const explicit = findCandidate(candidates, options.requestedStream);
	if (explicit) return selection(explicit.stream, candidates, rejectedStreams, 'explicit');

	const preference = options.preference;
	if (preference === 'main' || preference === 'sub') {
		const preferred = findCandidate(candidates, preference);
		if (preferred) return selection(preferred.stream, candidates, rejectedStreams, 'preference');
	}
	if ((preference === 'high' || preference === 'low') && candidates.length > 0) {
		const preferred = preference === 'high' ? candidates[0] : candidates.at(-1);
		if (preferred) return selection(preferred.stream, candidates, rejectedStreams, 'preference');
	}

	const automatic = candidates[0];
	if (automatic) return selection(automatic.stream, candidates, rejectedStreams, 'automatic');
	return { selectedStream: null, fallbackStreams: [], rejectedStreams, reason: 'unavailable' };
}

export function preferredRecordedStream(
	camera: Pick<CameraListItem, 'profiles'> | null
): RecordedStreamId {
	return selectRecordedStream(camera).selectedStream ?? fallbackStream(camera?.profiles ?? []);
}

export function browserSupportsRecordedEncoding(encoding: string): boolean {
	if (typeof MediaSource === 'undefined' || typeof MediaSource.isTypeSupported !== 'function') {
		return isDefaultEncodingSupported(encoding);
	}
	const normalized = encoding.trim().toLowerCase();
	if (isAvcEncoding(normalized)) {
		const codec = normalized.startsWith('avc1.') ? normalized : 'avc1.42E01E';
		return MediaSource.isTypeSupported(`video/mp4; codecs="${codec}"`);
	}
	if (isHevcEncoding(normalized)) {
		const codecs =
			normalized.startsWith('hvc1.') || normalized.startsWith('hev1.')
				? [normalized]
				: ['hvc1.1.6.L120.B0', 'hev1.1.6.L120.B0'];
		return codecs.some((codec) => MediaSource.isTypeSupported(`video/mp4; codecs="${codec}"`));
	}
	return false;
}

export function browserSupportsLiveEncoding(encoding: string): boolean {
	const normalized = encoding.trim().toLowerCase();
	const capabilities =
		typeof RTCRtpReceiver === 'undefined' ? null : RTCRtpReceiver.getCapabilities?.('video');
	if (!capabilities) return isDefaultEncodingSupported(normalized);
	const mimeTypes = new Set(capabilities.codecs.map((codec) => codec.mimeType.toLowerCase()));
	if (isAvcEncoding(normalized)) return mimeTypes.has('video/h264');
	if (isHevcEncoding(normalized)) {
		return mimeTypes.has('video/h265') || mimeTypes.has('video/hevc');
	}
	return false;
}

function selection(
	selectedStream: RecordedStreamId,
	candidates: readonly RankedProfile[],
	rejectedStreams: RejectedRecordedStream[],
	reason: RecordedSelectionReason
): RecordedStreamSelection {
	return {
		selectedStream,
		fallbackStreams: candidates
			.filter((candidate) => candidate.stream !== selectedStream)
			.map((candidate) => candidate.stream),
		rejectedStreams,
		reason
	};
}

function findCandidate(
	candidates: readonly RankedProfile[],
	stream: RecordedStreamId | null | undefined
): RankedProfile | undefined {
	return candidates.find((candidate) => candidate.stream === stream);
}

function uniqueStreams(streams: Iterable<RecordedStreamId>): RecordedStreamId[] {
	return [...new Set(streams)];
}

function compareQualityDescending(left: RankedProfile, right: RankedProfile): number {
	const leftResolution = resolutionPixels(left.profile?.resolution);
	const rightResolution = resolutionPixels(right.profile?.resolution);
	return (
		(right.profile?.quality_rank ?? 0) - (left.profile?.quality_rank ?? 0) ||
		rightResolution - leftResolution ||
		(right.profile?.bitrate_kbps ?? 0) - (left.profile?.bitrate_kbps ?? 0) ||
		(right.profile?.framerate ?? 0) - (left.profile?.framerate ?? 0) ||
		Number(right.stream === 'main') - Number(left.stream === 'main')
	);
}

function resolutionPixels(resolution: string | null | undefined): number {
	const match = resolution?.match(/^(\d+)\s*[x×]\s*(\d+)$/i);
	if (!match) return 0;
	return Number(match[1]) * Number(match[2]);
}

function isDefaultEncodingSupported(encoding: string): boolean {
	return isAvcEncoding(encoding.trim().toLowerCase());
}

function isAvcEncoding(encoding: string): boolean {
	return /^(h\.?264|avc|avc1(?:\..+)?)$/.test(encoding);
}

function isHevcEncoding(encoding: string): boolean {
	return /^(h\.?265|hevc|hev1(?:\..+)?|hvc1(?:\..+)?)$/.test(encoding);
}

function fallbackStream(profiles: readonly ProfileSummary[]): RecordedStreamId {
	return (
		profiles.find((profile) => profile.stream === 'main')?.stream ??
		profiles.find((profile) => profile.stream === 'sub')?.stream ??
		'main'
	);
}
