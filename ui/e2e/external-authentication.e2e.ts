import { expect, test, type Locator, type Page } from '@playwright/test';
import { AuthenticationObservation } from './fixtures/external-authentication-observer';
import {
	startAuthenticationFixture,
	type AuthenticationFixture
} from './fixtures/external-authentication-server';

const sessionCookie = '__Host-keeppeek-session';
type Session = {
	identity: { role: string; display_name: string } | null;
	csrf_token: string | null;
};
let fixture: AuthenticationFixture;

test.describe.configure({ mode: 'default' });
test.use({ ignoreHTTPSErrors: true, trace: 'off', video: 'off' });
test.beforeAll(async () => {
	test.setTimeout(300_000);
	fixture = await startAuthenticationFixture();
});
test.afterAll(async () => {
	await fixture?.close();
});
test.beforeEach(async () => {
	test.setTimeout(90_000);
	await fixture.command('healthy');
	await fixture.command('user');
	await fixture.command('oidc');
});

function observe(page: Page, allowOutage = false) {
	return new AuthenticationObservation(page, fixture, allowOutage);
}
async function session(page: Page): Promise<Session> {
	const response = await page.request.get(`${fixture.origin}/auth/session`, {
		headers: { Origin: fixture.origin }
	});
	expect(response.status()).toBe(200);
	return response.json();
}

async function signIn(
	page: Page,
	observed: AuthenticationObservation,
	screenshotPath?: string
): Promise<void> {
	await page.waitForLoadState('networkidle');
	await observed.settle();
	await page.goto(fixture.origin);
	await expect(page.getByRole('heading', { name: 'Sign in', exact: true })).toBeVisible();
	await expect(
		page.getByRole('button', { name: 'Continue with Fixture sign-in', exact: true })
	).toBeVisible();
	await page.waitForLoadState('networkidle');
	await observed.settle();
	if (screenshotPath) await page.screenshot({ path: screenshotPath, fullPage: true });
	const connected = page.waitForResponse(
		(response) =>
			new URL(response.url()).pathname === '/create' && response.request().method() === 'POST'
	);
	await page.getByRole('button', { name: 'Continue with Fixture sign-in', exact: true }).click();
	expect((await connected).status()).toBe(201);
	await expect(page.getByRole('button', { name: 'Sign out', exact: true })).toBeVisible();
	await expect(page).toHaveURL(`${fixture.origin}/`);
}

async function secureCookie(page: Page): Promise<string> {
	const cookie = (await page.context().cookies(fixture.origin)).find(
		(cookie) => cookie.name === sessionCookie
	);
	expect(Boolean(cookie), 'The server must issue a browser session cookie').toBe(true);
	if (!cookie) throw new Error('Browser session cookie missing.');
	expect(cookie.httpOnly).toBe(true);
	expect(cookie.secure).toBe(true);
	expect(cookie.sameSite).toBe('Lax');
	expect(cookie.path).toBe('/');
	return cookie.value;
}

async function reachable(target: Locator): Promise<void> {
	await target.evaluate((element) => element.scrollIntoView({ block: 'center' }));
	await expect(target).toBeVisible();
	await expect(target).toBeInViewport({ ratio: 1 });
	await expect
		.poll(
			() =>
				target.evaluate((element) => {
					const box = element.getBoundingClientRect();
					return [
						[0.1, 0.1],
						[0.9, 0.1],
						[0.5, 0.5],
						[0.1, 0.9],
						[0.9, 0.9]
					].every(([x, y]) => {
						const hit = document.elementFromPoint(
							box.left + box.width * x,
							box.top + box.height * y
						);
						return hit !== null && element.contains(hit);
					});
				}),
			'The target must not be covered by sticky navigation or the footer'
		)
		.toBe(true);
}

