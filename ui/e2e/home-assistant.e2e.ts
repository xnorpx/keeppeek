import { expect, test, type Page } from '@playwright/test';
import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { cpus, release } from 'node:os';
import { resolve } from 'node:path';
import {
	cardSourceIds,
	cardTestKeys,
	cardAdminKey,
	startHomeAssistantServer
} from './fixtures/home-assistant-server';
import {
	installCardProbe,
	interruptCardPeer,
	readCardProbe
} from './fixtures/home-assistant-browser';

test.describe.configure({ mode: 'default' });
let server: Awaited<ReturnType<typeof startHomeAssistantServer>>;
let browserVersion: string;
const modulePath = resolve(
	import.meta.dirname,
	'../../target/home-assistant-card/dist/keeppeek.js'
);

test.beforeAll(async ({ browser }, worker) => {
	browserVersion = browser.version();
	server = await startHomeAssistantServer(new URL(String(worker.project.use.baseURL)).origin);
});
test.afterAll(async () => {
	await server?.close();
});

async function harness(page: Page, target = '/home-assistant-test') {
	await installCardProbe(page);
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	page.on('console', (message) => {
		if (message.type() === 'error') errors.push(message.text());
	});
	await page.route('**/keeppeek.js', (route) =>
		route.fulfill({ contentType: 'text/javascript', path: modulePath })
	);
	await page.route('**/home-assistant-test', (route) =>
		route.fulfill({
			contentType: 'text/html',
			body: `<!doctype html><html lang="en"><head><meta name="viewport" content="width=device-width,initial-scale=1"><title>Home Assistant card harness</title><style>body{margin:0;padding:16px;background:#edf1f3;font-family:sans-serif}main{display:grid;grid-template-columns:repeat(auto-fit,minmax(min(100%,400px),1fr));gap:16px;align-items:start}keeppeek-card{min-width:0}</style><script type="module" src="/keeppeek.js"></script></head><body><main id="dashboard"></main></body></html>`
		})
	);
	await page.goto(target);
	await page
		.context()
		.grantPermissions(['local-network-access'], { origin: new URL(page.url()).origin });
	await page.waitForFunction(() => Boolean(customElements.get('keeppeek-card')));
	return errors;
}

async function mountCards(page: Page, keys: readonly string[], sourceIds: readonly string[]) {
	await page.evaluate(
		({ endpoint, tokens, sources }) => {
			const dashboard = document.querySelector('#dashboard')!;
			for (const token of tokens) {
				const card = document.createElement('keeppeek-card') as HTMLElement & {
					setConfig: (config: unknown) => void;
				};
				card.setConfig({
					type: 'custom:keeppeek-card',
					endpoint,
					token,
					sources: sources.map((source_id, index) => ({
						source_id,
						title: index === 0 ? 'Front entrance' : 'Side entrance'
					}))
				});
				dashboard.append(card);
			}
		},
		{ endpoint: server.url, tokens: [...keys], sources: [...sourceIds] }
	);
}

async function decodedFrames(page: Page, count: number) {
	await expect(page.locator('video')).toHaveCount(count);
	await expect
		.poll(
			() =>
				page.locator('video').evaluateAll((elements) =>
					elements.every((element) => {
						const video = element as HTMLVideoElement;
						return (
							video.videoWidth === 640 &&
							video.videoHeight === 360 &&
							video.getVideoPlaybackQuality().totalVideoFrames >= 2
						);
					})
				),
			{ timeout: 20_000, message: 'Every card video must decode real H.264 frames.' }
		)
		.toBe(true);
}

async function activeSessions(): Promise<number> {
	const response = await fetch(`${server.url}/metrics`, {
		headers: { Authorization: `Bearer ${cardAdminKey}` },
		signal: AbortSignal.timeout(5000)
	});
	if (!response.ok) throw new Error('Fixture metrics are unavailable.');
	const metric = /^keeppeek_webrtc_active_sessions (\d+)$/m.exec(await response.text());
	if (!metric) throw new Error('The active-session gauge is missing.');
	return Number(metric[1]);
}

async function clearCards(page: Page): Promise<void> {
	await page
		.locator('keeppeek-card, keeppeek-card-editor')
		.evaluateAll((elements) => elements.forEach((element) => element.remove()));
	await expect.poll(activeSessions, { timeout: 10_000 }).toBe(0);
	const probe = await readCardProbe(page);
	expect(probe.closed).toBe(probe.created);
	expect(probe.overflow).toBe(false);
}

