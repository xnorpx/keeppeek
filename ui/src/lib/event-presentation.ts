import type { EventIconKey, RecordingEvent, RecordingEventAttachment } from './types';

const iconKeys: ReadonlySet<string> = new Set<EventIconKey>([
	'event',
	'person',
	'vehicle',
	'animal',
	'package',
	'motion',
	'doorbell',
	'sound',
	'story',
	'alert'
]);
const imageTypes = ['snapshot', 'story-frame', 'thumbnail'] as const;
const imageContentTypes: ReadonlySet<string> = new Set(['image/jpeg', 'image/png', 'image/webp']);

export function canonicalEventAttachment(
	attachments: readonly RecordingEventAttachment[],
	explicitId?: string | null
): RecordingEventAttachment | null {
	const ids = new Set<string>();
	if (attachments.some((attachment) => ids.has(attachment.id) || !ids.add(attachment.id))) {
		return null;
	}
	if (explicitId) {
		const attachment = attachments.find((candidate) => candidate.id === explicitId);
		return attachment && isSupportedEventImage(attachment) ? attachment : null;
	}
	for (const type of imageTypes) {
		const attachment = attachments
			.filter((candidate) => candidate.type === type && isSupportedEventImage(candidate))
			.toSorted(compareAttachments)[0];
		if (attachment) return attachment;
	}
	return null;
}

export function isSupportedEventImage(attachment: RecordingEventAttachment): boolean {
	return (
		imageTypes.some((type) => type === attachment.type) &&
		imageContentTypes.has(attachment.content_type)
	);
}

export function eventIconKey(
	producerKey: string | null | undefined,
	eventType: string
): EventIconKey {
	if (producerKey && iconKeys.has(producerKey)) return producerKey as EventIconKey;
	const normalizedType = eventType.trim().toLocaleLowerCase('en-US');
	if (['person', 'human', 'face'].includes(normalizedType)) return 'person';
	if (['vehicle', 'car', 'truck'].includes(normalizedType)) return 'vehicle';
	if (['animal', 'pet'].includes(normalizedType)) return 'animal';
	if (normalizedType === 'package') return 'package';
	if (normalizedType === 'motion') return 'motion';
	if (normalizedType === 'doorbell') return 'doorbell';
	if (['sound', 'audio'].includes(normalizedType)) return 'sound';
	if (normalizedType === 'story') return 'story';
	if (normalizedType.includes('outage') || normalizedType.includes('unavailable')) return 'alert';
	return 'event';
}

export function eventOwnsCanonicalBoundingBox(event: RecordingEvent): boolean {
	return (
		event.bbox !== null &&
		event.canonical_attachment_id != null &&
		event.bbox_attachment_id === event.canonical_attachment_id
	);
}

export function orderedEventAttachments(event: RecordingEvent): RecordingEventAttachment[] {
	const attachments = event.attachments ?? [];
	const canonical = canonicalEventAttachment(attachments, event.canonical_attachment_id);
	const remaining = attachments
		.filter((attachment) => attachment !== canonical)
		.toSorted(compareAttachments);
	return canonical ? [canonical, ...remaining] : remaining;
}

function compareAttachments(
	left: RecordingEventAttachment,
	right: RecordingEventAttachment
): number {
	if (left.ordinal !== right.ordinal) return left.ordinal - right.ordinal;
	const leftTimestamp = left.timestamp_ms ?? Number.POSITIVE_INFINITY;
	const rightTimestamp = right.timestamp_ms ?? Number.POSITIVE_INFINITY;
	if (leftTimestamp !== rightTimestamp) return leftTimestamp - rightTimestamp;
	return left.id < right.id ? -1 : left.id > right.id ? 1 : 0;
}
