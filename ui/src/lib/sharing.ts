import { ShareGrantStatus } from './proto/webrtc_pb';

export function shareTime(value: bigint): string {
	return (
		new Intl.DateTimeFormat('en-GB', {
			dateStyle: 'medium',
			timeStyle: 'medium',
			timeZone: 'UTC'
		}).format(Number(value)) + ' UTC'
	);
}
export function shareDateInput(value: number): string {
	return new Date(value).toISOString().slice(0, 19);
}
export function shareBytes(value: bigint): string {
	return `${(Number(value) / 1_000_000).toFixed(1)} MB`;
}
export function shareQuota(value: string): number | undefined {
	if (value.trim() === '') return undefined;
	const parsed = Number(value);
	if (!/^\d+$/.test(value) || !Number.isInteger(parsed) || parsed < 1 || parsed > 4_294_967_295)
		throw new Error('Enter positive whole-number limits, or leave them blank for unlimited.');
	return parsed;
}
export function shareExpiry(value: string, horizon: bigint, now = Date.now()): bigint {
	if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(?::\d{2})?$/.test(value))
		throw new Error('Enter a valid UTC expiry time.');
	const parsed = Date.parse(value + 'Z');
	if (
		!Number.isSafeInteger(parsed) ||
		new Date(parsed).toISOString().slice(0, value.length) !== value
	)
		throw new Error('Enter a valid UTC expiry time.');
	if (parsed <= now || parsed - now > 86_400_000 || BigInt(parsed) > horizon)
		throw new Error(
			'Expiry must be in the future, within 24 hours, and no later than file expiry.'
		);
	return BigInt(parsed);
}
export function shareStatus(value: ShareGrantStatus): string {
	switch (value) {
		case ShareGrantStatus.ACTIVE:
			return 'Active';
		case ShareGrantStatus.EXPIRED:
			return 'Expired';
		case ShareGrantStatus.REVOKED:
			return 'Revoked';
		case ShareGrantStatus.LIMIT_REACHED:
			return 'Limit reached';
		default:
			return 'Unavailable';
	}
}
