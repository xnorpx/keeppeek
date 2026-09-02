import { expect, test } from '@playwright/test';
import { execFile } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { promisify } from 'node:util';
import { mockControlPeer } from './fixtures/control-peer';

const execFileAsync = promisify(execFile);
const keeppeekBinary = resolve(process.cwd(), '../target/release/keeppeek');
const backendURL = `http://127.0.0.1:${process.env.KEEPPEEK_E2E_BACKEND_PORT ?? '4317'}`;

const runtimeConfiguration = {
	host: '0.0.0.0',
	port: 3000,
	camera_count: 0,
	storage: {
		medium_term_path: '/recordings',
		long_term_path: '/recordings',
		recording_catalog_path: '/recordings/recordings.db',
		event_thumbnail_path: '/recordings/.event-thumbnails',
		event_thumbnail_max_mb: 1024,
		short_term_secs: 120,
		medium_term_secs: 1800,
		flush_interval_secs: 60,
		write_buffer_bytes: 8192,
		long_term_max_gb: 2048
	},
	recording_estimate: {
		estimated_bitrate_bps: 0,
		bytes_per_day: 0,
		known_streams: 0,
		unknown_streams: 0,
		estimated_retention_days: null
	}
};

test('non-interactive CLI completes the managed ProtoJSON lifecycle', async () => {
	const capabilities = await backupCli(['capabilities']);
	expect(capabilities.contractId).toBe('keeppeek.backup.v1');
	expect(capabilities.supportedSections).toContain('BACKUP_SECTION_RUNTIME_CONFIG');

	const created = await backupCli([
		'create',
		'--section',
		'runtime-config',
		'--section',
		'camera-database'
	]);
	expect(created.state).toBe('BACKUP_STATE_READY');
	expect(created.archiveSha256).toMatch(/^[0-9a-f]{64}$/);

	const listed = await backupCli(['list']);
	expect(
		listed.backups.some((backup: { backupId: string }) => backup.backupId === created.backupId)
	).toBe(true);
	const inspected = await backupCli(['inspect', created.backupId]);
	expect(inspected.archiveSha256).toBe(created.archiveSha256);

	const plan = await backupCli(['dry-run', created.backupId]);
	expect(plan.backupId).toBe(created.backupId);
	expect(plan.archiveSha256).toBe(created.archiveSha256);
	expect(plan.restartImpact.serverRestartRequired).toBe(true);

	const deleted = await backupCli(['delete', created.backupId]);
	expect(deleted).toEqual({ backupId: created.backupId, deleted: true });

	await expect(
		execFileAsync(
			keeppeekBinary,
			['backup', '--server', backendURL, 'inspect', 'not-a-backup-id'],
			{ maxBuffer: 32 * 1024 * 1024 }
		)
	).rejects.toMatchObject({ code: 3, stdout: '' });
});

test('Administrator creates, inspects, dry-runs, downloads, and deletes a backup', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	await mockControlPeer(page, {
		runtimeConfiguration,
		capabilityIds: ['keeppeek.backup.v1']
	});
	await page.goto('/settings#backups');
	const section = page.getByRole('region', { name: 'Backup and restore' });
	await expect(section).toBeVisible();
	await expect(section.getByText('No managed backups.')).toBeVisible();

	await section.getByRole('button', { name: 'Create backup' }).click();
	await expect(section.getByText('No managed backups.')).toHaveCount(0);
	await expect(section.getByText('SHA-256', { exact: false })).toBeVisible();
	await expect(section.getByText('runtime config', { exact: true })).toBeVisible();
	await expect(section.getByText('recording catalog', { exact: true }).first()).toBeVisible();

	await section.getByRole('button', { name: 'Run dry check' }).click();
	await expect(section.getByText(/target configuration changed/)).toHaveCount(0);
	await expect(section.getByText(/does not provide required reference/).first()).toBeVisible();
	await expect(section.getByText('External secrets required:', { exact: false })).toBeVisible();
	await expect(section.getByRole('button', { name: 'Stage restore' })).toBeDisabled();

	const downloadPromise = page.waitForEvent('download');
	await section.getByRole('button', { name: /^Download / }).click();
	const download = await downloadPromise;
	expect(download.suggestedFilename()).toMatch(/^keeppeek-backup-.*\.zip$/);
	const downloadPath = await download.path();
	expect(downloadPath).not.toBeNull();
	expect((await readFile(downloadPath!)).byteLength).toBeGreaterThan(0);
	await expect(page).toHaveURL(/\/settings#backups$/);
	await expect(section).toBeVisible();

	const deleteResponsePromise = page.waitForResponse(
		(response) =>
			new URL(response.url()).pathname === '/api/backups/delete' &&
			response.request().method() === 'POST'
	);
	await section.getByRole('button', { name: /^Delete / }).click();
	const deleteResponse = await deleteResponsePromise;
	expect(deleteResponse.status(), await deleteResponse.text()).toBe(200);
	await expect(section.getByText('No managed backups.')).toBeVisible();
});

test('backup workflow fits mobile without horizontal overflow', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	const controls = await mockControlPeer(page, {
		runtimeConfiguration,
		capabilityIds: ['keeppeek.backup.v1']
	});
	await page.goto('/settings#backups');
	await expect(page.getByRole('region', { name: 'Backup and restore' })).toBeVisible();
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
	await controls.publishCapabilities([]);
	await expect(page.getByRole('region', { name: 'Backup and restore' })).toHaveCount(0);
});

test('backup workflow remains hidden from User principals', async ({ page }) => {
	await mockControlPeer(page, {
		runtimeConfiguration,
		accessRole: 'user',
		capabilityIds: ['keeppeek.backup.v1']
	});
	await page.goto('/settings#backups');
	await expect(page.getByRole('region', { name: 'Backup and restore' })).toHaveCount(0);
});

async function backupCli(command: string[]): Promise<Record<string, any>> {
	const result = await execFileAsync(
		keeppeekBinary,
		['backup', '--server', backendURL, ...command],
		{ maxBuffer: 32 * 1024 * 1024 }
	);
	expect(result.stderr).toBe('');
	return JSON.parse(result.stdout) as Record<string, any>;
}
