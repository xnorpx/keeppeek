import { expect, test } from '@playwright/test';
import type {
	ConfigurationApplyResult,
	ConfigurationPlan,
	SanitizedConfig,
	ConfigurationTemplate,
	ConfigurationTemplateImportPreview
} from '../src/lib/types';
import { cameraFleetConfiguration, mockCameraFleet } from './fixtures/camera-fleet';

const template: ConfigurationTemplate = {
	template_id: 'outdoor',
	version: 1,
	name: 'Outdoor cameras',
	description: 'Outdoor recording policy',
	values: {
		backend: 'reo-proto',
		transport: 'tcp',
		recording_mode: 'event-boost',
		event_recording_duration_secs: 90
	},
	created_at_ms: 1_777_000_000_000,
	updated_at_ms: 1_777_000_000_000
};

const runtimeConfiguration = {
	host: '127.0.0.1',
	port: 8081,
	configuration_revision: 'runtime-revision-1',
	camera_count: 3,
	storage: {
		medium_term_path: '/recordings',
		long_term_path: '/recordings',
		recording_catalog_path: '/recordings/recordings.db',
		event_thumbnail_path: '/recordings/.event-thumbnails',
		event_thumbnail_max_mb: 16,
		short_term_secs: 5,
		medium_term_secs: 60,
		flush_interval_secs: 1,
		write_buffer_bytes: 8192,
		long_term_max_gb: 0
	},
	recording_estimate: {
		estimated_bitrate_bps: 0,
		bytes_per_day: 0,
		known_streams: 0,
		unknown_streams: 3,
		estimated_retention_days: null
	}
} satisfies SanitizedConfig;

function backendPlan(revision = 'configuration-revision-1'): ConfigurationPlan {
	return {
		plan_id: 'plan-backend',
		configuration_revision: revision,
		expires_at_ms: Date.now() + 600_000,
		authoritative_target_count: 1,
		targets: [
			{
				camera_id: 'porch',
				display_name: 'Porch',
				group_ids: ['exterior'],
				skipped: false,
				skip_reason: null
			}
		],
		changes: [
			{
				camera_id: 'porch',
				field: 'backend',
				old_configured_value: 'inherited',
				old_effective_value: 'auto',
				new_configured_value: 'retina',
				new_effective_value: 'retina',
				source: 'override',
				secret: false
			}
		],
		issues: [],
		impact: 'reconnect-camera',
		valid: true,
		apply_semantics:
			'Named fields become explicit camera overrides; untouched fields and secret references are preserved.'
	};
}

function appliedResult(): ConfigurationApplyResult {
	const snapshot = cameraFleetConfiguration(3, 'configuration-revision-2');
	const porch = snapshot.cameras.find((camera) => camera.camera.id === 'porch');
	if (!porch) throw new Error('Porch configuration fixture is missing');
	porch.camera.backend = 'retina';
	porch.backend.camera_override = 'retina';
	porch.backend.effective = 'retina';
	porch.backend.source = 'override';
	porch.backend.runtime_applied = false;
	porch.backend.warning = 'The persisted value is not currently applied.';
	return {
		plan_id: 'plan-backend',
		configuration_committed: true,
		snapshot,
		activations: [
			{
				camera_id: 'porch',
				status: 'restart-required',
				detail: 'Configuration is committed; restart the server to activate this camera.'
			}
		],
		impact: 'reconnect-camera'
	};
}

