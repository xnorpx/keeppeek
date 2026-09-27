import { expect, type Page, type Request, type Response } from '@playwright/test';
import type { AuthenticationFixture } from './external-authentication-server';

// These are synthetic fixture values, not production credentials.
const privateMarkers = [
	'synthetic-access-token',
	'synthetic-provider-subject-private',
	'-----BEGIN PRIVATE KEY-----'
];
const jwt = /eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}/;

export class AuthenticationObservation {
	readonly creates: { status: number; csrf: boolean; bearer: boolean }[] = [];
	private errors = new Set<string>();
	private leaks = new Set<string>();
	private handles = new Set<string>();
	private referrers: string[] = [];
	private requests = 0;
	private tasks: Promise<void>[] = [];
	private responses = 0;
	private expectedCleanupDenials = 0;
	private cleanupDenials = 0;
	private cleanupConsoleErrors = 0;

	constructor(
		private readonly page: Page,
		private readonly fixture: AuthenticationFixture,
		allowOutage = false
	) {
		page.on('pageerror', () => this.errors.add('pageerror'));
		page.on('console', (message) => {
			this.inspect(message.text(), 'console');
			const location = new URL(message.location().url || fixture.origin);
			if (
				this.expectedCleanupDenials > 0 &&
				message.type() === 'error' &&
				location.origin === fixture.origin &&
				location.pathname === '/delete' &&
				/^Failed to load resource: the server responded with a status of 401\b/.test(message.text())
			) {
				this.cleanupConsoleErrors++;
				return;
			}
			if (
				message.type() === 'error' &&
				!(
					allowOutage &&
					location.origin === fixture.issuer &&
					location.pathname === '/authorize' &&
					/Failed to load resource.*503/.test(message.text())
				)
			)
				this.errors.add(
					`console error: ${message.text().match(/net::[A-Z_]+|status of \d+|CORS/)?.[0] ?? 'application error'} at ${new URL(message.location().url || fixture.origin).pathname}`
				);
		});
		page.on('request', (request) => {
			this.inspect(request.url(), 'request URL');
			if (++this.requests <= 512) this.tasks.push(this.inspectReferrer(request));
			else this.leaks.add('request observation exceeded bound');
			if (new URL(request.url()).origin === fixture.origin && request.headers().authorization)
				this.leaks.add('browser Authorization header');
		});
		page.on('response', (response) => this.response(response));
	}
	expectInvalidatedCleanup(): void {
		this.expectedCleanupDenials++;
	}
	private inspect(text: string, surface: string): void {
		if (privateMarkers.some((marker) => text.includes(marker)) || jwt.test(text))
			this.leaks.add(surface);
	}
	private async inspectReferrer(request: Request): Promise<void> {
		try {
			const referrer = (await request.allHeaders()).referer ?? '';
			if (referrer.length > 16384) {
				this.leaks.add('referrer observation exceeded bound');
				return;
			}
			this.inspect(referrer, 'request referrer');
			this.referrers.push(referrer);
		} catch {
			this.errors.add('unreadable request referrer');
		}
	}
	private response(response: Response): void {
		if (++this.responses > 512) {
			this.leaks.add('response observation exceeded bound');
			return;
		}
		const url = new URL(response.url());
		if (url.origin !== this.fixture.origin) return;
		if (
			this.expectedCleanupDenials > 0 &&
			url.pathname === '/delete' &&
			response.request().method() === 'POST' &&
			response.status() === 401
		)
			this.cleanupDenials++;
		this.tasks.push(this.inspectCookies(response));
		if (url.pathname === '/create')
			this.creates.push({
				status: response.status(),
				csrf: Boolean(response.request().headers()['x-keeppeek-csrf']),
				bearer: Boolean(response.request().headers().authorization)
			});
	}
	private async inspectCookies(response: Response): Promise<void> {
		try {
			for (const header of await response.headersArray()) {
				if (header.name.toLowerCase() !== 'set-cookie') continue;
				const handle = /^__Host-keeppeek-(?:session|login)=([^;]+)/.exec(header.value)?.[1];
				if (handle) this.handles.add(handle);
			}
		} catch {
			this.errors.add('unreadable authentication response headers');
		}
	}
	async settle(): Promise<void> {
		await Promise.all(this.tasks);
	}
	async verify(): Promise<void> {
		await this.settle();
		if (this.expectedCleanupDenials > 0) {
			expect(this.cleanupDenials, 'Invalidated cookies must be rejected during RTC cleanup').toBe(
				this.expectedCleanupDenials
			);
			expect(
				this.cleanupConsoleErrors,
				'Only the corresponding denied cleanup resource is expected'
			).toBe(this.expectedCleanupDenials);
		}
		const cookies = await this.page.context().cookies(this.fixture.origin);
		const currentHandles = cookies
			.filter((cookie) => cookie.name.startsWith('__Host-keeppeek-'))
			.map((cookie) => cookie.value)
			.filter(Boolean);
		const markers = [...privateMarkers, 'synthetic-code', ...this.handles, ...currentHandles];
		expect(
			this.referrers.some((referrer) => markers.some((marker) => referrer.includes(marker))),
			'No callback code, provider material, or cookie handle may reach any request referrer'
		).toBe(false);
		const clean = await this.page.evaluate(
			({ markers }) => {
				const storage = [localStorage, sessionStorage]
					.flatMap((store) =>
						Array.from({ length: store.length }, (_, index) => {
							const key = store.key(index) ?? '';
							return `${key}:${store.getItem(key)}`;
						})
					)
					.join('\n');
				const exposed = [
					location.href,
					document.documentElement.outerHTML,
					storage,
					document.cookie
				].join('\n');
				return (
					!markers.some((marker) => exposed.includes(marker)) &&
					!/eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}/.test(exposed)
				);
			},
			{ markers }
		);
		expect(
			clean,
			'No provider secrets or HttpOnly cookie handles may reach DOM, storage, or URLs'
		).toBe(true);
		expect([...this.leaks], 'Secret or bearer artifacts must not cross browser surfaces').toEqual(
			[]
		);
		expect([...this.errors], 'The production UI must not emit console errors').toEqual([]);
		const audit = JSON.stringify(await this.fixture.audit());
		expect(
			markers.some((marker) => audit.includes(marker)),
			'Server audit and logs must exclude provider material and cookie handles'
		).toBe(false);
		expect(jwt.test(audit), 'Server logs must not contain provider ID tokens').toBe(false);
	}
}
