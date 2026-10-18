import { expect, test, type Page } from '@playwright/test';
import { mockControlPeer } from './fixtures/control-peer';

const fixtureToken = 'A'.repeat(43);
const sessionId = '00000000-0000-4000-8000-000000000132';
const browserLogsKey = 'keeppeek.browser-logs.v1';
const ready = {
	status: 'ready',
	session_id: sessionId,
	display_name: 'Front step visit',
	start_ms: Date.UTC(2026, 9, 17, 6, 11, 48),
	end_ms: Date.UTC(2026, 9, 17, 6, 12, 48),
	expires_at_ms: Date.UTC(2026, 9, 18, 18),
	can_stream: true,
	can_download: false,
	bytes: 24_600_000,
	has_gaps: false
};
type AuditWindow = Window & {
	__keeppeekStorageAudit?: (key: string, containsToken: boolean) => Promise<void>;
	__keeppeekInitialLog?: typeof console.log;
	__keeppeekIsolationGoto?: (url: string) => Promise<void>;
};
type StorageWrite = { key: string; containsToken: boolean };

async function auditStorage(page: Page) {
	const writes: StorageWrite[] = [];
	let overflow = false;
	await page.exposeBinding(
		'__keeppeekStorageAudit',
		(_source, key: string, containsToken: boolean) => {
			if (writes.length === 512) overflow = true;
			else writes.push({ key, containsToken });
		}
	);
	await page.addInitScript(
		({ token }) => {
			const target = window as AuditWindow;
			target.__keeppeekInitialLog = console.log;
			const original = Storage.prototype.setItem;
			Storage.prototype.setItem = function (key: string, value: string): void {
				const audit = target.__keeppeekStorageAudit;
				if (!audit) throw new Error('The storage audit binding was not installed.');
				void audit(String(key).slice(0, 80), String(value).includes(token)).catch(() => undefined);
				Reflect.apply(original, this, [key, value]);
			};
		},
		{ token: fixtureToken }
	);
	return { writes, overflow: () => overflow };
}

async function mockGuest(page: Page) {
	const requests: { method: string; url: string; tokenMatches: boolean }[] = [];
	await page.route('**/guest/session*', async (route) => {
		const request = route.request();
		requests.push({
			method: request.method(),
			url: request.url(),
			tokenMatches: request.method() === 'POST' && request.postDataJSON()?.token === fixtureToken
		});
		if (request.method() === 'DELETE') await route.fulfill({ status: 204 });
		else await route.fulfill({ json: ready });
	});
	return requests;
}

async function expectIsolatedGuest(page: Page) {
	await expect(page.getByRole('heading', { name: ready.display_name, exact: true })).toBeVisible();
	await expect(page.locator('nav')).toHaveCount(0);
	await expect(page.getByRole('link', { name: 'Dashboard', exact: true })).toHaveCount(0);
	await expect(page.getByRole('link', { name: 'Download MP4', exact: true })).toHaveCount(0);
	await expect(page.locator('video')).toHaveAttribute('src', `/guest/media?session=${sessionId}`);
	const state = await page.evaluate(
		({ logKey, token }) => {
			const before = sessionStorage.getItem(logKey);
			console.log('Guest isolation fixture probe');
			window.dispatchEvent(new ErrorEvent('error', { message: 'Guest isolation fixture probe' }));
			return {
				href: location.href,
				originalConsole: console.log === (window as AuditWindow).__keeppeekInitialLog,
				logsUnchanged: before === sessionStorage.getItem(logKey),
				storedToken: [sessionStorage, localStorage].some((storage) =>
					Object.keys(storage).some((key) => storage.getItem(key)?.includes(token))
				),
				normalModules: performance
					.getEntriesByType('resource')
					.map((entry) => entry.name)
					.filter((name) => /AppShell\.svelte|browser-logs\.ts|control-client\.ts/.test(name))
			};
		},
		{ logKey: browserLogsKey, token: fixtureToken }
	);
	expect(new URL(state.href).hash).toBe('');
	expect(state.originalConsole).toBe(true);
	expect(state.logsUnchanged).toBe(true);
	expect(state.storedToken).toBe(false);
	expect(state.normalModules).toEqual([]);
}

async function installKitNavigation(page: Page): Promise<void> {
	await page.evaluate(async () => {
		const modulePath = '/node_modules/@sveltejs/kit/src/runtime/app/navigation/client.js';
		const navigation = await import(modulePath);
		(window as AuditWindow).__keeppeekIsolationGoto = navigation.goto;
	});
}

