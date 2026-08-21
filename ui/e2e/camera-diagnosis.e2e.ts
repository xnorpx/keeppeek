import { expect, test, type Page } from '@playwright/test';
import { mockControlPeer, type HealthFixture } from './fixtures/control-peer';
import { diagnosisHealth, diagnosisVisualHealth } from './fixtures/diagnosis';

const health: HealthFixture = diagnosisHealth;

async function mockDiagnosis(page: Page): Promise<string[]> {
	const writes: string[] = [];
	await mockControlPeer(page, { health });
	page.on('request', (request) => {
		const pathname = new URL(request.url()).pathname;
		if (request.method() !== 'GET' && pathname !== '/create' && pathname !== '/delete') {
			writes.push(`${request.method()} ${request.url()}`);
		}
	});
	return writes;
}

test('diagnoses a camera from server evidence without inventing outage history', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	const writes = await mockDiagnosis(page);

	await page.goto('/system-health/camera/back-yard');

	await expect(page).toHaveTitle('Back Yard - KeepPeek');
	await expect(page.getByRole('link', { name: 'Health', exact: true })).toHaveAttribute(
		'aria-current',
		'page'
	);
	await expect(page.getByRole('heading', { name: 'Back Yard', exact: true })).toBeVisible();
	await expect(page.getByText('offline', { exact: true })).toBeVisible();
	await expect(page.getByText('192.0.2.83 · Reolink RLC-820A · main')).toBeVisible();

	const diagnosis = page.getByRole('region', {
		name: 'No stream health report has been received'
	});
	await expect(diagnosis).toContainText('Connection refused');
	await expect(diagnosis).toContainText('No stream report available');
	await expect(diagnosis).toContainText('Unavailable');
	await expect(diagnosis).toContainText('Unavailable in health API');

	await expect(page.getByRole('button', { name: 'Retry now' })).toBeDisabled();
	await expect(page.getByRole('link', { name: 'Open camera page' })).toHaveAttribute(
		'href',
		'/camera?camera=back-yard'
	);
	await expect(page.getByRole('link', { name: 'Open 192.0.2.83' })).toHaveAttribute(
		'href',
		'http://192.0.2.83'
	);
	await expect(page.getByRole('button', { name: 'Probe unavailable' })).toBeDisabled();
	await expect(page.getByText('Switch to TCP', { exact: true })).toBeVisible();
	await expect(page.getByText('Server update required · keeppeek.runtime-config.v1')).toBeVisible();
	await expect(page.getByRole('link', { name: 'Open logs' })).toHaveAttribute(
		'href',
		'/settings/logs'
	);

	const summary = page.getByRole('complementary', { name: 'Diagnosis evidence summary' });
	await expect(summary.getByText('Drops', { exact: true }).locator('..')).toContainText(
		'Unavailable'
	);
	await expect(summary).toContainText('1 camera currently reports online');
	await expect(summary).toContainText(
		'1 camera other than this one is degraded, stale, or offline'
	);
	await expect(summary).toContainText('Gap start unavailable');
	await expect(page.getByText('2h 14m', { exact: true })).toHaveCount(0);
	await expect(page.getByText('27', { exact: true })).toHaveCount(0);
	await expect(page.locator('input[type="password"]')).toHaveCount(0);
	expect(writes).toEqual([]);
});

test('switches an advertised diagnosis transport through WebRTC control', async ({ page }) => {
	const controls = await mockControlPeer(page, {
		health,
		capabilityIds: ['keeppeek.runtime-config.v1'],
		cameraUpdateResult: {
			camera: {
				id: 'back-yard',
				ip: '192.0.2.83',
				display_name: 'Back Yard',
				manufacturer_override: null,
				username_configured: true,
				password_configured: true,
				onvif_port: null,
				http_port: null,
				main_rtsp_url: null,
				sub_rtsp_url: null,
				uid_configured: false,
				backend: 'retina',
				transport: 'tcp',
				health: 'offline',
				model: 'RLC-820A'
			},
			restart_required: true
		}
	});
	await page.goto('/system-health/camera/back-yard');

	const action = page.getByRole('button', { name: 'Switch to TCP' });
	await expect(action).toBeEnabled();
	await action.click();

	await expect(page.getByText('Transport saved. Apply the pending restart')).toBeVisible();
	await expect(
		page.getByRole('heading', { name: 'Review transport and ports', exact: true })
	).toBeVisible();
	expect(controls.cameraUpdates).toEqual([{ ip: '192.0.2.83', update: { transport: 'tcp' } }]);
});

test('renders Board 26 mobile issue evidence without gap or retry history', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await mockDiagnosis(page);

	await page.goto('/system-health/camera/back-yard');

	const diagnosis = page.locator('[data-mobile-camera-diagnosis="issue"]');
	await expect(diagnosis).toBeVisible();
	await expect(diagnosis).toContainText('Connection refused');
	await expect(diagnosis).toContainText('RECORDING GAP START UNAVAILABLE');
	await expect(diagnosis).toContainText('Latest stream report');
	await expect(diagnosis).toContainText('Next retry');
	await expect(diagnosis).toContainText('Unavailable');
	await expect(diagnosis).toContainText('Credential probe unavailable');
	await expect(diagnosis).toContainText('Retry unavailable');
	await expect(diagnosis).not.toContainText('NO FOOTAGE SINCE');
	await expect(diagnosis).not.toContainText('18s');
	expect(
		await diagnosis.evaluate((element) =>
			Math.round(element.children[1].getBoundingClientRect().height)
		)
	).toBe(660);
	await expect(page.locator('[data-shell-mobile-nav]')).toHaveCount(0);
});

test('renders Board 26 current stream evidence and switches TCP through WebRTC', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	const controls = await mockControlPeer(page, {
		health: diagnosisVisualHealth,
		capabilityIds: ['keeppeek.runtime-config.v1'],
		cameraUpdateResult: {
			camera: {
				id: 'porch',
				ip: '192.168.1.59',
				display_name: 'Porch',
				manufacturer_override: null,
				username_configured: true,
				password_configured: true,
				onvif_port: null,
				http_port: null,
				main_rtsp_url: null,
				sub_rtsp_url: null,
				uid_configured: false,
				backend: 'retina',
				transport: 'tcp',
				health: 'degraded',
				model: null
			},
			restart_required: true
		}
	});

	await page.goto('/system-health/camera/porch');

	const diagnosis = page.locator('[data-mobile-camera-diagnosis="stream"]');
	await expect(diagnosis).toContainText('184,000');
	await expect(diagnosis).toContainText('History unavailable');
	await expect(diagnosis).toContainText('NO CAUSAL CONFIDENCE');
	await expect(diagnosis).not.toContainText('LOSS 24H');
	await expect(diagnosis).not.toContainText('HIGH CONFIDENCE');
	const action = page.getByRole('button', { name: 'Switch to TCP and test' });
	await expect(action).toBeEnabled();
	await action.click();
	await expect(diagnosis).toContainText('Transport saved. Apply the pending restart');
	expect(controls.cameraUpdates).toEqual([{ ip: '192.168.1.59', update: { transport: 'tcp' } }]);
	await expect(page.locator('[data-shell-mobile-nav]')).toHaveCount(0);
});
