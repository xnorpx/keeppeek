import { chromium, expect, test } from '@playwright/test';
import { mkdir, writeFile } from 'node:fs/promises';
import { dirname } from 'node:path';
import { performance } from 'node:perf_hooks';
import { startHomeAssistantServer } from './fixtures/home-assistant-server';
import { recordedAudioBackendRoute } from './fixtures/recorded-audio-playback';

function microphoneTone(): Buffer {
	const rate = 48_000;
	const samples = rate * 2;
	const wav = Buffer.alloc(44 + samples * 2);
	wav.write('RIFF', 0);
	wav.writeUInt32LE(wav.length - 8, 4);
	wav.write('WAVEfmt ', 8);
	wav.writeUInt32LE(16, 16);
	wav.writeUInt16LE(1, 20);
	wav.writeUInt16LE(1, 22);
	wav.writeUInt32LE(rate, 24);
	wav.writeUInt32LE(rate * 2, 28);
	wav.writeUInt16LE(2, 32);
	wav.writeUInt16LE(16, 34);
	wav.write('data', 36);
	wav.writeUInt32LE(samples * 2, 40);
	for (let i = 0; i < samples; i++) {
		wav.writeInt16LE(Math.round(12_000 * Math.sin((2 * Math.PI * 440 * i) / rate)), 44 + 2 * i);
	}
	return wav;
}

test('the Talk control sends the browser microphone tone to the Hikvision speaker and releases it', async ({
	baseURL
}, info) => {
	if (!baseURL) throw new Error('The browser microphone test needs a configured UI origin');
	test.setTimeout(90_000);
	const wav = info.outputPath('microphone-440hz.wav');
	await mkdir(dirname(wav), { recursive: true });
	await writeFile(wav, microphoneTone());
	const browser = await chromium.launch({
		args: [
			'--use-fake-device-for-media-stream',
			'--use-fake-ui-for-media-stream',
			`--use-file-for-fake-audio-capture=${wav}`
		]
	});
	try {
		const fixture = await startHomeAssistantServer(new URL(baseURL).origin, {
			trustedLocal: true,
			hikvisionSpeaker: true
		});
		try {
			const observe = fixture.speakerObservation;
			if (!observe) throw new Error('The simulated camera has no speaker observation');
			const context = await browser.newContext({ baseURL, permissions: ['microphone'] });
			await context.route('**/*', recordedAudioBackendRoute(new URL(baseURL).origin, fixture.url));
			const page = await context.newPage();
			await page.goto('/');
			await expect
				.poll(observe)
				.toEqual({ active: false, bytes: 0, audible: 0, crossings: 0, peerClosedUploads: 0 });
			const tile = page.locator('[data-peek-camera="127.0.0.1"]');
			await tile.getByRole('button', { name: /^Focus .* live view$/ }).click();
			const focus = page.getByRole('region', { name: /focus$/ });
			const talk = focus.getByRole('button', { name: 'Talk to 127.0.0.1', exact: true });
			await expect(talk).toBeVisible({ timeout: 30_000 });
			const started = performance.now();
			await talk.click();
			await expect
				.poll(async () => (await observe()).audible, { timeout: 15_000 })
				.toBeGreaterThan(0);
			const firstAudibleAfterMs = performance.now() - started;
			await expect.poll(async () => (await observe()).audible).toBeGreaterThan(1_600);
			const speaker = await observe();
			expect(speaker.active).toBe(true);
			expect(speaker.bytes).toBeGreaterThan(0);
			const hz = (speaker.crossings * 8_000) / (2 * speaker.bytes);
			expect(hz).toBeGreaterThan(350);
			expect(hz).toBeLessThan(520);
			await info.attach('speaker-observation', {
				contentType: 'application/json',
				body: JSON.stringify({ ...speaker, hz, firstAudibleAfterMs })
			});
			await focus.getByRole('button', { name: 'Stop talkback to 127.0.0.1', exact: true }).click();
			await expect.poll(async () => (await observe()).active).toBe(false);
			await expect.poll(async () => (await observe()).peerClosedUploads).toBe(1);
			await expect(talk).toBeVisible();
			await talk.click();
			await expect
				.poll(async () => {
					const current = await observe();
					return current.active && current.audible > 0;
				})
				.toBe(true);
			await page.close();
			await expect.poll(async () => (await observe()).active).toBe(false);
			await expect.poll(async () => (await observe()).peerClosedUploads).toBe(2);
			await info.attach('speaker-closure', {
				contentType: 'application/json',
				body: JSON.stringify(await observe())
			});
		} finally {
			try {
				await info.attach('recorder-log', {
					body: fixture.logs(),
					contentType: 'text/plain'
				});
			} finally {
				await fixture.close();
			}
		}
	} finally {
		await browser.close();
	}
});
