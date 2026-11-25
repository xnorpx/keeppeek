import { expect, test } from '@playwright/test';
import { mockControlPeer } from './fixtures/control-peer';
import { mixedCameras, mixedHealth } from './fixtures/peek';

for (const accessRole of ['user', 'administrator'] as const) {
	test(`the signed-in ${accessRole} can play doorbell sounds with role-appropriate Talk controls`, async ({
		page
	}) => {
		await page.addInitScript(() => {
			Object.defineProperty(navigator.mediaDevices, 'getUserMedia', {
				value: () => {
					sessionStorage.setItem('doorbell-microphone-requested', 'yes');
					return Promise.reject(new Error('A doorbell sound must not request the microphone'));
				}
			});
		});
		const front = mixedCameras[0];
		const requests = await mockControlPeer(page, {
			cameras: [
				{
					...front,
					is_reolink: true,
					groupIds: ['entrances'],
					profiles: [
						{ name: 'Sub', stream: 'sub', encoding: 'h264', resolution: '640x360', framerate: 15 }
					],
					capabilities: { ...front.capabilities!, two_way_audio: true },
					sounds: {
						quickReplies: [{ assetId: 'reply-1', displayName: 'Please leave the parcel' }],
						chimes: [{ assetId: 'chime-1', displayName: 'Front door chime' }]
					}
				}
			],
			health: mixedHealth,
			accessLocal: false,
			accessRole
		});
		await page.goto('/viewer?camera=front-door');
		const focus = page.getByRole('region', { name: 'Front Door focus' });
		await focus.getByLabel('Play doorbell sound').click();
		await focus.getByRole('button', { name: 'Please leave the parcel' }).click();
		await focus.getByRole('button', { name: 'Front door chime' }).click();
		await expect
			.poll(() => requests.doorbellSounds)
			.toEqual([
				{ action: 'playQuickReply', sourceId: 'front-door', assetId: 'reply-1' },
				{ action: 'playChime', sourceId: 'front-door', assetId: 'chime-1' }
			]);
		const talk = focus.getByRole('button', { name: 'Talk to front-door', exact: true });
		if (accessRole === 'administrator') await expect(talk).toBeVisible();
		else await expect.soft(talk).toHaveCount(0);

		await page.goto('/');
		await expect(
			page.locator('[data-peek-wall-content] [data-camera-id="front-door"]').first()
		).toBeVisible();
		const broadcast = page.getByRole('button', { name: 'Broadcast talkback', exact: true });
		const target = page.getByRole('combobox', { name: 'Broadcast talkback target' });
		if (accessRole === 'administrator') {
			await expect(broadcast).toBeVisible();
			await expect(target).toBeVisible();
		} else {
			await expect.soft(broadcast).toHaveCount(0);
			await expect.soft(target).toHaveCount(0);
		}
		expect(requests.talkbackCommands).toBe(0);
		expect(
			await page.evaluate(() => sessionStorage.getItem('doorbell-microphone-requested'))
		).toBeNull();
	});
}
