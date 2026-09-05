import { mkdir, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { chromium, type Page } from '@playwright/test';

type IssuedCredential = { credential: { id: string }; accessKey: string };
type BenchmarkClient = {
	checkAccess(): Promise<void>;
	signIn(key: string): Promise<void>;
	getCameras(): Promise<{ id: string; name: string | null }[]>;
	createAccessCredential(input: { name: string; role: 'user' }): Promise<IssuedCredential>;
	revokeAccessCredential(id: string): Promise<unknown>;
	getPeekLayoutRegistry(): Promise<unknown>;
	getNotificationInbox(): Promise<unknown>;
	getNotificationHistory(): Promise<unknown>;
	queryStoredTimeline(options: {
		sourceIds: string[];
		startMs: number;
		endMs: number;
		includeEvents: boolean;
		includeAttachments: boolean;
	}): Promise<unknown>;
	closeOnPageHide(): void;
};
type BenchmarkWindow = typeof window & { accessBenchmark: BenchmarkClient };
type QueryName =
	| 'grid_registry'
	| 'wildcard_timeline_events'
	| 'notification_inbox'
	| 'notification_history'
	| 'recording_coverage';

async function manageCredential(page: Page, revokeId?: string): Promise<IssuedCredential | null> {
	return page.evaluate(async (id) => {
		const modulePath = '/src/lib/control-client.ts';
		const { ControlClient } = await import(modulePath);
		const controller: BenchmarkClient = new ControlClient();
		try {
			await controller.checkAccess();
			if (id) {
				await controller.revokeAccessCredential(id);
				return null;
			}
			const cameras = await controller.getCameras();
			if (cameras.length !== 1 || cameras[0]?.name !== 'e2e-h264') {
				throw new Error('This benchmark requires the disposable logging E2E fixture');
			}
			return await controller.createAccessCredential({
				name: `Access benchmark ${crypto.randomUUID()}`,
				role: 'user'
			});
		} finally {
			controller.closeOnPageHide();
		}
	}, revokeId);
}

async function startUser(page: Page, accessKey: string): Promise<void> {
	await page.evaluate(async (key) => {
		const modulePath = '/src/lib/control-client.ts';
		const { ControlClient } = await import(modulePath);
		const controller: BenchmarkClient = new ControlClient();
		(window as BenchmarkWindow).accessBenchmark = controller;
		await controller.signIn(key);
	}, accessKey);
}

async function measureQuery(page: Page, name: QueryName, key: string) {
	return page.evaluate(
		async ({ name, key }) => {
			const controller = (window as BenchmarkWindow).accessBenchmark;
			if (!controller) throw new Error('The benchmark page reloaded during measurement.');
			const endMs = Date.now();
			const run = async (): Promise<unknown> => {
				switch (name) {
					case 'grid_registry':
						return controller.getPeekLayoutRegistry();
					case 'notification_inbox':
						return controller.getNotificationInbox();
					case 'notification_history':
						return controller.getNotificationHistory();
					case 'wildcard_timeline_events':
						return controller.queryStoredTimeline({
							sourceIds: [],
							startMs: endMs - 60_000,
							endMs,
							includeEvents: true,
							includeAttachments: false
						});
					case 'recording_coverage': {
						const response = await fetch('/recording-coverage', {
							headers: { Authorization: `Bearer ${key}` },
							signal: AbortSignal.timeout(5_000)
						});
						if (!response.ok) throw new Error(`Coverage returned ${response.status}`);
						return response.json();
					}
				}
			};
			const durations: number[] = [];
			const sizes: number[] = [];
			for (let sample = 0; sample < 110; sample++) {
				const started = performance.now();
				const value = await run();
				const elapsed = performance.now() - started;
				if (sample < 10) continue;
				durations.push(elapsed);
				const json = JSON.stringify(value, (_key, item) =>
					typeof item === 'bigint' ? item.toString() : item
				);
				sizes.push(new TextEncoder().encode(json).byteLength);
			}
			return { name, durations, sizes };
		},
		{ name, key }
	);
}

async function measureUser(page: Page, accessKey: string) {
	await startUser(page, accessKey);
	const queries: QueryName[] = [
		'grid_registry',
		'wildcard_timeline_events',
		'notification_inbox',
		'notification_history',
		'recording_coverage'
	];
	const results = [];
	try {
		for (const name of queries) {
			const { durations, sizes } = await measureQuery(page, name, accessKey);
			durations.sort((left, right) => left - right);
			sizes.sort((left, right) => left - right);
			results.push({
				name,
				samples: 100,
				warmups: 10,
				medianMs: durations[49],
				p95Ms: durations[94],
				minMs: durations[0],
				maxMs: durations[99],
				medianDecodedJsonBytes: sizes[49]
			});
		}
		return results;
	} finally {
		await page.evaluate(() => (window as BenchmarkWindow).accessBenchmark?.closeOnPageHide());
	}
}

const [baseUrl, label, output] = process.argv.slice(2);
if (!baseUrl || !label || !output || new URL(baseUrl).hostname !== '127.0.0.1') {
	throw new Error(
		'Usage: benchmark-camera-access.ts <http://127.0.0.1:port> <build-label> <output.json>'
	);
}
const browser = await chromium.launch({ headless: true });
const administrator = await browser.newPage();
let issued: IssuedCredential | null = null;
try {
	await administrator.goto(new URL('/settings#access', baseUrl).href);
	issued = await manageCredential(administrator);
	if (!issued) throw new Error('The benchmark credential was not issued');
	const remote = await browser.newContext({
		extraHTTPHeaders: { 'X-Forwarded-For': '203.0.113.77' }
	});
	try {
		const page = await remote.newPage();
		await page.goto(baseUrl);
		const results = await measureUser(page, issued.accessKey);
		const report = { label, browser: browser.version(), samplesPerQuery: 100, results };
		const outputPath = resolve(output);
		await mkdir(dirname(outputPath), { recursive: true });
		await writeFile(outputPath, `${JSON.stringify(report, null, 2)}\n`);
		console.log(JSON.stringify(report, null, 2));
	} finally {
		await remote.close();
	}
} finally {
	try {
		if (issued) await manageCredential(administrator, issued.credential.id);
	} finally {
		await browser.close();
	}
}
