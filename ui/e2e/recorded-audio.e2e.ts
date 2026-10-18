import { expect, test, type Page } from '@playwright/test';
import { resolve } from 'node:path';
import { writeFile } from 'node:fs/promises';
import {
	cardSourceIds,
	cardTestKeys,
	startHomeAssistantServer
} from './fixtures/home-assistant-server';
import {
	decodedRecordedAudio,
	inspectRecordedAudio,
	previewRecordedAudio,
	recordedAudioBackendRoute
} from './fixtures/recorded-audio-playback';
import type { CalibrationRecording } from './fixtures/calibration-recording';
import { correctedMseAppends, installRecordedAudioMseProbe } from './fixtures/recorded-audio-mse';
import { installRecordedAudioControlProbe } from './fixtures/recorded-audio-control';

function recordingLink(at: number) {
	return `/keep?camera=${cardSourceIds[0]}&stream=sub&at=${at}&date=${new Date(at).toISOString().slice(0, 10)}`;
}

async function signIn(page: Page) {
	await expect(page.getByRole('heading', { name: 'Remote sign-in', exact: true })).toBeVisible();
	await page.getByLabel('Access key', { exact: true }).fill(cardTestKeys[0]);
	await page.getByRole('button', { name: 'Sign in', exact: true }).click();
}

async function exerciseCalibration(
	page: Page,
	manifest: CalibrationRecording,
	observations: unknown[]
) {
	await page.goto(recordingLink(manifest.start_ms + 100));
	await signIn(page);
	const configured = await decodedRecordedAudio(page, '', 0, 2.25);
	observations.push({
		phase: 'configured',
		...configured,
		timing: await inspectRecordedAudio(page),
		appends: await correctedMseAppends(page, configured.source, 2)
	});
	const advanced = await previewRecordedAudio(page, -200, configured.source);
	observations.push({
		phase: 'advanced-preview',
		...advanced,
		appends: await correctedMseAppends(page, advanced.source)
	});
	const delayed = await previewRecordedAudio(page, 450, advanced.source);
	observations.push({
		phase: 'delayed-preview',
		...delayed,
		appends: await correctedMseAppends(page, delayed.source)
	});
	const reset = await previewRecordedAudio(page, null, delayed.source);
	observations.push({
		phase: 'reset',
		...reset,
		appends: await correctedMseAppends(page, reset.source)
	});
	const position = page.getByRole('slider', { name: 'Recording position', exact: true });
	await position.press('Home');
	await position.press('ArrowRight');
	await expect(position).toHaveValue('1');
	const sought = await decodedRecordedAudio(page, reset.source, 0, 0.9);
	expect(sought.currentTime).toBeGreaterThanOrEqual(0.9);
	observations.push({ phase: 'seek', ...sought });
	await page.goto(recordingLink(manifest.periods[1]!.start_ms + 100));
	await signIn(page);
	observations.push({
		phase: 'second-period',
		...(await decodedRecordedAudio(page)),
		timing: await inspectRecordedAudio(page)
	});
}

test('decodes recorded AAC through signed previews, reset, seek, and a second container period', async ({
	page,
	context,
	baseURL
}, info) => {
	test.setTimeout(120_000);
	if (!baseURL) throw new Error('Recorded audio calibration requires the configured UI origin.');
	const fixture = await startHomeAssistantServer(new URL(baseURL).origin, {
		trustedLocal: false,
		calibrationFixture: {
			mediaPath: resolve(
				import.meta.dirname,
				'../../crates/test-camera/testdata/flash-tone-lead-200ms.mp4'
			)
		}
	});
	const manifest = fixture.calibration;
	if (!manifest) throw new Error('The calibrated recording manifest is missing.');
	const routeBackend = recordedAudioBackendRoute(new URL(baseURL).origin, fixture.url);
	await context.route('**/*', routeBackend);
	await installRecordedAudioMseProbe(page);
	const controlErrors = await installRecordedAudioControlProbe(page);
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	const observations: unknown[] = [];
	try {
		await exerciseCalibration(page, manifest, observations);
		await page.screenshot({ path: info.outputPath('recorded-audio-calibration.png') });
		expect(errors).toEqual([]);
	} finally {
		try {
			const evidence = info.outputPath('recorded-audio-observations.json');
			await writeFile(
				evidence,
				JSON.stringify(
					{
						manifest,
						observations,
						errors,
						controlErrors: controlErrors(),
						serverLog: fixture.logs(),
						scope:
							'Real WebRTC/MSE H264 and AAC decode across two initializations on one corrected SourceBuffer, signed preview/reset, decoded seek and fresh second-period cursor; no browser acoustic synchronization or wire-generation ordering measurement.'
					},
					null,
					2
				)
			);
			await info.attach('recorded-audio-observations.json', {
				path: evidence,
				contentType: 'application/json'
			});
		} finally {
			try {
				await context.unroute('**/*', routeBackend);
			} finally {
				await fixture.close();
			}
		}
	}
});