test('previews exact selected-camera changes before apply and reports activation', async ({
	page
}) => {
	const initial = cameraFleetConfiguration();
	const controls = await mockCameraFleet(page, 3, {
		capabilityIds: ['keeppeek.configuration.v1'],
		configurationSnapshots: [initial],
		configurationPlanResult: backendPlan(),
		configurationApplyResult: appliedResult()
	});
	await page.goto('/cameras');

	await page.getByRole('checkbox', { name: 'Select Porch' }).check();
	await page.getByRole('button', { name: 'Manage selected' }).click();
	const dialog = page.getByRole('dialog', { name: 'Camera fleet configuration' });
	await expect(dialog).toBeVisible();
	await dialog.locator('#policy-backend-operation').selectOption('set');
	await dialog.locator('#policy-backend-value').selectOption('retina');
	await dialog.getByRole('button', { name: 'Preview exact changes' }).click();

	await expect(dialog.getByRole('heading', { name: 'Review configuration changes' })).toBeVisible();
	await expect(dialog.getByText('1 TARGETS', { exact: true })).toBeVisible();
	await expect(dialog.getByText('RECONNECT CAMERA', { exact: true })).toBeVisible();
	await expect(dialog.getByRole('cell', { name: 'auto', exact: true })).toBeVisible();
	await expect(dialog.getByRole('cell', { name: 'retina', exact: true })).toBeVisible();
	await dialog.getByRole('button', { name: 'Apply exact plan' }).click();

	await expect(dialog.getByRole('heading', { name: 'Configuration committed' })).toBeVisible();
	await expect(dialog.getByText('RESTART REQUIRED', { exact: true })).toBeVisible();
	expect(controls.configurationActions).toEqual(['get', 'plan', 'apply']);
	expect(controls.configurationPlans).toEqual([{ target: 'cameraIds', change: 'patch' }]);
});

test('reloads current evidence after a conflict and keeps the complete bulk draft', async ({
	page
}) => {
	const current = cameraFleetConfiguration(3, 'configuration-revision-2');
	await mockCameraFleet(page, 3, {
		capabilityIds: ['keeppeek.configuration.v1'],
		configurationSnapshots: [cameraFleetConfiguration(), current],
		configurationPlanResult: backendPlan(),
		configurationApplyConflictRevision: current.configuration_revision
	});
	await page.goto('/cameras');

	await page.getByRole('checkbox', { name: 'Select Porch' }).check();
	await page.getByRole('button', { name: 'Manage selected' }).click();
	const dialog = page.getByRole('dialog', { name: 'Camera fleet configuration' });
	await dialog.locator('#policy-backend-operation').selectOption('set');
	await dialog.locator('#policy-backend-value').selectOption('retina');
	await dialog.getByRole('button', { name: 'Preview exact changes' }).click();
	await dialog.getByRole('button', { name: 'Apply exact plan' }).click();

	await expect(dialog.getByRole('alert')).toContainText(
		'configuration changed after this editor was opened'
	);
	await expect(dialog.getByText(/CURRENT config/)).toBeVisible();
	await dialog.getByRole('button', { name: 'Back to draft' }).click();
	await expect(dialog.locator('#policy-backend-operation')).toHaveValue('set');
	await expect(dialog.locator('#policy-backend-value')).toHaveValue('retina');
});

test('creates a template and preserves edits when the editor is cancelled', async ({ page }) => {
	const created = cameraFleetConfiguration(3, 'configuration-revision-2', [template]);
	const controls = await mockCameraFleet(page, 3, {
		capabilityIds: ['keeppeek.configuration.v1'],
		configurationSnapshots: [cameraFleetConfiguration(), created],
		configurationTemplateResult: template
	});
	await page.goto('/cameras');
	await page.getByRole('button', { name: 'Configuration', exact: true }).click();
	const dialog = page.getByRole('dialog', { name: 'Camera fleet configuration' });
	await dialog.getByRole('tab', { name: 'Templates' }).click();
	await dialog.getByRole('button', { name: 'New template' }).click();
	await dialog.getByLabel('Name', { exact: true }).fill('Outdoor cameras');
	await dialog.getByLabel('Backend').selectOption('reo-proto');
	await dialog.getByLabel('Recording mode').selectOption('event-boost');
	await dialog.getByRole('button', { name: 'Create template' }).click();

	await expect(dialog.getByRole('heading', { name: 'Outdoor cameras' })).toBeVisible();
	await dialog.getByRole('button', { name: 'Edit' }).click();
	await dialog.getByLabel('Name', { exact: true }).fill('Unsaved template name');
	await dialog.getByRole('button', { name: 'Cancel' }).click();
	await expect(dialog.getByRole('heading', { name: 'Outdoor cameras' })).toBeVisible();
	await expect(dialog.getByLabel('Name', { exact: true })).toHaveCount(0);
	expect(controls.configurationActions).toEqual(['get', 'saveTemplate', 'get']);
});

