import { LiveQualitySelectionReason } from './proto/webrtc_pb';

export function liveVariant(value: string | undefined): 'main' | 'sub' | null {
	return value === 'main' || value === 'sub' ? value : null;
}

export function safeBitrate(bitsPerSecond: bigint | undefined): number | null {
	if (bitsPerSecond === undefined) return null;
	const estimate = Number(bitsPerSecond);
	return Number.isSafeInteger(estimate) && estimate >= 0 ? estimate : null;
}

const reasonLabels: Record<LiveQualitySelectionReason, string> = {
	[LiveQualitySelectionReason.UNSPECIFIED]: 'Unavailable',
	[LiveQualitySelectionReason.MANUAL]: 'Manual quality choice',
	[LiveQualitySelectionReason.FIXED_DELIVERY]: 'Fixed delivery',
	[LiveQualitySelectionReason.BANDWIDTH_UNKNOWN]: 'Awaiting network estimate',
	[LiveQualitySelectionReason.INSUFFICIENT_BANDWIDTH]: 'Insufficient bandwidth',
	[LiveQualitySelectionReason.STABLE]: 'Stable selection',
	[LiveQualitySelectionReason.UPGRADE_HOLD]: 'Waiting for stable budget',
	[LiveQualitySelectionReason.DOWNGRADE_HOLD]: 'Lower network budget',
	[LiveQualitySelectionReason.WAITING_KEYFRAME]: 'Waiting for keyframe',
	[LiveQualitySelectionReason.SINGLE_VARIANT]: 'Only one compatible variant',
	[LiveQualitySelectionReason.VARIANT_UNAVAILABLE]: 'Variant unavailable'
};

export function liveQualityReason(reason: LiveQualitySelectionReason | undefined): string {
	return reason === undefined ? 'Unavailable' : (reasonLabels[reason] ?? 'Unavailable');
}

export function liveVariantLabel(value: string | undefined, empty: string): string {
	if (value === '') return empty;
	const variant = liveVariant(value);
	return variant === null ? 'Unavailable' : variant === 'main' ? 'Main' : 'Sub';
}