for (const viewport of [
	{ width: 1440, height: 900 },
	{ width: 390, height: 844 }
]) {
	test(`built card displays two direct live cameras at ${viewport.width}px`, async ({
		page
	}, testInfo) => {
		await page.setViewportSize(viewport);
		const errors = await harness(page);
		const requests: string[] = [];
		page.on('request', (request) => {
			if (request.method() === 'POST' && /\/(create|delete)$/.test(new URL(request.url()).pathname))
				requests.push(request.url());
		});
		await mountCards(page, [cardTestKeys[0]], cardSourceIds);
		await decodedFrames(page, 2);
		expect(requests.filter((url) => url.endsWith('/create'))).toEqual([`${server.url}/create`]);
		expect(
			await page.locator('keeppeek-card').evaluate((card) => card.shadowRoot!.innerHTML)
		).not.toContain(cardTestKeys[0]);
		expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(
			true
		);
		await page.screenshot({
			path: testInfo.outputPath(`home-assistant-${viewport.width}.png`),
			fullPage: true
		});
		const deleted = page.waitForResponse((response) => response.url() === `${server.url}/delete`);
		await page.locator('keeppeek-card').evaluate((card) => card.remove());
		expect((await deleted).ok()).toBe(true);
		expect(errors).toEqual([]);
	});
}

test('three cards share media and reconnect only the remaining consumers', async ({ page }) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	const errors = await harness(page);
	await mountCards(page, [cardTestKeys[0], cardTestKeys[0], cardTestKeys[0]], [cardSourceIds[0]]);
	await decodedFrames(page, 3);
	expect(await readCardProbe(page)).toMatchObject({ created: 1, closed: 0, subscriptions: 1 });
	expect(await activeSessions()).toBe(1);
	await interruptCardPeer(page);
	await page
		.locator('keeppeek-card')
		.first()
		.evaluate((card) => card.remove());
	await decodedFrames(page, 2);
	expect(await readCardProbe(page)).toMatchObject({ created: 2, closed: 1, subscriptions: 2 });
	await page
		.locator('keeppeek-card')
		.first()
		.evaluate((card) => card.remove());
	await decodedFrames(page, 1);
	expect(await activeSessions()).toBe(1);
	await clearCards(page);
	expect(errors).toEqual([]);
});

test('invalid credentials and unknown sources produce isolated actionable states', async ({
	page
}) => {
	const errors = await harness(page);
	await mountCards(page, ['ffffffff-ffff-4fff-afff-ffffffffffff'], [cardSourceIds[0]]);
	await expect(page.getByRole('alert')).toContainText('Access denied');
	await clearCards(page);
	await mountCards(page, [cardTestKeys[0]], [cardSourceIds[0], 'missing-source']);
	await expect(
		page.getByText('Unknown source ID. Choose a source listed by this server.')
	).toBeVisible();
	await expect
		.poll(
			() =>
				page
					.locator('video')
					.first()
					.evaluate(
						(video) => (video as HTMLVideoElement).getVideoPlaybackQuality().totalVideoFrames
					),
			{ timeout: 20_000 }
		)
		.toBeGreaterThan(1);
	expect(errors.join('\n')).not.toContain(cardTestKeys[0]);
	expect(
		await page.evaluate(() => ({ local: localStorage.length, session: sessionStorage.length }))
	).toEqual({ local: 0, session: 0 });
	await clearCards(page);
});

test('an unconfigured origin is rejected before a KeepPeek session is allocated', async ({
	page
}) => {
	const origin = new URL(String(test.info().project.use.baseURL));
	origin.hostname = 'localhost';
	origin.pathname = '/home-assistant-test';
	const errors = await harness(page, origin.href);
	await mountCards(page, [cardTestKeys[0]], [cardSourceIds[0]]);
	await expect(page.getByRole('alert')).toContainText('direct_card.allowed_origins');
	expect(await activeSessions()).toBe(0);
	expect(errors.join('\n')).not.toContain(cardTestKeys[0]);
	await clearCards(page);
});

