import { expect, test, type Page } from '@playwright/test';
import type { CameraAccessSettings } from '../src/lib/access';
import type { Request } from '../src/lib/proto/webrtc_pb';

type CameraAccessProbe = {
	checkAccess(): Promise<void>;
	signIn(key: string): Promise<void>;
	getCameras(): Promise<{ id: string; name: string | null }[]>;
	getPeekLayoutRegistry(): Promise<{ layouts: { items: { cameraId: string }[] }[] }>;
	createAccessCredential(input: { name: string; role: 'user' }): Promise<{
		credential: { id: string; name: string };
		accessKey: string;
	}>;
	getCameraAccess(id: string): Promise<CameraAccessSettings>;
	saveCameraAccess(settings: CameraAccessSettings): Promise<CameraAccessSettings>;
	revokeAccessCredential(id: string): Promise<unknown>;
};
type ProbeWindow = typeof window & { cameraAccessProbe: CameraAccessProbe };

async function startProbe(page: Page, accessKey?: string): Promise<void> {
	await page.evaluate(async (key) => {
		const modulePath = '/src/lib/control-client.ts';
		const { ControlClient } = await import(modulePath);
		const controller: CameraAccessProbe = new ControlClient();
		(window as ProbeWindow).cameraAccessProbe = controller;
		if (key) await controller.signIn(key);
		else await controller.checkAccess();
	}, accessKey);
}

