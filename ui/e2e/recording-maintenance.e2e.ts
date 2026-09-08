import { expect, test } from '@playwright/test';
import { spawn, type ChildProcess } from 'node:child_process';
import { createServer } from 'node:net';
import { readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';

test.describe.configure({ mode: 'serial' });
let backend: ChildProcess | undefined;
let backendPort = 0;
let serverOutput = '';
const runId = `maintenance-${process.pid}`;
const root = path.resolve('..', 'target', `ui-logging-e2e-${runId}`);
const now = Date.now();

test.beforeAll(async ({ request }) => {
	const listener = createServer();
	await new Promise<void>((resolve) => listener.listen(0, '127.0.0.1', resolve));
	const address = listener.address();
	if (!address || typeof address === 'string') throw new Error('Test port unavailable');
	backendPort = address.port;
	await new Promise<void>((resolve, reject) =>
		listener.close((error) => (error ? reject(error) : resolve()))
	);
	backend = spawn('bun', ['scripts/start-logging-e2e-server.ts'], {
		env: {
			...process.env,
			KEEPPEEK_E2E_RUN_ID: runId,
			KEEPPEEK_E2E_BACKEND_PORT: String(backendPort),
			KEEPPEEK_E2E_SEED_AGE_SECONDS: '1200',
			KEEPPEEK_E2E_SEED_STABLE_ID: '1'
		},
		stdio: ['ignore', 'pipe', 'pipe']
	});
	backend.stdout?.on('data', (bytes: Buffer) => {
		serverOutput = (serverOutput + bytes.toString()).slice(-64_000);
	});
	backend.stderr?.on('data', (bytes: Buffer) => {
		serverOutput = (serverOutput + bytes.toString()).slice(-64_000);
	});
	await expect
		.poll(
			async () => {
				if (backend?.exitCode !== null)
					throw new Error(`Maintenance fixture exited: ${serverOutput}`);
				return request.get(`http://127.0.0.1:${backendPort}/metrics`).then(
					(response) => response.ok(),
					() => false
				);
			},
			{ timeout: 60_000 }
		)
		.toBe(true);
});

test.afterAll(async () => {
	if (!backend || backend.exitCode !== null) return;
	const exited = new Promise<void>((resolve) => backend?.once('exit', () => resolve()));
	backend.kill('SIGTERM');
	await exited;
});

test('previews, cancels and deletes exact synthetic recordings with desktop and mobile parity', async ({
	page
}, info) => {
	test.setTimeout(90_000);
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	page.on('console', (message) => {
		if (message.type() === 'error') errors.push(message.text());
	});
	await page.route('**/create', (route) =>
		route.continue({ url: `http://127.0.0.1:${backendPort}/create` })
	);
	await page.route('**/delete', (route) =>
		route.continue({ url: `http://127.0.0.1:${backendPort}/delete` })
	);
	await writeFile(path.join(root, 'recordings', 'unindexed.mp4'), Buffer.from([24, 42, 64]));
	await page.setViewportSize({ width: 1440, height: 900 });
	await page.goto(`/recordings/maintenance?start=${now - 22 * 60_000}&end=${now - 14 * 60_000}`);
	await expect(page.getByRole('button', { name: 'Preview deletion' })).toBeEnabled();
	await page.getByRole('button', { name: 'Preview deletion' }).click();
	const dialog = page.getByRole('dialog');
	await expect(
		dialog.getByRole('heading', { name: 'Delete 1 recording permanently?' })
	).toBeVisible();
	await expect(dialog.getByRole('button', { name: 'Delete permanently' })).toBeDisabled();
	await page.screenshot({ path: info.outputPath('maintenance-desktop.png'), fullPage: true });
	await dialog.getByRole('button', { name: 'Keep recordings' }).click();
	await page.getByRole('button', { name: 'Cancel job' }).click();
	await expect(page.getByRole('heading', { name: 'cancelled', exact: true })).toBeVisible();
	await page.setViewportSize({ width: 390, height: 844 });
	await page.getByRole('button', { name: 'Preview deletion' }).click();
	await expect(dialog).toBeVisible();
	await dialog.getByLabel('Type DELETE 1').fill('DELETE 1');
	await page.screenshot({ path: info.outputPath('maintenance-mobile.png'), fullPage: true });
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
		.toBe(true);
	await dialog.getByRole('button', { name: 'Delete permanently' }).click();
	await expect(page.getByRole('heading', { name: 'deleted', exact: true })).toBeVisible({
		timeout: 15_000
	});
	const downloaded = page.waitForEvent('download');
	await page.getByRole('button', { name: 'Download report', exact: true }).click();
	const report = await downloaded;
	const data = JSON.parse(await readFile((await report.path())!, 'utf8'));
	expect(data.deletedCount).toBe(1);
	expect(data.confirmationNonce).toBeUndefined();
	await page.getByRole('button', { name: 'Reconciliation', exact: true }).click();
	await page.getByRole('button', { name: 'Inspect catalog' }).click();
	await expect(page.getByText('unindexed.mp4', { exact: true })).toBeVisible();
	await expect(
		page
			.getByRole('listitem')
			.filter({ hasText: 'unindexed.mp4' })
			.getByText('unknown file', { exact: true })
	).toBeVisible();
	expect(await readFile(path.join(root, 'recordings', 'unindexed.mp4'))).toEqual(
		Buffer.from([24, 42, 64])
	);
	expect(errors).toEqual([]);
});