test('duplicates, edits, and confirms deletion of versioned templates', async ({ page }) => {
	const duplicate: ConfigurationTemplate = {
		...template,
		template_id: 'outdoor-copy',
		name: 'Outdoor cameras copy'
	};
	const edited: ConfigurationTemplate = {
		...duplicate,
		version: 2,
		name: 'Outdoor copy edited',
		updated_at_ms: duplicate.updated_at_ms + 1_000
	};
	const controls = await mockCameraFleet(page, 3, {
		capabilityIds: ['keeppeek.configuration.v1'],
		configurationSnapshots: [
			cameraFleetConfiguration(3, 'configuration-revision-1', [template]),
			cameraFleetConfiguration(3, 'configuration-revision-2', [template, duplicate]),
			cameraFleetConfiguration(3, 'configuration-revision-3', [template, edited]),
			cameraFleetConfiguration(3, 'configuration-revision-4', [template])
		],
		configurationTemplateResults: [duplicate, edited]
	});
	await page.goto('/cameras');
	await page.getByRole('button', { name: 'Configuration', exact: true }).click();
	const dialog = page.getByRole('dialog', { name: 'Camera fleet configuration' });
	await dialog.getByRole('tab', { name: 'Templates' }).click();
	const original = dialog.locator('article').filter({ hasText: 'Outdoor cameras' });
	await original.getByRole('button', { name: 'Duplicate' }).click();

	await expect(dialog.getByLabel('Name', { exact: true })).toHaveValue('Outdoor cameras copy');
	await dialog.getByLabel('Name', { exact: true }).fill('Outdoor copy edited');
	await dialog.getByRole('button', { name: 'Save template' }).click();

	const editedArticle = dialog.locator('article').filter({ hasText: 'Outdoor copy edited' });
	await expect(editedArticle).toBeVisible();
	await editedArticle.getByRole('button', { name: 'Delete', exact: true }).click();
	await editedArticle.getByRole('button', { name: 'Confirm delete' }).click();

	await expect(
		dialog.getByRole('heading', { name: 'Outdoor copy edited', exact: true })
	).toHaveCount(0);
	await expect(dialog.getByRole('heading', { name: 'Outdoor cameras', exact: true })).toBeVisible();
	expect(controls.configurationActions).toEqual([
		'get',
		'duplicateTemplate',
		'get',
		'saveTemplate',
		'get',
		'deleteTemplate'
	]);
});

test('previews and applies a bounded template import', async ({ page }) => {
	const imported = cameraFleetConfiguration(3, 'configuration-revision-2', [template]);
	const importPreview: ConfigurationTemplateImportPreview = {
		preview_id: 'import-1',
		configuration_revision: 'configuration-revision-1',
		expires_at_ms: Date.now() + 600_000,
		templates: [template],
		issues: [],
		valid: true
	};
	const controls = await mockCameraFleet(page, 3, {
		capabilityIds: ['keeppeek.configuration.v1'],
		configurationSnapshots: [cameraFleetConfiguration(), imported],
		configurationImportPreview: importPreview
	});
	await page.goto('/cameras');
	await page.getByRole('button', { name: 'Configuration', exact: true }).click();
	const dialog = page.getByRole('dialog', { name: 'Camera fleet configuration' });
	await dialog.getByRole('tab', { name: 'Templates' }).click();
	await dialog.locator('input[type="file"]').setInputFiles({
		name: 'templates.json',
		mimeType: 'application/json',
		buffer: Buffer.from('{"document_version":1,"templates":[]}')
	});

	await expect(dialog.getByText('Import preview · 1 templates')).toBeVisible();
	await dialog.getByRole('button', { name: 'Apply import' }).click();
	await expect(dialog.getByRole('heading', { name: 'Outdoor cameras' })).toBeVisible();
	expect(controls.configurationActions).toEqual(['get', 'previewImport', 'applyImport']);
});

