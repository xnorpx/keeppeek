import { create } from '@bufbuild/protobuf';
import { page, userEvent } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { RecordedAudioTimingSchema } from '$lib/proto/webrtc_pb';
import RecordedAudioPreview from './RecordedAudioPreview.svelte';
import RecordedAudioTimingDetails from './RecordedAudioTimingDetails.svelte';
import '../../app.css';

const timing = create(RecordedAudioTimingSchema, {
	configuredOffsetMs: 250,
	appliedOffsetMs: 250,
	configurationRevision: 'timing-fingerprint',
	mappingVersion: 1,
	audioPresent: true,
	clippedAudioSamples: 0,
	firstVideoPtsUs: 0n,
	firstAudioPtsUs: 64_000n,
	storedSkewUs: 64_000n
});

describe('recorded audio preview', () => {
	it.each([320, 390, 1280])(
		'keeps bounded preview and configured reset distinct at %ipx',
		async (width) => {
			await page.viewport(width, 900);
			const onapply = vi.fn(async () => {});
			const onclose = vi.fn();
			await render(RecordedAudioPreview, { timing, name: 'Front Door / Main', onapply, onclose });
			const input = page.getByRole('spinbutton', { name: 'Preview offset (ms)' });
			await input.fill('0');
			await page.getByRole('button', { name: 'Apply preview' }).click();
			expect(onapply).toHaveBeenCalledWith(0);
			await page.getByRole('button', { name: 'Reset preview' }).click();
			expect(onapply).toHaveBeenLastCalledWith(null);
			await expect.element(page.getByText('Saved configured')).toBeVisible();
			await expect.element(page.getByText('New exports use saved settings.')).toBeVisible();
			const dialog = page.getByRole('dialog').element();
			expect(dialog.scrollWidth).toBeLessThanOrEqual(dialog.clientWidth);
			await userEvent.keyboard('{Escape}');
			expect(onclose).toHaveBeenCalledOnce();
		}
	);

	it('retains a rejected preview draft and reports the actual failure', async () => {
		const onapply = vi.fn(async () => {
			throw new Error('Unsupported AAC timestamps.');
		});
		await render(RecordedAudioPreview, {
			timing,
			name: 'Front Door / Main',
			onapply,
			onclose: vi.fn()
		});
		const input = page.getByRole('spinbutton', { name: 'Preview offset (ms)' });
		await input.fill('1250');
		await expect
			.element(page.getByRole('status'))
			.toHaveTextContent(
				'More than 1000 ms can create a noticeable lead or delay. Check a clear sound and visible action.'
			);
		await page.getByRole('button', { name: 'Apply preview' }).click();
		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent('Unsupported AAC timestamps. Your draft is retained.');
		await expect.element(input).toHaveValue(1250);
		expect(onapply).toHaveBeenCalledOnce();
	});

	it('labels absent export timing as unavailable and inspected no-audio separately', async () => {
		const view = await render(RecordedAudioTimingDetails, { snapshot: true });
		await expect.element(page.getByText('Recorded audio timing unavailable.')).toBeVisible();
		await expect.element(page.getByText('0 ms', { exact: true })).not.toBeInTheDocument();
		await view.rerender({ snapshot: true, timing: { ...timing, audioPresent: false } });
		await expect.element(page.getByText('No audio in the inspected export range.')).toBeVisible();
		await expect.element(page.getByText(/Later setting or preview changes/)).toBeVisible();
	});
});
