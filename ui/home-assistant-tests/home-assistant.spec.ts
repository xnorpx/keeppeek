import { expect, type Page } from '@playwright/test';
import { writeFile } from 'node:fs/promises';
import { test, onboardHomeAssistant } from './fixtures';
import {
	installCardProbe,
	interruptCardPeer,
	readCardProbe
} from '../e2e/fixtures/home-assistant-browser';
import { cardSourceIds, cardTestKeys } from '../e2e/fixtures/home-assistant-server';
import { observeBrowserErrors } from './browser-diagnostics';
import { sanitizedLog } from './container';

test('CI diagnostics redact credentials while preserving ordinary errors', () => {
	const sanitized = sanitizedLog(
		`\u001b[31mBearer fixture-header-token\u001b[0m\npassword=local-ha-test-password\naccess_token=fixture-ha-token\n${cardTestKeys[0]}\nCamera connection failed`
	);
	for (const secret of [
		'fixture-header-token',
		'local-ha-test-password',
		'fixture-ha-token',
		cardTestKeys[0],
		'\u001b'
	]) {
		expect(sanitized).not.toContain(secret);
	}
	expect(sanitized).toContain('Camera connection failed');
});

async function decodedFrames(page: Page, count: number): Promise<void> {
	await expect(page.locator('keeppeek-card video')).toHaveCount(count);
	await expect
		.poll(
			async () => {
				const frames = await page.locator('keeppeek-card video').evaluateAll((elements) =>
					Promise.all(
						elements.map((element) => {
							const video = element as HTMLVideoElement;
							const stream = video.srcObject;
							if (!(stream instanceof MediaStream) || video.paused || video.readyState < 2)
								return false;
							return new Promise<boolean>((resolveFrame) => {
								const timer = setTimeout(() => {
									video.cancelVideoFrameCallback(callback);
									resolveFrame(false);
								}, 2000);
								const callback = video.requestVideoFrameCallback(() => {
									clearTimeout(timer);
									resolveFrame(
										video.isConnected &&
											video.srcObject === stream &&
											!video.paused &&
											video.videoWidth === 640 &&
											video.videoHeight === 360 &&
											video.getVideoPlaybackQuality().totalVideoFrames >= 2 &&
											stream.getVideoTracks().some((track) => track.readyState === 'live')
									);
								});
							});
						})
					)
				);
				return frames.length === count && frames.every(Boolean);
			},
			{ timeout: 30_000 }
		)
		.toBe(true);
	await expect(page.locator('keeppeek-card .video-state')).toHaveCount(0);
}

async function closedCardSessions(page: Page): Promise<void> {
	await expect
		.poll(async () => {
			const probe = await readCardProbe(page);
			return probe.created - probe.closed;
		})
		.toBe(0);
}

async function captureEvidence(page: Page, filePath: string): Promise<void> {
	await page.screenshot({ path: filePath, fullPage: true, animations: 'disabled' });
}

test('real Lovelace shares video, resizes, changes theme, and releases sessions on navigation', async ({
	page,
	homeAssistant
}, testInfo) => {
	const diagnostics = await observeBrowserErrors(page);
	await installCardProbe(page);
	await page.setViewportSize({ width: 1440, height: 1000 });
	await onboardHomeAssistant(page, homeAssistant.url);
	await writeFile(
		testInfo.outputPath('onboarding-notices.json'),
		JSON.stringify(diagnostics.onboardingNotices)
	);
	diagnostics.phase('live dashboard');
	const requests: string[] = [];
	page.on('request', (request) => {
		if (request.method() === 'POST' && /\/(create|delete)$/.test(new URL(request.url()).pathname))
			requests.push(request.url());
	});
	await page.goto(`${homeAssistant.url}/dashboard-keeppeek/live`);
	await expect(page.locator('keeppeek-card')).toHaveCount(1);
	await decodedFrames(page, 2);
	expect(await homeAssistant.activeSessions()).toBe(1);
	const initial = await readCardProbe(page);
	expect(initial.subscriptions).toBe(2);
	await captureEvidence(page, testInfo.outputPath('lovelace-desktop.png'));
	const surface = page.locator('keeppeek-card .keeppeek-card');
	const background = await surface.evaluate((element) => getComputedStyle(element).backgroundColor);
	diagnostics.phase('theme and resize');
	await page.emulateMedia({ colorScheme: 'dark' });
	await expect
		.poll(() => surface.evaluate((element) => getComputedStyle(element).backgroundColor))
		.not.toBe(background);
	expect((await readCardProbe(page)).created).toBe(initial.created);
	await page.setViewportSize({ width: 390, height: 844 });
	await decodedFrames(page, 2);
	expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
	const mobile = await readCardProbe(page);
	expect(mobile.created - mobile.closed).toBe(1);
	expect(await homeAssistant.activeSessions()).toBe(1);
	await captureEvidence(page, testInfo.outputPath('lovelace-mobile-dark.png'));
	await page.setViewportSize({ width: 1440, height: 1000 });
	diagnostics.phase('dashboard navigation');
	await page.getByRole('tab', { name: 'Away', exact: true }).click();
	await expect.poll(() => homeAssistant.activeSessions()).toBe(0);
	await closedCardSessions(page);
	const beforeShared = await readCardProbe(page);
	expect(beforeShared.closed).toBe(beforeShared.created);
	await page.getByRole('tab', { name: 'Shared', exact: true }).click();
	await decodedFrames(page, 3);
	const shared = await readCardProbe(page);
	expect(shared.created - shared.closed).toBe(1);
	expect(shared).toMatchObject({ subscriptions: beforeShared.subscriptions + 1, overflow: false });
	expect(await homeAssistant.activeSessions()).toBe(1);
	diagnostics.phase('peer interruption');
	await interruptCardPeer(page);
	await expect.poll(async () => (await readCardProbe(page)).created).toBe(shared.created + 1);
	await decodedFrames(page, 3);
	await page.getByRole('tab', { name: 'Away', exact: true }).click();
	await expect.poll(() => homeAssistant.activeSessions()).toBe(0);
	await closedCardSessions(page);
	const released = await readCardProbe(page);
	expect(released.created).toBe(released.closed);
	expect(released).toMatchObject({ subscriptions: shared.subscriptions + 1, overflow: false });
	expect(requests.every((url) => new URL(url).origin === homeAssistant.keeppeekURL)).toBe(true);
	expect(diagnostics.errors).toEqual([]);
});