for (const viewport of [
	{ width: 1440, height: 900 },
	{ width: 390, height: 844 }
]) {
	test(`real TLS OIDC User login and CSRF-bound WebRTC at ${viewport.width}px`, async ({
		page
	}, testInfo) => {
		await page.setViewportSize(viewport);
		const observed = observe(page);
		await signIn(page, observed, testInfo.outputPath(`tls-sign-in-${viewport.width}.png`));
		expect((await session(page)).identity?.role).toBe('user');
		await secureCookie(page);
		expect(observed.creates).toEqual([{ status: 201, csrf: true, bearer: false }]);
		await expect(page.getByRole('link', { name: 'Settings', exact: true })).toHaveCount(0);
		expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(
			true
		);
		await observed.verify();
		await page.screenshot({
			path: testInfo.outputPath(`tls-oidc-${viewport.width}.png`),
			fullPage: true
		});
	});
}

test('TLS OIDC logout rotates the session and revocation removes the live RTC session', async ({
	page
}) => {
	const observed = observe(page);
	await signIn(page, observed);
	const first = await secureCookie(page);
	const before = await session(page);
	const rejected = await page.request.post(`${fixture.origin}/auth/logout`, {
		headers: { Origin: fixture.origin }
	});
	expect(rejected.status()).toBe(403);
	expect((await session(page)).identity?.role).toBe('user');
	const loggedOut = page.waitForResponse(
		(response) => new URL(response.url()).pathname === '/auth/logout'
	);
	observed.expectInvalidatedCleanup();
	const logoutCleanup = page.waitForResponse(
		(response) => new URL(response.url()).pathname === '/delete' && response.status() === 401
	);
	await page.getByRole('button', { name: 'Sign out', exact: true }).click();
	const response = await loggedOut;
	await logoutCleanup;
	expect(response.ok()).toBe(true);
	expect(response.request().headers()['x-keeppeek-csrf'] === before.csrf_token).toBe(true);
	await expect(page.getByRole('heading', { name: 'Sign in', exact: true })).toBeVisible();
	expect((await session(page)).identity).toBeNull();
	await signIn(page, observed);
	const revokedHandle = await secureCookie(page);
	expect(revokedHandle !== first, 'A new login must rotate the cookie handle').toBe(true);
	observed.expectInvalidatedCleanup();
	const cleanup = page.waitForResponse(
		(response) => new URL(response.url()).pathname === '/delete' && response.status() === 401
	);
	await fixture.command('revoke');
	await cleanup;
	await expect(page.getByRole('heading', { name: 'Sign in', exact: true })).toBeVisible();
	const login = page.waitForResponse(
		(response) => new URL(response.url()).pathname === '/auth/login'
	);
	await page.getByRole('button', { name: 'Continue with Fixture sign-in', exact: true }).click();
	expect((await login).status(), 'A revoked session must not block a fresh provider login').toBe(
		303
	);
	await expect(page.getByRole('button', { name: 'Sign out', exact: true })).toBeVisible();
	expect((await session(page)).identity?.role).toBe('user');
	expect((await secureCookie(page)) !== revokedHandle).toBe(true);
	expect(observed.creates).toEqual(
		Array.from({ length: 3 }, () => ({ status: 201, csrf: true, bearer: false }))
	);
	await observed.verify();
});

test('provider outage preserves an existing session but does not admit a new login', async ({
	page,
	browser
}) => {
	const existing = observe(page);
	await signIn(page, existing);
	const handle = await secureCookie(page);
	await fixture.command('outage');
	await existing.settle();
	await page.reload();
	await expect(page.getByRole('button', { name: 'Sign out', exact: true })).toBeVisible();
	expect((await secureCookie(page)) === handle).toBe(true);
	expect((await session(page)).identity?.role).toBe('user');
	const context = await browser.newContext({ ignoreHTTPSErrors: true });
	try {
		const newcomer = await context.newPage();
		const observed = observe(newcomer, true);
		await newcomer.goto(fixture.origin);
		await expect(
			newcomer.getByRole('button', { name: 'Continue with Fixture sign-in', exact: true })
		).toBeVisible();
		await newcomer.waitForLoadState('networkidle');
		await observed.settle();
		const denied = newcomer.waitForResponse(
			(response) => response.status() === 503 && new URL(response.url()).origin === fixture.issuer
		);
		await newcomer
			.getByRole('button', { name: 'Continue with Fixture sign-in', exact: true })
			.click();
		expect((await denied).status()).toBe(503);
		expect((await session(newcomer)).identity).toBeNull();
		expect(observed.creates).toHaveLength(0);
		await fixture.command('healthy');
		await signIn(newcomer, observed);
		await observed.verify();
	} finally {
		await context.close();
	}
	await existing.verify();
});

