import { expect, test, type Page, type Locator } from '@playwright/test';
import { create, toBinary } from '@bufbuild/protobuf';
import {
	ControlEnvelopeSchema,
	NotificationSchema,
	SubscriptionStreamStateSchema,
	VideoQuality
} from '../src/lib/proto/webrtc_pb';
import { mockControlPeer } from './fixtures/control-peer';
import { mixedCameras } from './fixtures/peek';
import { presentMockVideoFrame } from './fixtures/media';
import type { ProfileSummary } from '../src/lib/types';

const main: ProfileSummary = {
	name: 'Main',
	stream: 'main',
	encoding: 'h264',
	resolution: '1920x1080',
	framerate: 25
};
const sub: ProfileSummary = {
	name: 'Sub',
	stream: 'sub',
	encoding: 'h264',
	resolution: '640x360',
	framerate: 15
};

async function focusedFixture(page: Page, profiles: ProfileSummary[]) {
	const camera = { ...mixedCameras[0], profiles };
	const requests = await mockControlPeer(page, {
		cameras: [camera],
		health: {
			status: 'healthy',
			cameras: [
				{ id: camera.id, state: 'healthy', lifecycle: 'Connected', last_error: null, streams: [] }
			]
		}
	});
	await page.goto('/viewer?camera=front-door');
	const focus = page.getByRole('region', { name: 'Front Door focus' });
	const live = focus.locator('[data-peek-focus-stage] [data-camera-id="front-door"]');
	await expect.poll(() => requests.mediaSubscriptions.length).toBe(1);
	return { requests, focus, live };
}

async function presentAdmittedFrame(page: Page, live: Locator, stream: 'main' | 'sub') {
	const envelope = create(ControlEnvelopeSchema, {
		message: {
			case: 'notification',
			value: create(NotificationSchema, {
				event: {
					case: 'subscriptionStreamState',
					value: create(SubscriptionStreamStateSchema, {
						subscriptionId: 'camera-0',
						activeVariantId: stream
					})
				}
			})
		}
	});
	await page.evaluate(
		(bytes) => {
			(
				window as unknown as { pushKeepPeekCapabilities(payload: number[]): void }
			).pushKeepPeekCapabilities(bytes);
		},
		Array.from(toBinary(ControlEnvelopeSchema, envelope))
	);
	await expect(live).toHaveAttribute('data-stream', stream);
	await presentMockVideoFrame(
		live.locator('video'),
		stream === 'main' ? 1920 : 640,
		stream === 'main' ? 1080 : 360
	);
}

test('focused Auto delegates native selection only after its admitted preview and preserves explicit choices', async ({
	page
}) => {
	let creates = 0;
	page.on('request', (request) => {
		if (request.method() === 'POST' && request.url().endsWith('/create')) creates += 1;
	});
	const { requests, focus, live } = await focusedFixture(page, [main, sub]);
	expect(requests.mediaSubscriptions[0]).toMatchObject({
		variantId: 'sub',
		videoQuality: VideoQuality.AUTO
	});
	const session = await live.getAttribute('data-session-id');
	const initialCreates = creates;
	const video = await live.locator('video').elementHandle();
	await presentMockVideoFrame(live.locator('video'), 1920, 1080);
	await expect(live).toHaveAttribute('data-status', 'live');
	expect(requests.mediaSubscriptions).toHaveLength(1);
	await presentAdmittedFrame(page, live, 'sub');
	await expect
		.poll(() => requests.mediaSubscriptions.at(-1))
		.toMatchObject({ variantId: '', videoQuality: VideoQuality.AUTO });
	await expect(focus).toHaveAttribute('data-focused-live-selected-variant', 'sub');
	for (const [choice, variant] of [
		['High', 'main'],
		['Low', 'sub'],
		['Main', 'main'],
		['Sub', 'sub']
	] as const) {
		await focus.getByRole('button', { name: choice, exact: true }).click();
		await expect
			.poll(() => requests.mediaSubscriptions.at(-1))
			.toMatchObject({ variantId: variant, videoQuality: VideoQuality.AUTO });
	}
	await focus.getByRole('button', { name: 'Auto', exact: true }).click();
	await expect.poll(() => requests.mediaSubscriptions.at(-1)?.variantId).toBe('');
	await presentAdmittedFrame(page, live, 'sub');
	await live.locator('video').evaluate((element) => element.dispatchEvent(new Event('error')));
	await expect(focus).toHaveAttribute('data-focused-live-fallback-variant', 'main');
	await expect.poll(() => requests.mediaSubscriptions.at(-1)?.variantId).toBe('main');
	await expect(focus).toContainText('Sub live playback was unavailable. Showing Main instead.');
	const afterFallback = requests.mediaSubscriptions.length;
	await live.locator('video').evaluate((element) => element.dispatchEvent(new Event('error')));
	expect(requests.mediaSubscriptions).toHaveLength(afterFallback);
	await expect(live).toHaveAttribute('data-session-id', session!);
	expect(new Set(requests.mediaSubscriptions.map((request) => request.subscriptionId))).toEqual(
		new Set(['camera-0'])
	);
	expect(
		await live.locator('video').evaluate((element, original) => element === original, video)
	).toBe(true);
	expect(creates).toBe(initialCreates);
});

test('focused Auto stays exact when only one of two advertised codecs is compatible', async ({
	page
}) => {
	await page.addInitScript(() => {
		const original = RTCRtpReceiver.getCapabilities.bind(RTCRtpReceiver);
		Object.defineProperty(RTCRtpReceiver, 'getCapabilities', {
			value: (kind: string) => {
				const capabilities = original(kind);
				return kind === 'video' && capabilities
					? {
							...capabilities,
							codecs: capabilities.codecs.filter(
								(codec) => codec.mimeType.toLowerCase() === 'video/h264'
							)
						}
					: capabilities;
			}
		});
	});
	const { requests, focus, live } = await focusedFixture(page, [
		{ ...main, encoding: 'h265' },
		sub
	]);
	await presentAdmittedFrame(page, live, 'sub');
	await expect.poll(() => requests.mediaSubscriptions.length).toBe(2);
	await expect(focus).toHaveAttribute('data-focused-live-preference', 'auto');
	expect(requests.mediaSubscriptions.map((request) => request.variantId)).toEqual(['sub', 'sub']);
});

test('focused Auto delegates a single advertised native variant instead of inventing its sibling', async ({
	page
}) => {
	const { requests, focus, live } = await focusedFixture(page, [main]);
	expect(requests.mediaSubscriptions[0].variantId).toBe('main');
	await presentAdmittedFrame(page, live, 'main');
	await expect
		.poll(() => requests.mediaSubscriptions.at(-1))
		.toMatchObject({ variantId: '', videoQuality: VideoQuality.AUTO });
	await expect(focus).toHaveAttribute('data-focused-live-selected-variant', 'main');
});
