import { page, userEvent } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { create } from '@bufbuild/protobuf';
import {
	LiveQualityDiagnosticsSchema,
	LiveQualitySelectionReason,
	type LiveQualityDiagnostics
} from '$lib/proto/webrtc_pb';
import '../../app.css';

const state = vi.hoisted(() => ({
	estimate: null as number | null,
	quality: undefined as LiveQualityDiagnostics | undefined,
	requestedQuality: 'high' as 'auto' | 'high'
}));

vi.mock('$lib/stream-peer-context', () => ({
	useLivePeer: () => ({
		track: () => ({
			status: 'queued',
			subscribed: false,
			requestedQuality: state.requestedQuality,
			activeStream: 'main',
			estimatedBitrateBps: 123_000,
			liveQuality: state.quality
		}),
		attach: () => () => {},
		markPlaying: () => {},
		resumeAudioPlayback: async () => {},
		sessionId: 'session-network',
		get estimatedBitrateBps() {
			return state.estimate;
		},
		connectionState: 'new',
		iceConnectionState: 'new'
	})
}));

import LiveVideo from './LiveVideo.svelte';

describe('Live network estimate diagnostics', () => {
	it.each([
		[320, null, 'Unknown'],
		[390, 0, '0 bps'],
		[390, 8_000, '8 kbps'],
		[1280, 8_000_000, '8.0 Mbps']
	] as const)(
		'keeps the %ipx session estimate %s distinct from receive rate',
		async (width, estimate, label) => {
			await page.viewport(width, 844);
			state.estimate = estimate;
			state.quality = undefined;
			state.requestedQuality = 'high';
			const { container } = await render(LiveVideo, {
				cameraId: 'front-door',
				stream: 'main',
				quality: 'high',
				class: 'h-64 w-full'
			});
			const video = container.querySelector('video');
			await page.getByRole('button', { name: 'WebRTC stream diagnostics' }).click();
			const dialog = page.getByRole('dialog', { name: 'WebRTC stream diagnostics' });
			await expect.element(dialog).toBeVisible();
			const panel = document.querySelector<HTMLElement>('[data-web-rtc-diagnostics="front-door"]')!;
			const metric = panel.querySelector<HTMLElement>(
				'[data-web-rtc-metric="session-network-estimate"]'
			)!;
			expect(metric).not.toBeNull();
			expect(metric.textContent?.trim()).toBe(label);
			expect(metric.previousElementSibling?.textContent).toBe('Session network estimate');
			expect(
				panel.querySelector('[data-web-rtc-metric="receive-bitrate"]')?.textContent?.trim()
			).toBe('—');
			expect(panel.textContent).not.toContain('Server capacity');
			expect(container.querySelector('video')).toBe(video);
			expect(
				container.querySelector('[data-requested-quality]')?.getAttribute('data-requested-quality')
			).toBe('high');
			const bounds = panel.getBoundingClientRect();
			expect(bounds.left).toBeGreaterThanOrEqual(0);
			expect(bounds.right).toBeLessThanOrEqual(width);
			expect(metric.getBoundingClientRect().right).toBeLessThanOrEqual(bounds.right);
			await userEvent.keyboard('{Escape}');
			await expect
				.element(page.getByRole('button', { name: 'WebRTC stream diagnostics' }))
				.toHaveAttribute('aria-expanded', 'false');
			await expect
				.poll(() => document.querySelector('[data-web-rtc-diagnostics="front-door"]'))
				.toBeNull();
		}
	);

	it.each([
		{
			name: 'unknown estimate',
			width: 320,
			quality: create(LiveQualityDiagnosticsSchema, {
				activeVariantId: 'sub',
				selectionReason: LiveQualitySelectionReason.BANDWIDTH_UNKNOWN
			}),
			budget: 'Unknown',
			reason: 'Awaiting network estimate',
			active: 'Sub',
			pending: 'No change'
		},
		{
			name: 'zero allocation and keyframe handover',
			width: 390,
			quality: create(LiveQualityDiagnosticsSchema, {
				allocatedBitrateBps: 0n,
				activeVariantId: 'main',
				pendingVariantId: 'sub',
				selectionReason: LiveQualitySelectionReason.WAITING_KEYFRAME
			}),
			budget: '0 bps',
			reason: 'Waiting for keyframe',
			active: 'Main',
			pending: 'Sub'
		},
		{
			name: 'recovery hold',
			width: 390,
			quality: create(LiveQualityDiagnosticsSchema, {
				allocatedBitrateBps: 4_000_000n,
				activeVariantId: 'sub',
				selectionReason: LiveQualitySelectionReason.UPGRADE_HOLD
			}),
			budget: '4.0 Mbps',
			reason: 'Waiting for stable budget',
			active: 'Sub',
			pending: 'No change'
		},
		{
			name: 'manual High',
			manual: true,
			width: 320,
			quality: create(LiveQualityDiagnosticsSchema, {
				activeVariantId: 'main',
				selectionReason: LiveQualitySelectionReason.MANUAL
			}),
			budget: 'Not used for manual',
			reason: 'Manual quality choice',
			active: 'Main',
			pending: 'No change'
		},
		{
			name: 'manual pending keyframe',
			manual: true,
			width: 320,
			quality: create(LiveQualityDiagnosticsSchema, {
				activeVariantId: 'sub',
				pendingVariantId: 'main',
				selectionReason: LiveQualitySelectionReason.WAITING_KEYFRAME
			}),
			budget: 'Not used for manual',
			reason: 'Waiting for keyframe',
			active: 'Sub',
			pending: 'Main'
		},
		{
			name: 'single native variant',
			width: 320,
			quality: create(LiveQualityDiagnosticsSchema, {
				activeVariantId: 'sub',
				selectionReason: LiveQualitySelectionReason.SINGLE_VARIANT
			}),
			budget: 'Single variant',
			reason: 'Only one compatible variant',
			active: 'Sub',
			pending: 'No change'
		},
		{
			name: 'fixed delivery',
			width: 320,
			quality: create(LiveQualityDiagnosticsSchema, {
				activeVariantId: 'main',
				selectionReason: LiveQualitySelectionReason.FIXED_DELIVERY
			}),
			budget: 'Fixed delivery',
			reason: 'Fixed delivery',
			active: 'Main',
			pending: 'No change'
		},
		{
			name: 'future telemetry',
			width: 320,
			quality: create(LiveQualityDiagnosticsSchema, {
				allocatedBitrateBps: 18_446_744_073_709_551_615n,
				activeVariantId: 'future',
				pendingVariantId: 'future',
				selectionReason: 999 as LiveQualitySelectionReason
			}),
			budget: 'Unknown',
			reason: 'Unavailable',
			active: 'Unavailable',
			pending: 'Unavailable'
		},
		{
			name: 'older server',
			width: 320,
			quality: undefined,
			budget: 'Unavailable',
			reason: 'Unavailable',
			active: 'Main',
			pending: 'No change'
		}
	])('shows $name without changing playback or overflowing $width px', async (scene) => {
		await page.viewport(scene.width, 844);
		state.estimate = null;
		state.quality = scene.quality;
		state.requestedQuality = 'manual' in scene && scene.manual ? 'high' : 'auto';
		const { container } = await render(LiveVideo, {
			cameraId: 'front-door',
			stream: 'main',
			quality: state.requestedQuality,
			class: 'h-64 w-full'
		});
		const video = container.querySelector('video');
		await page.getByRole('button', { name: 'WebRTC stream diagnostics' }).click();
		await expect
			.element(page.getByRole('dialog', { name: 'WebRTC stream diagnostics' }))
			.toBeVisible();
		const panel = document.querySelector<HTMLElement>('[data-web-rtc-diagnostics="front-door"]')!;
		for (const [metric, expected] of [
			['allocated-bitrate', scene.budget],
			['quality-reason', scene.reason],
			['active-stream', scene.active],
			['pending-stream', scene.pending]
		]) {
			const value = panel.querySelector<HTMLElement>(`[data-web-rtc-metric="${metric}"]`)!;
			expect(value).not.toBeNull();
			expect(value.textContent?.trim()).toBe(expected);
			expect(value.getBoundingClientRect().right).toBeLessThanOrEqual(scene.width);
		}
		expect(container.querySelector('video')).toBe(video);
		expect(
			container.querySelector('[data-requested-quality]')?.getAttribute('data-requested-quality')
		).toBe(state.requestedQuality);
		expect(panel.getBoundingClientRect().left).toBeGreaterThanOrEqual(0);
		expect(panel.getBoundingClientRect().right).toBeLessThanOrEqual(scene.width);
		const dialogBounds = panel.closest('[role="dialog"]')!.getBoundingClientRect();
		expect(dialogBounds.top).toBeGreaterThanOrEqual(0);
		expect(dialogBounds.bottom).toBeLessThanOrEqual(844);
	});
});