test('keeps the configuration dialog inside 390px and closes it with Escape', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await mockCameraFleet(page, 3, {
		capabilityIds: ['keeppeek.configuration.v1'],
		configurationSnapshots: [cameraFleetConfiguration()]
	});
	await page.goto('/cameras');
	await page.getByRole('button', { name: 'Configuration', exact: true }).click();
	const dialog = page.getByRole('dialog', { name: 'Camera fleet configuration' });
	await expect(dialog).toBeVisible();
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
	const bounds = await dialog.boundingBox();
	expect(bounds).not.toBeNull();
	expect(bounds!.x).toBeGreaterThanOrEqual(0);
	expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(390);
	await dialog.getByRole('tab', { name: 'Overview' }).focus();
	await page.keyboard.press('ArrowRight');
	await expect(dialog.getByRole('tab', { name: 'Defaults' })).toHaveAttribute(
		'aria-selected',
		'true'
	);
	await expect(dialog.getByRole('tab', { name: 'Defaults' })).toBeFocused();
	await page.keyboard.press('Escape');
	await expect(dialog).toHaveCount(0);
});

test('confirms navigation away from an unsaved configuration draft', async ({ page }) => {
	await mockCameraFleet(page, 3, {
		capabilityIds: ['keeppeek.configuration.v1'],
		configurationSnapshots: [cameraFleetConfiguration()],
		runtimeConfiguration
	});
	await page.goto('/cameras');
	await page.getByRole('button', { name: 'Configuration', exact: true }).click();
	const dialog = page.getByRole('dialog', { name: 'Camera fleet configuration' });
	await dialog.getByRole('tab', { name: 'Defaults' }).click();
	await dialog.locator('#policy-backend-operation').selectOption('set');
	await dialog.locator('#policy-backend-value').selectOption('retina');
	await expect(dialog.getByText('UNSAVED DRAFT', { exact: true })).toBeVisible();
	await dialog.getByRole('tab', { name: 'Overview' }).click();

	let dialogPromise = page.waitForEvent('dialog');
	let navigationPromise = dialog.getByRole('link', { name: 'Storage and retention' }).click();
	let confirmation = await dialogPromise;
	expect(confirmation.message()).toBe('Discard your unsaved configuration changes?');
	await confirmation.dismiss();
	await navigationPromise;
	await expect(page).toHaveURL(/\/cameras$/);
	await expect(dialog).toBeVisible();

	dialogPromise = page.waitForEvent('dialog');
	navigationPromise = dialog.getByRole('link', { name: 'Storage and retention' }).click();
	confirmation = await dialogPromise;
	expect(confirmation.message()).toBe('Discard your unsaved configuration changes?');
	await confirmation.accept();
	await navigationPromise;
	await expect(page).toHaveURL(/\/settings#storage$/);
});

test('preserves an open bulk draft through capability loss and recovery', async ({ page }) => {
	const controls = await mockCameraFleet(page, 3, {
		capabilityIds: ['keeppeek.configuration.v1'],
		configurationSnapshots: [cameraFleetConfiguration()]
	});
	await page.goto('/cameras');
	await page.getByRole('checkbox', { name: 'Select Porch' }).check();
	await page.getByRole('button', { name: 'Manage selected' }).click();
	const dialog = page.getByRole('dialog', { name: 'Camera fleet configuration' });
	await dialog.locator('#policy-backend-operation').selectOption('set');
	await dialog.locator('#policy-backend-value').selectOption('retina');

	await controls.publishCapabilities([]);
	await expect(dialog).toContainText('Server update required · keeppeek.configuration.v1');
	await expect(dialog.getByRole('button', { name: 'Preview exact changes' })).toBeDisabled();
	await expect(dialog.locator('#policy-backend-operation')).toHaveValue('set');
	await expect(dialog.locator('#policy-backend-value')).toHaveValue('retina');

	await controls.publishCapabilities(['keeppeek.configuration.v1']);
	await expect(dialog.getByRole('button', { name: 'Preview exact changes' })).toBeEnabled();
	await expect(dialog.locator('#policy-backend-operation')).toHaveValue('set');
	await expect(dialog.locator('#policy-backend-value')).toHaveValue('retina');
});