test('the installed visual editor discovers sources and preserves its credential reference', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 1000 });
	const errors = await harness(page);
	await mountCards(page, [cardTestKeys[0]], [cardSourceIds[0]]);
	await decodedFrames(page, 1);
	await page.evaluate(
		({ endpoint, token, sourceId }) => {
			const editor = document.createElement('keeppeek-card-editor') as HTMLElement & {
				setConfig: (config: unknown) => void;
			};
			editor.setConfig({
				type: 'custom:keeppeek-card',
				endpoint,
				token,
				sources: [{ source_id: sourceId }],
				grid_options: { columns: 12 }
			});
			editor.addEventListener('config-changed', (event) => {
				const config = (
					event as CustomEvent<{ config: { token: string; title: string; grid_options: unknown } }>
				).detail.config;
				Reflect.set(window, 'editorResult', {
					credentialPreserved: config.token === token,
					title: config.title,
					layout: config.grid_options
				});
			});
			document.querySelector('#dashboard')!.append(editor);
		},
		{ endpoint: server.url, token: cardTestKeys[0], sourceId: cardSourceIds[0] }
	);
	await page.getByRole('button', { name: 'Load sources' }).click();
	await expect(page.locator('datalist option')).toHaveCount(2);
	await page.getByLabel('Card title').fill('Entrances');
	expect(await page.evaluate(() => Reflect.get(window, 'editorResult'))).toEqual({
		credentialPreserved: true,
		title: 'Entrances',
		layout: { columns: 12 }
	});
	expect(await page.locator('input[type="password"]').inputValue()).toBe('');
	expect(
		await page.locator('keeppeek-card-editor').evaluate((editor) => editor.shadowRoot!.innerHTML)
	).not.toContain(cardTestKeys[0]);
	expect((await readCardProbe(page)).created).toBe(1);
	await clearCards(page);
	expect(errors).toEqual([]);
});

test('shared connections meet the measured bootstrap and resource budgets', async ({
	page
}, testInfo) => {
	test.setTimeout(120_000);
	await page.setViewportSize({ width: 1440, height: 900 });
	const errors = await harness(page);
	const samples: Record<string, number[]> = { isolated: [], shared: [] };
	for (let run = 0; run < 10; run += 1) {
		for (const mode of run % 2 === 0 ? ['isolated', 'shared'] : ['shared', 'isolated']) {
			const before = await readCardProbe(page);
			const started = performance.now();
			await mountCards(
				page,
				mode === 'isolated' ? cardTestKeys : [cardTestKeys[0], cardTestKeys[0], cardTestKeys[0]],
				[cardSourceIds[0]]
			);
			await decodedFrames(page, 3);
			samples[mode]!.push(performance.now() - started);
			const after = await readCardProbe(page);
			const expectedConnections = mode === 'isolated' ? 3 : 1;
			expect(after.created - before.created).toBe(expectedConnections);
			expect(after.subscriptions - before.subscriptions).toBe(expectedConnections);
			expect(await activeSessions()).toBe(expectedConnections);
			await clearCards(page);
		}
	}
	const summarize = (values: number[]) => {
		const sorted = values.toSorted((left, right) => left - right);
		return {
			runs: sorted.length,
			p50_ms: Number(sorted[4]!.toFixed(1)),
			p95_ms: Number(sorted[9]!.toFixed(1))
		};
	};
	const report = {
		environment: {
			browser: browserVersion,
			os: process.platform,
			os_release: release(),
			arch: process.arch,
			cpu: cpus()[0]?.model,
			runtime: process.version
		},
		isolated: { ...summarize(samples.isolated!), sessions: 3, subscriptions: 3 },
		shared: { ...summarize(samples.shared!), sessions: 1, subscriptions: 1 },
		bootstrap_p95_budget_ms: 10_000,
		resource_reduction_percent: 66.7
	};
	expect(report.shared.p95_ms).toBeLessThan(report.bootstrap_p95_budget_ms);
	await writeFile(testInfo.outputPath('performance.json'), JSON.stringify(report, null, 2) + '\n');
	await testInfo.attach('performance', {
		body: JSON.stringify(report),
		contentType: 'application/json'
	});
	console.log('Home Assistant performance:', JSON.stringify(report));
	expect(errors).toEqual([]);
});

test('the distribution has version and integrity metadata', async () => {
	const metadata = JSON.parse(
		await readFile(resolve(modulePath, '../keeppeek-card.json'), 'utf8')
	) as { version: string; gzip_bytes: number; sha256: string };
	expect(metadata.version).toMatch(/^\d+\.\d+\.\d+/);
	expect(metadata.gzip_bytes).toBeLessThan(500 * 1024);
	expect(metadata.sha256).toBe(
		createHash('sha256')
			.update(await readFile(modulePath))
			.digest('hex')
	);
	expect(
		JSON.parse(await readFile(resolve(import.meta.dirname, '../../hacs.json'), 'utf8'))
	).toMatchObject({ filename: 'keeppeek.js', hide_default_branch: true });
});
