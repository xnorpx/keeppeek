import { create } from '@bufbuild/protobuf';
import { page, userEvent } from 'vitest/browser';
import { expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../../app.css';
import { archiveDraft, encodeArchiveDraft, type ArchiveController } from '$lib/archive';
import {
	ArchiveResultSchema,
	ArchivePrimaryCopyState,
	StorageObjectKind,
	StorageVolumeResultSchema,
	type ArchiveJob
} from '$lib/proto/webrtc_pb';
import type { SanitizedConfig } from '$lib/types';
import ArchiveSection from './ArchiveSection.svelte';

vi.mock('$app/navigation', () => ({ beforeNavigate: vi.fn() }));
function fixture(): SanitizedConfig {
	const draft = archiveDraft();
	draft.root = '{secret:ROOT}';
	return {
		host: 'localhost',
		port: 3000,
		configuration_revision: 'revision-one',
		camera_count: 0,
		storage: {
			archive: encodeArchiveDraft(draft),
			medium_term_path: '/medium',
			long_term_path: '/long',
			recording_catalog_path: '/catalog',
			event_thumbnail_path: '/images',
			event_thumbnail_max_mb: 10,
			short_term_secs: 90,
			medium_term_secs: 1200,
			flush_interval_secs: 60,
			write_buffer_bytes: 8192,
			long_term_max_gb: 100
		},
		recording_estimate: {
			estimated_bitrate_bps: 0,
			bytes_per_day: 0,
			known_streams: 0,
			unknown_streams: 0,
			estimated_retention_days: null
		}
	};
}
function controller() {
	return {
		archive: vi.fn<ArchiveController['archive']>().mockResolvedValue(
			create(ArchiveResultSchema, {
				result: { case: 'jobs', value: { status: { runtimeAvailable: true, online: true } } }
			})
		),
		storageVolumes: vi.fn<ArchiveController['storageVolumes']>(),
		getRuntimeConfiguration: vi.fn<ArchiveController['getRuntimeConfiguration']>(),
		updateRuntimeConfiguration: vi.fn<ArchiveController['updateRuntimeConfiguration']>()
	};
}

function expectMobileTargets(container: Element) {
	for (const target of container.querySelectorAll(
		'button,input:not([type="checkbox"]),select,summary,label:has(input[type="checkbox"])'
	)) {
		if (target.checkVisibility())
			expect(
				target.getBoundingClientRect().height,
				target.textContent ?? target.tagName
			).toBeGreaterThanOrEqual(44);
	}
}

function configureMediaCatalog(
	control: ReturnType<typeof controller>,
	job: Pick<ArchiveJob, 'objectId' | 'kind' | 'bytes'>
) {
	control.storageVolumes.mockImplementation(async (request) =>
		create(StorageVolumeResultSchema, {
			result:
				request.action.case === 'list'
					? {
							case: 'volumes',
							value: { runtimeAvailable: true, volumes: [{ volumeId: 'legacy-long-term' }] }
						}
					: {
							case: 'objects',
							value: {
								objects: [{ object: { id: job.objectId, kind: job.kind }, bytes: job.bytes }]
							}
						}
		})
	);
}

async function selectMediaObject(job: Pick<ArchiveJob, 'objectId' | 'kind'>) {
	await page.getByRole('button', { name: 'Load archive source volumes' }).click();
	await page.getByLabelText('Source volume', { exact: true }).selectOptions('legacy-long-term');
	await page.getByRole('button', { name: 'Load eligible media' }).click();
	await page
		.getByLabelText('Media object', { exact: true })
		.selectOptions(`${job.kind}:${job.objectId}`);
}

async function inspectEviction(
	job: Pick<ArchiveJob, 'objectId' | 'targetId' | 'bytes'>,
	control: ReturnType<typeof controller>
) {
	const opener = page.getByRole('button', { name: `Evict primary ${job.objectId}` });
	await opener.click();
	const eviction = page.getByRole('dialog', { name: 'Evict primary copy?' });
	await expect.element(eviction).toBeVisible();
	await expect.element(eviction.getByRole('button', { name: 'Cancel', exact: true })).toHaveFocus();
	expect(eviction.element().textContent).toContain(`${job.bytes} bytes`);
	expect(eviction.element().textContent).toContain(job.objectId);
	expect(eviction.element().textContent).toContain(job.targetId);
	expect(eviction.element().getBoundingClientRect().width).toBeLessThanOrEqual(320);
	expect(eviction.element().scrollWidth).toBeLessThanOrEqual(eviction.element().clientWidth);
	await eviction.getByRole('button', { name: 'Cancel', exact: true }).click();
	await expect.element(opener).toHaveFocus();
	expect(
		control.archive.mock.calls.filter(([request]) => request.action.case === 'evict')
	).toHaveLength(0);
	await opener.click();
	await userEvent.keyboard('{Escape}');
	await expect.element(eviction).not.toBeInTheDocument();
	expect(
		control.archive.mock.calls.filter(([request]) => request.action.case === 'evict')
	).toHaveLength(0);
	await opener.click();
	return eviction;
}

it('preserves a conflicting dirty draft, references and original CAS revision during refresh', async () => {
	await page.viewport(390, 844);
	const config = fixture();
	const control = controller();
	control.updateRuntimeConfiguration.mockRejectedValue(
		new Error('Configuration revision conflict')
	);
	const { container } = await render(ArchiveSection, {
		config,
		controller: control,
		onsaved: vi.fn()
	});
	await page.getByRole('button', { name: 'Edit archive settings' }).click();
	await expect
		.element(page.getByLabelText('Maximum pending bytes', { exact: true }))
		.not.toBeVisible();
	await page.getByText('Advanced selection & transfer limits', { exact: true }).click();
	expectMobileTargets(container);
	await page.getByLabelText('Maximum pending bytes', { exact: true }).fill('9000000000000000000');
	await page.getByRole('button', { name: 'Refresh archive jobs' }).click();
	await page.getByRole('button', { name: 'Save archive settings', exact: true }).click();
	await expect.element(page.getByRole('alert')).toBeVisible();
	expect(page.getByRole('alert').element().textContent).toContain('Your draft is preserved');
	await expect
		.element(page.getByLabelText('Archive folder', { exact: true }))
		.toHaveValue('{secret:ROOT}');
	await expect
		.element(page.getByLabelText('Maximum pending bytes', { exact: true }))
		.toHaveValue('9000000000000000000');
	expect(control.updateRuntimeConfiguration.mock.calls[0]?.[0]).toMatchObject({
		expected_configuration_revision: 'revision-one',
		storage: {
			archive: {
				root: '{secret:ROOT}',
				maximumPendingBytes: 9_000_000_000_000_000_000n,
				evictAfterArchive: false
			}
		}
	});
	expect(container.scrollWidth).toBeLessThanOrEqual(390);
	const current = fixture();
	current.configuration_revision = 'revision-two';
	control.getRuntimeConfiguration.mockResolvedValue(current);
	control.updateRuntimeConfiguration.mockResolvedValue({
		config: current,
		restart_required: true
	});
	const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
	try {
		await page.getByRole('button', { name: 'Reload archive settings' }).click();
		expect(control.getRuntimeConfiguration).not.toHaveBeenCalled();
		confirm.mockReturnValue(true);
		await page.getByRole('button', { name: 'Reload archive settings' }).click();
		await expect
			.element(page.getByLabelText('Maximum pending bytes', { exact: true }))
			.toHaveValue('68719476736');
		await page.getByLabelText('Minimum age seconds', { exact: true }).fill('3600');
		await page.getByRole('button', { name: 'Save archive settings', exact: true }).click();
		expect(
			control.updateRuntimeConfiguration.mock.calls[1]?.[0].expected_configuration_revision
		).toBe('revision-two');
	} finally {
		confirm.mockRestore();
	}
});

it('previews each retention page independently and retries the same confirmation after a lost reply', async () => {
	const config = fixture();
	config.storage.archive!.retentionDays = 30;
	const control = controller();
	const response = Promise.withResolvers<void>();
	let confirmations = 0;
	control.archive.mockImplementation(async (request) => {
		if (request.action.case === 'list')
			return create(ArchiveResultSchema, {
				result: { case: 'jobs', value: { status: { runtimeAvailable: true, online: true } } }
			});
		const second =
			request.action.case === 'previewDeletion' &&
			request.action.value.selection.case === 'retentionPage' &&
			request.action.value.selection.value.afterJobId === 'next';
		if (request.action.case === 'confirmDeletion' && confirmations++ === 0) {
			await response.promise;
			throw new Error('Lost confirmation reply');
		}
		const confirmed = request.action.case === 'confirmDeletion';
		return create(ArchiveResultSchema, {
			result: {
				case: 'deletionPreview',
				value: {
					targetId: 'local-archive',
					planId: second ? 'p2' : 'p1',
					revision: 'rev',
					confirmationNonce: confirmed ? '' : 'nonce',
					nextAfterJobId: second ? '' : 'next',
					selectedCount: second ? 0 : 1,
					selectedBytes: second ? 0n : 10n,
					confirmed,
					items: [
						{
							jobId: second ? 'two' : 'one',
							objectId: second ? 'protected' : 'footage',
							kind: StorageObjectKind.RECORDING,
							bytes: 10n,
							state: second ? 'preservation_hold' : confirmed ? 'queued' : 'eligible'
						}
					]
				}
			}
		});
	});
	await page.viewport(320, 844);
	await render(ArchiveSection, {
		config,
		controller: control,
		retentionAvailable: true,
		onsaved: vi.fn()
	});
	await page.getByRole('button', { name: 'Preview expired archive copies' }).click();
	const dialog = page.getByRole('dialog', { name: 'Archive deletion preview' });
	await expect
		.element(dialog.getByText('Eligible for deletion · 10 bytes', { exact: true }))
		.toBeVisible();
	await expect.element(dialog.getByRole('button', { name: 'Close', exact: true })).toHaveFocus();
	await expect
		.element(
			dialog.getByText(
				'After removal, KeepPeek permanently excludes these objects from automatic archiving and the manual copy picker.',
				{ exact: true }
			)
		)
		.toBeVisible();
	expect(dialog.element().scrollWidth).toBeLessThanOrEqual(dialog.element().clientWidth);
	const confirm = dialog.getByRole('button', { name: 'Confirm archive deletion' });
	const close = dialog.getByRole('button', { name: 'Close', exact: true });
	await confirm.click();
	await expect.element(close).toBeDisabled();
	await userEvent.keyboard('{Escape}');
	await expect.element(dialog).toBeVisible();
	response.resolve();
	await expect.element(dialog.getByRole('alert')).toHaveTextContent('Lost confirmation reply');
	await expect.element(close).toHaveFocus();
	await confirm.click();
	await expect
		.element(dialog.getByRole('status'))
		.toHaveTextContent('Batch confirmed. Refresh archive jobs to follow progress.');
	await expect.element(close).toHaveFocus();
	const requests = control.archive.mock.calls.flatMap(([r]) =>
		r.action.case === 'confirmDeletion' ? [r.action.value] : []
	);
	expect(requests).toHaveLength(2);
	expect(requests[0]).toEqual(requests[1]);
	await dialog.getByRole('button', { name: 'Preview next page' }).click();
	await expect
		.element(dialog.getByText('Preservation hold or maintenance · 10 bytes', { exact: true }))
		.toBeVisible();
	await expect.element(confirm).not.toBeInTheDocument();
	await expect.element(close).toHaveFocus();
	await expect
		.element(dialog.getByRole('status'))
		.toHaveTextContent('Target local-archive · 0 selected · 0 bytes');
});
it('queues and reconciles a real selection, then confirms eviction and restores the primary', async () => {
	const control = controller();
	const job = {
		jobId: 'a0000000-0000-4000-8000-000000000001',
		objectId: 'recording-one',
		targetId: 'local-archive',
		kind: StorageObjectKind.RECORDING,
		state: 'ready',
		bytes: 42n,
		primaryCopy: ArchivePrimaryCopyState.PRESENT
	};
	configureMediaCatalog(control, job);
	control.archive.mockImplementation(async (request) => {
		if (request.action.case === 'list')
			return create(ArchiveResultSchema, {
				result: { case: 'jobs', value: { status: { runtimeAvailable: true, online: true } } }
			});
		if (request.action.case === 'copy') throw new Error('Lost acknowledgment');
		if (request.action.case === 'evict')
			return create(ArchiveResultSchema, {
				result: { case: 'job', value: { ...job, primaryCopy: ArchivePrimaryCopyState.EVICTED } }
			});
		return create(ArchiveResultSchema, { result: { case: 'job', value: job } });
	});
	await page.viewport(320, 844);
	await render(ArchiveSection, { config: fixture(), controller: control, onsaved: vi.fn() });
	await selectMediaObject(job);
	await page.getByRole('button', { name: 'Queue archive copy' }).click();
	await expect
		.element(page.getByRole('article', { name: `Archive job ${job.jobId}` }))
		.toBeVisible();
	const copy = control.archive.mock.calls.find(([request]) => request.action.case === 'copy')?.[0];
	const get = control.archive.mock.calls.find(([request]) => request.action.case === 'get')?.[0];
	expect(copy?.action.value).toMatchObject({ objectId: job.objectId });
	if (copy?.action.case !== 'copy') throw new Error('Expected archive copy');
	expect(get?.action.value).toMatchObject({
		jobId: copy.action.value.jobId
	});
	const eviction = await inspectEviction(job, control);
	await eviction.getByRole('button', { name: 'Evict primary', exact: true }).click();
	await expect.element(page.getByText('Primary evicted; archive retained.')).toBeVisible();
	await page.getByRole('button', { name: `Restore primary ${job.objectId}` }).click();
	await page
		.getByRole('dialog', { name: 'Restore primary copy?' })
		.getByRole('button', { name: 'Restore primary', exact: true })
		.click();
	expect(
		control.archive.mock.calls.filter(([request]) => request.action.case === 'restore')
	).toHaveLength(1);
});
it('reports probe pending and prevents cancellation during primary eviction reconciliation', async () => {
	const control = controller();
	control.archive.mockImplementation(async (request) =>
		request.action.case === 'probe'
			? create(ArchiveResultSchema, {
					result: { case: 'status', value: { runtimeAvailable: true, probePending: true } }
				})
			: create(ArchiveResultSchema, {
					result: {
						case: 'jobs',
						value: {
							status: { runtimeAvailable: true },
							jobs: [
								{
									jobId: 'evicting',
									objectId: 'recording',
									state: 'ready',
									action: 'evict',
									primaryCopy: ArchivePrimaryCopyState.EVICTING
								}
							]
						}
					}
				})
	);
	await render(ArchiveSection, { config: fixture(), controller: control, onsaved: vi.fn() });
	await page.getByRole('button', { name: 'Test archive destination' }).click();
	await expect.element(page.getByText('Archive destination probe pending.')).toBeVisible();
	await expect
		.element(page.getByRole('button', { name: 'Test archive destination' }))
		.toBeDisabled();
	await expect
		.element(page.getByRole('button', { name: 'Cancel archive job evicting' }))
		.not.toBeInTheDocument();
});
it('cancels a transfer and retries a failed restore using their existing durable IDs', async () => {
	const control = controller();
	const copying = { jobId: 'copying', objectId: 'clip', state: 'copying' };
	const restore = {
		jobId: 'restore',
		objectId: 'older-clip',
		state: 'ready',
		action: 'restore',
		errorCode: 'archive_operation_failed',
		primaryCopy: ArchivePrimaryCopyState.EVICTED
	};
	control.archive.mockImplementation(async (request) => {
		if (request.action.case === 'list')
			return create(ArchiveResultSchema, {
				result: {
					case: 'jobs',
					value: { jobs: [copying, restore], status: { runtimeAvailable: true } }
				}
			});
		return create(ArchiveResultSchema, {
			result: {
				case: 'job',
				value:
					request.action.case === 'cancel'
						? { ...copying, state: 'cancelled' }
						: request.action.case === 'retry' && request.action.value.jobId === 'copying'
							? { ...copying, state: 'queued' }
							: { ...restore, errorCode: '' }
			}
		});
	});
	await render(ArchiveSection, { config: fixture(), controller: control, onsaved: vi.fn() });
	await expect
		.element(page.getByRole('article', { name: 'Archive job restore' }).getByRole('alert'))
		.toHaveTextContent(
			'Restore failed. Check the primary disk and retry. The archive copy is retained.'
		);
	await page.getByRole('button', { name: 'Cancel archive job copying' }).click();
	await expect
		.element(page.getByRole('button', { name: 'Cancel archive job copying' }))
		.not.toBeInTheDocument();
	await page.getByRole('button', { name: 'Retry archive job copying' }).click();
	await expect
		.element(page.getByRole('button', { name: 'Cancel archive job copying' }))
		.toBeVisible();
	await page.getByRole('button', { name: 'Retry archive job restore' }).click();
	expect(
		control.archive.mock.calls
			.filter(([request]) => request.action.case === 'cancel' || request.action.case === 'retry')
			.map(([request]) => request.action)
	).toMatchObject([
		{ case: 'cancel', value: { jobId: 'copying' } },
		{ case: 'retry', value: { jobId: 'copying' } },
		{ case: 'retry', value: { jobId: 'restore' } }
	]);
});

it('retries verification of a legacy archive and requires a fresh preview for a blocked deletion', async () => {
	const control = controller();
	const legacy = {
		jobId: 'legacy',
		objectId: 'legacy-clip',
		state: 'ready',
		errorCode: 'archive_reverification_failed'
	};
	const blocked = {
		jobId: 'blocked',
		objectId: 'protected-clip',
		state: 'ready',
		errorCode: 'archive_primary_write_protection_required'
	};
	control.archive.mockImplementation(async (request) =>
		create(ArchiveResultSchema, {
			result:
				request.action.case === 'list'
					? { case: 'jobs', value: { jobs: [legacy, blocked], status: { runtimeAvailable: true } } }
					: { case: 'job', value: { ...legacy, errorCode: '' } }
		})
	);
	await render(ArchiveSection, {
		config: fixture(),
		controller: control,
		retentionAvailable: true,
		onsaved: vi.fn()
	});
	await expect
		.element(page.getByRole('button', { name: 'Retry archive job blocked' }))
		.not.toBeInTheDocument();
	await expect
		.element(page.getByRole('button', { name: 'Preview archive deletion protected-clip' }))
		.toBeVisible();
	await page.getByRole('button', { name: 'Retry archive job legacy' }).click();
	expect(control.archive.mock.calls.at(-1)?.[0].action).toMatchObject({
		case: 'retry',
		value: { jobId: 'legacy' }
	});
	await expect
		.element(page.getByRole('button', { name: 'Retry archive job legacy' }))
		.not.toBeInTheDocument();
	await page.getByRole('button', { name: 'Edit archive settings' }).click();
	await expect
		.element(page.getByRole('textbox', { name: 'Archive retention days', exact: true }))
		.toHaveAccessibleDescription(/Age starts when a replica is verified/);
});