async function createEditableDashboard(page: Page, keeppeekURL: string): Promise<void> {
	await page.evaluate(
		async ({ endpoint, token, sourceId }) => {
			const application = document.querySelector('home-assistant') as HTMLElement & {
				hass: { callWS: (message: Record<string, unknown>) => Promise<unknown> };
			};
			await application.hass.callWS({
				type: 'lovelace/dashboards/create',
				title: 'Card editor test',
				url_path: 'dashboard-card-editor',
				show_in_sidebar: true
			});
			await application.hass.callWS({
				type: 'lovelace/config/save',
				url_path: 'dashboard-card-editor',
				config: {
					views: [
						{
							title: 'Editor',
							path: 'edit',
							cards: [
								{
									type: 'custom:keeppeek-card',
									endpoint,
									token,
									title: 'Before edit',
									sources: [{ source_id: sourceId, title: 'Front entrance' }]
								}
							]
						}
					]
				}
			});
		},
		{ endpoint: keeppeekURL, token: cardTestKeys[0], sourceId: cardSourceIds[0] }
	);
}

test('Home Assistant opens the real visual editor and persists its changes', async ({
	page,
	homeAssistant
}, testInfo) => {
	const diagnostics = await observeBrowserErrors(page);
	await page.setViewportSize({ width: 1440, height: 1000 });
	await onboardHomeAssistant(page, homeAssistant.url);
	await writeFile(
		testInfo.outputPath('onboarding-notices.json'),
		JSON.stringify(diagnostics.onboardingNotices)
	);
	diagnostics.phase('visual editor');
	await createEditableDashboard(page, homeAssistant.keeppeekURL);
	await page.goto(`${homeAssistant.url}/dashboard-card-editor/edit`);
	await decodedFrames(page, 1);
	await page.getByRole('button', { name: 'Open dashboard menu', exact: true }).click();
	await page.getByText('Edit dashboard', { exact: true }).click();
	await page.getByRole('button', { name: 'Edit', exact: true }).first().click();
	await expect(page.locator('keeppeek-card-editor')).toBeVisible();
	await page.getByLabel('Card title', { exact: true }).fill('After edit');
	await page.getByRole('button', { name: 'Load sources', exact: true }).click();
	await expect(page.locator('keeppeek-card-editor datalist option')).toHaveCount(2);
	expect(await page.locator('keeppeek-card-editor input[type="password"]').inputValue()).toBe('');
	expect(
		await page.locator('keeppeek-card-editor').evaluate((element) => element.shadowRoot!.innerHTML)
	).not.toContain(cardTestKeys[0]);
	await captureEvidence(page, testInfo.outputPath('lovelace-editor.png'));
	await page.getByRole('button', { name: 'Save', exact: true }).click();
	await page.goto(`${homeAssistant.url}/dashboard-card-editor/edit`);
	await expect(
		page.locator('keeppeek-card').getByRole('heading', { name: 'After edit' })
	).toBeVisible();
	await decodedFrames(page, 1);
	await page.goto(`${homeAssistant.url}/dashboard-keeppeek/away`);
	await expect.poll(() => homeAssistant.activeSessions()).toBe(0);
	expect(diagnostics.errors).toEqual([]);
});