test('enforces per-user group and camera access with Paper-grounded controls', async ({
	page,
	browser
}, testInfo) => {
	test.setTimeout(120_000);
	const pageErrors: string[] = [];
	page.on('pageerror', (error) => pageErrors.push(error.message));
	await page.goto('/settings#access');
	await expect(
		page.getByRole('heading', { name: 'Access credentials', exact: true })
	).toBeVisible();
	await startProbe(page);
	const fixture = await page.evaluate(async () => {
		const controller = (window as ProbeWindow).cameraAccessProbe;
		const cameras = await controller.getCameras();
		const camera = cameras.find((camera) => camera.name === 'e2e-h264');
		if (!camera) throw new Error('The test camera must be available');
		const issued = await controller.createAccessCredential({
			name: `Camera grant test ${Date.now()}`,
			role: 'user'
		});
		const policy = await controller.getCameraAccess(issued.credential.id);
		return {
			id: issued.credential.id,
			name: issued.credential.name,
			key: issued.accessKey,
			cameraId: camera.id,
			cameraName: camera.name ?? camera.id,
			defaultAll: policy.allCameras
		};
	});
	expect(fixture.defaultAll).toBe(true);
	const userContext = await browser.newContext({
		extraHTTPHeaders: { 'X-Forwarded-For': '203.0.113.77' }
	});
	const userPage = await userContext.newPage();
	userPage.on('pageerror', (error) => pageErrors.push(error.message));
	try {
		await userPage.goto('/');
		await expect(userPage.getByRole('heading', { name: 'Remote sign-in' })).toBeVisible();
		await userPage.getByLabel('Access key').fill(fixture.key);
		await userPage.getByRole('button', { name: 'Sign in', exact: true }).click();
		await startProbe(userPage, fixture.key);
		expect(
			await userPage.evaluate(async () =>
				(await (window as ProbeWindow).cameraAccessProbe.getCameras()).map((camera) => camera.id)
			)
		).toContain(fixture.cameraId);
		await page.evaluate(async (id) => {
			const controller = (window as ProbeWindow).cameraAccessProbe;
			const policy = await controller.getCameraAccess(id);
			await controller.saveCameraAccess({
				...policy,
				allCameras: false,
				groupIds: [],
				cameraIds: []
			});
		}, fixture.id);
		await expect(userPage.getByRole('heading', { name: 'Remote sign-in' })).toBeVisible();
		await userPage.getByLabel('Access key').fill(fixture.key);
		await userPage.getByRole('button', { name: 'Sign in', exact: true }).click();
		await userPage.evaluate(
			async (key) => (window as ProbeWindow).cameraAccessProbe.signIn(key),
			fixture.key
		);
		expect(
			await userPage.evaluate(async () => (window as ProbeWindow).cameraAccessProbe.getCameras())
		).toEqual([]);
		const emptyGrid = await userPage.evaluate(async () =>
			(window as ProbeWindow).cameraAccessProbe.getPeekLayoutRegistry()
		);
		expect(emptyGrid.layouts.every((layout) => layout.items.length === 0)).toBe(true);
		const denied = await userPage.evaluate(async (cameraId) => {
			const controller = (window as ProbeWindow).cameraAccessProbe as unknown as {
				request(command: Request['command']): Promise<unknown>;
			};
			try {
				await controller.request({
					case: 'cameraControlCommand',
					value: {
						$typeName: 'keeppeek.webrtc.v1.CameraControlCommand',
						action: {
							case: 'getMotionDetection',
							value: { $typeName: 'keeppeek.webrtc.v1.GetMotionDetection', sourceId: cameraId }
						}
					}
				});
				return false;
			} catch (error) {
				return error instanceof Error && error.message.includes('camera access');
			}
		}, fixture.cameraId);
		expect(denied).toBe(true);

		await page.getByRole('button', { name: 'Refresh', exact: true }).click();
		await page.getByRole('button', { name: `User access for ${fixture.name}` }).click();
		await expect(page.getByRole('dialog', { name: 'User access' })).toBeVisible();
		await page.setViewportSize({ width: 320, height: 740 });
		await expect(page.getByRole('dialog', { name: 'User access' })).toBeVisible();
		await expect(page.getByRole('radio', { name: 'Selected groups and cameras' })).toBeChecked();
		const mobileRow = page.locator('[data-user-access-row]').first();
		expect(await mobileRow.evaluate((element) => element.getBoundingClientRect().height)).toBe(52);
		await expect
			.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
			.toBe(true);
		await page.screenshot({ path: testInfo.outputPath('user-access-paper-mobile.png') });
		await page.setViewportSize({ width: 1440, height: 900 });
		await page.getByRole('checkbox', { name: 'test-camera', exact: true }).check();
		expect(await mobileRow.evaluate((element) => element.getBoundingClientRect().height)).toBe(44);
		const type = await page
			.getByRole('heading', { name: 'User access', exact: true })
			.evaluate((element) => {
				const style = getComputedStyle(element);
				return { family: style.fontFamily, size: style.fontSize };
			});
		expect(type.family).toContain('Archivo');
		expect(type.size).toBe('18px');
		await page.screenshot({ path: testInfo.outputPath('user-access-paper-desktop.png') });
		await page.getByRole('button', { name: 'Save access', exact: true }).click();
		await expect(page.getByRole('dialog', { name: 'User access' })).toHaveCount(0);
		await expect(userPage.getByRole('heading', { name: 'Remote sign-in' })).toBeVisible();
		await userPage.getByLabel('Access key').fill(fixture.key);
		await userPage.getByRole('button', { name: 'Sign in', exact: true }).click();
		await userPage.evaluate(
			async (key) => (window as ProbeWindow).cameraAccessProbe.signIn(key),
			fixture.key
		);
		expect(
			await userPage.evaluate(async () =>
				(await (window as ProbeWindow).cameraAccessProbe.getCameras()).map((camera) => camera.id)
			)
		).toEqual([fixture.cameraId]);
		const grid = await userPage.evaluate(async () =>
			(window as ProbeWindow).cameraAccessProbe.getPeekLayoutRegistry()
		);
		expect(grid.layouts.flatMap((layout) => layout.items.map((item) => item.cameraId))).toContain(
			fixture.cameraId
		);
		await page.evaluate(async (id) => {
			const controller = (window as ProbeWindow).cameraAccessProbe;
			const settings = await controller.getCameraAccess(id);
			await controller.saveCameraAccess({
				...settings,
				allCameras: false,
				groupIds: [],
				cameraIds: []
			});
		}, fixture.id);
		await expect(userPage.getByRole('heading', { name: 'Remote sign-in' })).toBeVisible();
		expect(pageErrors).toEqual([]);
	} finally {
		await userContext.close();
		await page.evaluate(
			async (id) => (window as ProbeWindow).cameraAccessProbe.revokeAccessCredential(id),
			fixture.id
		);
	}
});