for (const viewport of [
	{ width: 1440, height: 900 },
	{ width: 390, height: 844 }
]) {
	test(`trusted proxy strips caller assertions and establishes a TLS User session at ${viewport.width}px`, async ({
		page
	}, testInfo) => {
		await page.setViewportSize(viewport);
		await fixture.command('proxy');
		await page.setExtraHTTPHeaders({
			'X-KeepPeek-Subject': 'attacker',
			'X-KeepPeek-Role': 'administrator',
			'X-Forwarded-For': '127.0.0.1',
			Forwarded: 'for=127.0.0.1;proto=http'
		});
		const observed = observe(page);
		await page.goto(fixture.origin);
		await expect(page.getByRole('button', { name: 'Sign out', exact: true })).toBeVisible();
		const current = await session(page);
		expect(current.identity?.role).toBe('user');
		expect(current.identity?.display_name).toBe('Fixture User');
		await secureCookie(page);
		expect(observed.creates).toEqual([{ status: 201, csrf: true, bearer: false }]);
		expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(
			true
		);
		await observed.verify();
		await page.screenshot({
			path: testInfo.outputPath(`tls-proxy-${viewport.width}.png`),
			fullPage: true
		});
	});
}

for (const viewport of [
	{ width: 1440, height: 900 },
	{ width: 390, height: 844 }
]) {
	test(`OIDC Administrator settings show provider and identity/session metadata at ${viewport.width}px`, async ({
		page
	}, testInfo) => {
		await page.setViewportSize(viewport);
		await fixture.command('administrator');
		const observed = observe(page);
		await signIn(page, observed);
		expect((await session(page)).identity?.role).toBe('administrator');
		if (viewport.width >= 768)
			await expect(page.getByRole('link', { name: 'Settings', exact: true })).toBeVisible();
		await observed.settle();
		await page.goto(`${fixture.origin}/settings#access`);
		const section = page.getByRole('region', { name: 'External sign-in', exact: true });
		await expect(section.getByLabel('Provider ID', { exact: true })).toHaveValue('company');
		await expect(section.getByLabel('Issuer URL', { exact: true })).toHaveValue(fixture.issuer);
		await expect(section.getByText(/^Subject fingerprint: .+/).first()).toBeVisible();
		await expect(section.getByText(/^Session age: \d+ min$/).first()).toBeVisible();
		await expect(
			section.getByRole('button', { name: 'Revoke identity Fixture User', exact: true }).first()
		).toBeEnabled();
		await expect(
			section.getByRole('button', { name: 'Revoke browser session', exact: true }).first()
		).toBeEnabled();
		expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(
			true
		);
		await reachable(section.getByRole('heading', { name: 'External sign-in', exact: true }));
		await page.screenshot({
			path: testInfo.outputPath(`tls-admin-settings-${viewport.width}.png`)
		});
		await reachable(section.getByRole('group', { name: 'Role mapping 1', exact: true }));
		await page.screenshot({
			path: testInfo.outputPath(`tls-admin-role-mapping-${viewport.width}.png`)
		});
		const identityRevoke = section
			.getByRole('button', { name: 'Revoke identity Fixture User', exact: true })
			.first();
		await reachable(identityRevoke);
		await identityRevoke.click({ trial: true });
		await expect(section.getByText(/^Subject fingerprint: .+/).first()).toBeInViewport({
			ratio: 1
		});
		await page.screenshot({
			path: testInfo.outputPath(`tls-admin-identities-${viewport.width}.png`)
		});
		const sessionRevoke = section
			.getByRole('button', { name: 'Revoke browser session', exact: true })
			.first();
		await reachable(sessionRevoke);
		await sessionRevoke.click({ trial: true });
		await expect(section.getByText(/^Session age: \d+ min$/).first()).toBeInViewport({ ratio: 1 });
		await page.screenshot({
			path: testInfo.outputPath(`tls-admin-sessions-${viewport.width}.png`)
		});
		await observed.verify();
	});
}