async function startKitNavigation(page: Page, url: string): Promise<void> {
	await page.evaluate((destination) => {
		const navigate = (window as AuditWindow).__keeppeekIsolationGoto;
		if (!navigate) throw new Error('The real Kit navigation module was not installed.');
		void navigate(destination).catch(() => undefined);
	}, url);
}

test('a fresh share document strips its fragment and never starts normal control or logging', async ({
	page
}, testInfo) => {
	const audit = await auditStorage(page);
	const requests = await mockGuest(page);
	const normalRequests: string[] = [];
	page.on('request', (request) => {
		if (/\/(auth\/session|create)(?:\?|$)/.test(new URL(request.url()).pathname))
			normalRequests.push(request.url());
	});
	await page.goto(`/share#${fixtureToken}`);
	await expectIsolatedGuest(page);
	await expect
		.poll(() => requests.filter((request) => request.method === 'GET').length)
		.toBeGreaterThanOrEqual(1);
	expect(requests[0]).toMatchObject({ method: 'POST', tokenMatches: true });
	expect(new URL(requests[1].url).searchParams.get('session')).toBe(sessionId);
	expect(normalRequests).toEqual([]);
	expect(audit.overflow()).toBe(false);
	expect(audit.writes.every((write) => !write.containsToken)).toBe(true);
	await testInfo.attach('guest-storage-audit', {
		body: JSON.stringify(audit.writes),
		contentType: 'application/json'
	});
});

test('an invalid fragment cannot resume an existing guest cookie', async ({ page, context }) => {
	const requests = await mockGuest(page);
	await context.addCookies([
		{
			name: 'guest-fixture-existing-context',
			value: 'nonsecret-fixture',
			url: test.info().project.use.baseURL!
		}
	]);
	await page.goto('/share#invalid');
	await expect(
		page.getByRole('heading', { name: 'This link is unavailable.', exact: true })
	).toBeVisible();
	await expect(page.locator('video')).toHaveCount(0);
	expect(new URL(page.url()).hash).toBe('');
	expect(requests).toEqual([]);
});

for (const queued of [false, true]) {
	test(`normal to guest uses a new document${queued ? ' during a queued Kit navigation' : ''}`, async ({
		page
	}, testInfo) => {
		const audit = await auditStorage(page);
		await mockControlPeer(page, { cameras: [], health: { status: 'healthy', cameras: [] } });
		await mockGuest(page);
		await page.goto('/');
		await expect(page.getByRole('heading', { name: 'No cameras yet', exact: true })).toBeVisible();
		await installKitNavigation(page);
		const before = await page.evaluate(() => performance.timeOrigin);
		const gate = Promise.withResolvers<void>();
		let holdingEvents = false;
		if (queued) {
			await page.route('**/src/routes/events/+page.svelte*', async (route) => {
				holdingEvents = true;
				await gate.promise;
				await route.continue().catch(() => undefined);
			});
		}
		try {
			if (queued) {
				await startKitNavigation(page, '/events');
				await expect.poll(() => holdingEvents).toBe(true);
			}
			await startKitNavigation(page, `/share#${fixtureToken}`);
			await expectIsolatedGuest(page);
			expect(await page.evaluate(() => performance.timeOrigin)).not.toBe(before);
			expect(audit.overflow()).toBe(false);
			expect(audit.writes.every((write) => !write.containsToken)).toBe(true);
			await testInfo.attach('cross-document-storage-audit', {
				body: JSON.stringify(audit.writes),
				contentType: 'application/json'
			});
		} finally {
			gate.resolve();
		}
	});
}

test('later hashes are consumed before Kit can preserve a fragment or reuse old media', async ({
	page
}) => {
	const audit = await auditStorage(page);
	const requests = await mockGuest(page);
	await page.goto(`/share#${fixtureToken}`);
	await expectIsolatedGuest(page);
	await page.evaluate((token) => {
		location.hash = token;
	}, fixtureToken);
	await expect.poll(() => requests.filter((request) => request.method === 'POST').length).toBe(2);
	await expectIsolatedGuest(page);
	const before = requests.length;
	await page.evaluate(() => {
		location.hash = 'invalid';
	});
	await expect(
		page.getByRole('heading', { name: 'This link is unavailable.', exact: true })
	).toBeVisible();
	await expect(page.locator('video')).toHaveCount(0);
	expect(new URL(page.url()).hash).toBe('');
	expect(requests.length).toBe(before);
	expect(audit.writes.every((write) => !write.containsToken)).toBe(true);
});
