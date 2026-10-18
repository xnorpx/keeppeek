import { page, userEvent } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { ControlClient } from '$lib/control-client';
import type { RuntimeProfileRegistry, RuntimeProfileStatus } from '$lib/runtime-profiles';
import type { CameraSettings } from '$lib/types';
import RuntimeProfilesSectionFixture from './RuntimeProfilesSection.fixture.svelte';

function recoveryClient(profileId: string | null, state: 'degraded' | 'pending' = 'degraded') {
	const registry: RuntimeProfileRegistry = {
		revision: '3',
		active_profile_id: profileId,
		profiles: { away: { id: 'away', name: 'Away', revision: '2', cameras: {} } }
	};
	const status: RuntimeProfileStatus = {
		revision: '3',
		active_profile_id: profileId,
		cameras: {
			'192.0.2.10': {
				camera_id: '192.0.2.10',
				state,
				reason: state === 'degraded' ? 'recording writer reported an active write failure' : null,
				policy: {
					available: true,
					recording_mode: 'sub',
					privacy_enforced: false,
					notifications_enabled: true,
					available_source: 'base',
					recording_mode_source: 'base',
					privacy_source: 'base',
					notifications_source: 'base'
				}
			}
		}
	};
	const runtimeProfiles = {
		getActive: vi.fn().mockResolvedValue({ profileId, revision: '3' }),
		getStatus: vi.fn().mockResolvedValue(status),
		getRegistry: vi.fn().mockResolvedValue(registry),
		getCameras: vi.fn().mockResolvedValue({}),
		watchActive: vi.fn().mockResolvedValue({ close: vi.fn() }),
		watchStatus: vi.fn().mockResolvedValue({ close: vi.fn() }),
		watchRegistry: vi.fn().mockResolvedValue({ close: vi.fn() }),
		saveRegistry: vi.fn(),
		activate: vi.fn().mockResolvedValue('4')
	};
	return {
		client: { runtimeProfiles } as unknown as ControlClient,
		runtimeProfiles,
		status,
		registry
	};
}

describe('RuntimeProfilesSection', () => {
	it('keeps newer watched active and registry state when initial reads complete late', async () => {
		const { client, runtimeProfiles, registry, status } = recoveryClient('away');
		const delayed = Promise.withResolvers<{ profileId: string | null; revision: string }>();
		runtimeProfiles.getActive.mockReturnValue(delayed.promise);
		runtimeProfiles.watchActive.mockImplementation(
			async (listener: (next: { profileId: string | null; revision: string }) => void) => {
				listener({ profileId: null, revision: '5' });
				return { close: vi.fn() };
			}
		);
		runtimeProfiles.watchRegistry.mockImplementation(
			async (listener: (next: RuntimeProfileRegistry) => void) => {
				listener({
					...registry,
					revision: '5',
					active_profile_id: null,
					profiles: { away: { ...registry.profiles.away!, name: 'Generation five' } }
				});
				return { close: vi.fn() };
			}
		);
		runtimeProfiles.watchStatus.mockImplementation(
			async (listener: (next: RuntimeProfileStatus) => void) => {
				listener({ ...status, revision: '5', active_profile_id: null });
				return { close: vi.fn() };
			}
		);
		await render(RuntimeProfilesSectionFixture, { props: { client, cameras: [] } });
		delayed.resolve({ profileId: 'away', revision: '3' });
		await expect.element(page.getByLabelText('Display name')).toHaveValue('Generation five');
		await expect
			.element(page.getByText(/Active policy:/))
			.toHaveTextContent('Active policy: Base · revision 5');
		await expect.element(page.getByText('Camera application status for revision 5')).toBeVisible();
		expect(runtimeProfiles.getActive).toHaveBeenCalledOnce();
	});

	it('does not replace a same-revision failure observed while a status read is pending', async () => {
		const { client, runtimeProfiles, status } = recoveryClient('away');
		const delayed = Promise.withResolvers<RuntimeProfileStatus>();
		const oldStatus = structuredClone(status);
		oldStatus.cameras['192.0.2.10']!.state = 'applied';
		oldStatus.cameras['192.0.2.10']!.reason = null;
		let publishStatus: (next: RuntimeProfileStatus) => void = () => {};
		runtimeProfiles.getStatus.mockReturnValue(delayed.promise);
		runtimeProfiles.watchStatus.mockImplementation(async (listener: typeof publishStatus) => {
			publishStatus = listener;
			return { close: vi.fn() };
		});
		await render(RuntimeProfilesSectionFixture, { props: { client, cameras: [] } });
		await vi.waitFor(() => expect(runtimeProfiles.getStatus).toHaveBeenCalledOnce());
		publishStatus(status);
		delayed.resolve(oldStatus);
		await expect.element(page.getByLabelText('degraded policy application')).toBeVisible();
		await expect
			.element(page.getByText('recording writer reported an active write failure'))
			.toBeVisible();
	});
	it('ignores a registry watch read that completes after a newer save', async () => {
		const { client, runtimeProfiles, registry } = recoveryClient('away');
		const delayed = Promise.withResolvers<RuntimeProfileRegistry>();
		let watchRead: Promise<void> | undefined;
		runtimeProfiles.watchRegistry.mockImplementation(
			async (listener: (next: RuntimeProfileRegistry) => void) => {
				watchRead = delayed.promise.then(listener);
				return { close: vi.fn() };
			}
		);
		runtimeProfiles.saveRegistry.mockImplementation(async (candidate: RuntimeProfileRegistry) => ({
			...candidate,
			revision: '4'
		}));
		await render(RuntimeProfilesSectionFixture, { props: { client, cameras: [] } });
		await page.getByLabelText('Display name').fill('Saved generation four');
		await page.getByRole('button', { name: 'Save profile changes' }).click();
		await expect
			.element(page.getByRole('status'))
			.toHaveTextContent('Runtime profile changes saved.');
		delayed.resolve(registry);
		await watchRead;
		await expect.element(page.getByLabelText('Display name')).toHaveValue('Saved generation four');
		await page.getByLabelText('Display name').fill('Next edit');
		await page.getByRole('button', { name: 'Save profile changes' }).click();
		expect(runtimeProfiles.saveRegistry).toHaveBeenLastCalledWith(
			expect.objectContaining({ revision: '4' })
		);
	});

	it('invalidates a failed registry watch while preserving edits until explicit reload', async () => {
		const { client, runtimeProfiles } = recoveryClient('away');
		let failRead: (cause: unknown) => void = () => {};
		runtimeProfiles.watchRegistry.mockImplementation(
			async (_listener: unknown, onError: (cause: unknown) => void) => {
				failRead = onError;
				return { close: vi.fn() };
			}
		);
		await render(RuntimeProfilesSectionFixture, { props: { client, cameras: [] } });
		await page.getByLabelText('Display name').fill('My unsaved label');
		failRead?.(new Error('Registry read failed.'));
		await expect.element(page.getByRole('alert')).toHaveTextContent('Registry read failed.');
		await expect.element(page.getByLabelText('Display name')).toHaveValue('My unsaved label');
		await expect.element(page.getByLabelText('Display name')).toBeDisabled();
		await page.getByRole('button', { name: 'Discard changes and reload' }).click();
		await expect.element(page.getByLabelText('Display name')).toBeEnabled();
		await expect.element(page.getByLabelText('Display name')).toHaveValue('Away');
		expect(runtimeProfiles.saveRegistry).not.toHaveBeenCalled();
	});
	it('installs the newest deferred watch after a successful pending save', async () => {
		const { client, runtimeProfiles, registry } = recoveryClient('away');
		const pending = Promise.withResolvers<RuntimeProfileRegistry>();
		let publishRegistry: (next: RuntimeProfileRegistry) => void = () => {};
		runtimeProfiles.watchRegistry.mockImplementation(async (listener: typeof publishRegistry) => {
			publishRegistry = listener;
			return { close: vi.fn() };
		});
		runtimeProfiles.saveRegistry.mockReturnValueOnce(pending.promise);
		await render(RuntimeProfilesSectionFixture, { props: { client, cameras: [] } });
		await page.getByLabelText('Display name').fill('Saved generation four');
		await page.getByRole('button', { name: 'Save profile changes' }).click();
		await vi.waitFor(() => expect(runtimeProfiles.saveRegistry).toHaveBeenCalledOnce());
		publishRegistry({
			...registry,
			revision: '5',
			profiles: { away: { ...registry.profiles.away!, name: 'Another editor generation five' } }
		});
		publishRegistry(registry);
		pending.resolve({
			...registry,
			revision: '4',
			profiles: { away: { ...registry.profiles.away!, name: 'Saved generation four' } }
		});
		await expect
			.element(page.getByLabelText('Display name'))
			.toHaveValue('Another editor generation five');
		runtimeProfiles.saveRegistry.mockResolvedValue({ ...registry, revision: '6' });
		await page.getByLabelText('Display name').fill('Next edit');
		await page.getByRole('button', { name: 'Save profile changes' }).click();
		expect(runtimeProfiles.saveRegistry).toHaveBeenLastCalledWith(
			expect.objectContaining({ revision: '5' })
		);
	});

	it('conflicts with another editor without losing the draft and reloads only after explicit discard', async () => {
		const { client, runtimeProfiles, registry } = recoveryClient('away');
		let publishRegistry: (next: RuntimeProfileRegistry) => void = () => {};
		runtimeProfiles.watchRegistry.mockImplementation(
			async (listener: (next: RuntimeProfileRegistry) => void) => {
				publishRegistry = listener;
				return { close: vi.fn() };
			}
		);
		const current = {
			...registry,
			revision: '4',
			profiles: { away: { ...registry.profiles.away!, name: 'Another editor' } }
		};
		runtimeProfiles.saveRegistry.mockImplementation(async (candidate: RuntimeProfileRegistry) => {
			if (candidate.revision !== current.revision) throw new Error('Profile revision conflict.');
			return current;
		});
		await render(RuntimeProfilesSectionFixture, { props: { client, cameras: [] } });
		await expect.element(page.getByLabelText('Display name')).toHaveValue('Away');
		await page.getByLabelText('Display name').fill('My unsaved label');
		publishRegistry(current);
		await page.getByRole('button', { name: 'Save profile changes' }).click();
		await expect.element(page.getByRole('alert')).toHaveTextContent('Profile revision conflict.');
		await expect.element(page.getByLabelText('Display name')).toHaveValue('My unsaved label');
		expect(runtimeProfiles.saveRegistry).toHaveBeenCalledWith(
			expect.objectContaining({ revision: '3' })
		);
		runtimeProfiles.getRegistry.mockResolvedValue(current);
		await page.getByRole('button', { name: 'Discard changes and reload' }).click();
		await expect.element(page.getByLabelText('Display name')).toHaveValue('Another editor');
		await expect
			.element(page.getByText('Unsaved changes', { exact: true }))
			.not.toBeInTheDocument();
	});
	it('refreshes source bindings after a camera-list update without discarding profile edits', async () => {
		const { client, runtimeProfiles, registry } = recoveryClient('away');
		const stable = '00000000-0000-4000-8000-000000000001';
		registry.profiles.away!.cameras = { [stable]: { recording_mode: 'main' } };
		runtimeProfiles.getCameras.mockResolvedValue({ [stable]: '192.0.2.10' });
		const camera = { id: '192.0.2.10', display_name: 'Front door' } as CameraSettings;
		const view = await render(RuntimeProfilesSectionFixture, {
			props: { client, cameras: [camera] }
		});
		await expect
			.element(page.getByRole('combobox', { name: 'Recording', exact: true }))
			.toHaveValue('main');
		await page.getByLabelText('Display name').fill('Unsaved label');
		runtimeProfiles.getCameras.mockResolvedValue({ [stable]: '192.0.2.20' });
		await view.rerender({ client, cameras: [{ ...camera, id: '192.0.2.20' }] });
		await expect
			.element(page.getByRole('combobox', { name: 'Recording', exact: true }))
			.toBeEnabled();
		await expect
			.element(page.getByRole('combobox', { name: 'Recording', exact: true }))
			.toHaveValue('main');
		await expect.element(page.getByLabelText('Display name')).toHaveValue('Unsaved label');
		expect(runtimeProfiles.getCameras).toHaveBeenCalledTimes(2);
	});
	it('keeps a profile override bound to its identity after the camera IP changes', async () => {
		const { client, runtimeProfiles, registry } = recoveryClient('away');
		const stable = '00000000-0000-4000-8000-000000000001';
		registry.profiles.away!.cameras = { [stable]: { recording_mode: 'main' } };
		runtimeProfiles.getCameras.mockResolvedValue({ [stable]: '192.0.2.20' });
		runtimeProfiles.saveRegistry.mockResolvedValue(registry);
		const cameras = [{ id: '192.0.2.20', display_name: 'Front door' } as CameraSettings];
		await render(RuntimeProfilesSectionFixture, { props: { client, cameras } });
		await expect
			.element(page.getByRole('combobox', { name: 'Recording', exact: true }))
			.toHaveValue('main');
		await page.getByRole('combobox', { name: 'Recording', exact: true }).selectOptions('sub');
		await page.getByRole('button', { name: 'Save profile changes' }).click();
		expect(runtimeProfiles.saveRegistry).toHaveBeenCalledWith(
			expect.objectContaining({
				profiles: {
					away: expect.objectContaining({ cameras: { [stable]: { recording_mode: 'sub' } } })
				}
			})
		);
	});

	it('does not assign a removed target to a replacement camera reusing its IP', async () => {
		const { client, runtimeProfiles, registry } = recoveryClient('away');
		const removed = '00000000-0000-4000-8000-000000000001';
		const replacement = '00000000-0000-4000-8000-000000000002';
		registry.profiles.away!.cameras = { [removed]: { privacy_enforced: true } };
		runtimeProfiles.getCameras.mockResolvedValue({ [replacement]: '192.0.2.10' });
		const cameras = [{ id: '192.0.2.10', display_name: 'Replacement camera' } as CameraSettings];
		await render(RuntimeProfilesSectionFixture, { props: { client, cameras } });
		await expect
			.element(page.getByRole('region', { name: 'Unavailable profile targets' }).getByText(removed))
			.toBeVisible();
		await expect
			.element(page.getByRole('combobox', { name: 'Privacy', exact: true }))
			.toHaveValue('');
		await page.getByRole('combobox', { name: 'Privacy', exact: true }).selectOptions('true');
		runtimeProfiles.saveRegistry.mockResolvedValue(registry);
		await page.getByRole('button', { name: 'Save profile changes' }).click();
		expect(runtimeProfiles.saveRegistry).toHaveBeenCalledWith(
			expect.objectContaining({
				profiles: {
					away: expect.objectContaining({
						cameras: {
							[removed]: { privacy_enforced: true },
							[replacement]: { privacy_enforced: true }
						}
					})
				}
			})
		);
	});

	it('keeps edits and activation disabled after a camera catalog read fails until reload', async () => {
		const { client, runtimeProfiles } = recoveryClient('away');
		const cameras = [{ id: '192.0.2.10', display_name: 'Front door' } as CameraSettings];
		const bindings = { '00000000-0000-4000-8000-000000000001': '192.0.2.10' };
		runtimeProfiles.getCameras.mockResolvedValue(bindings);
		const view = await render(RuntimeProfilesSectionFixture, { props: { client, cameras } });
		await expect.element(page.getByLabelText('Display name')).toBeEnabled();
		runtimeProfiles.getCameras.mockRejectedValueOnce(new Error('Camera catalog read failed.'));
		await view.rerender({ client, cameras: [...cameras] });
		await expect.element(page.getByRole('alert')).toHaveTextContent('Camera catalog read failed.');
		await expect.element(page.getByLabelText('Display name')).toBeDisabled();
		await expect
			.element(page.getByRole('button', { name: 'Reapply active policy' }))
			.toBeDisabled();
		await page.getByRole('button', { name: 'Reload profiles' }).click();
		await expect.element(page.getByLabelText('Display name')).toBeEnabled();
		await expect.element(page.getByRole('button', { name: 'Reapply active policy' })).toBeEnabled();
		expect(runtimeProfiles.getCameras).toHaveBeenCalledTimes(3);
	});
	it('keeps unrelated profile actions available when one camera identity is ambiguous', async () => {
		const { client, runtimeProfiles, registry } = recoveryClient('away');
		const stable = '00000000-0000-4000-8000-000000000001';
		const missing = '00000000-0000-4000-8000-000000000002';
		registry.profiles.away!.cameras = { [missing]: { privacy_enforced: true } };
		runtimeProfiles.getCameras.mockResolvedValue({ [stable]: '192.0.2.10' });
		runtimeProfiles.saveRegistry.mockImplementation(async (next: RuntimeProfileRegistry) => ({
			...next,
			revision: '4'
		}));
		const cameras = [
			{ id: '192.0.2.10', display_name: 'Front door' },
			{ id: '192.0.2.20', display_name: 'Ambiguous source' }
		] as CameraSettings[];
		await render(RuntimeProfilesSectionFixture, { props: { client, cameras } });
		await expect.element(page.getByLabelText('Display name')).toBeEnabled();
		await expect.element(page.getByRole('button', { name: 'Select Base' })).toBeEnabled();
		await expect.element(page.getByRole('button', { name: 'Reapply active policy' })).toBeEnabled();
		const recording = page.getByRole('combobox', { name: 'Recording', exact: true });
		await expect.element(recording.nth(0)).toBeEnabled();
		await expect.element(recording.nth(1)).toBeDisabled();
		await expect
			.element(page.getByText('Camera identity is unavailable or ambiguous.'))
			.toBeVisible();
		await recording.nth(0).selectOptions('sub');
		await page.getByRole('button', { name: 'Save profile changes' }).click();
		expect(runtimeProfiles.saveRegistry).toHaveBeenCalledWith(
			expect.objectContaining({
				profiles: {
					away: expect.objectContaining({
						cameras: {
							[stable]: { recording_mode: 'sub' },
							[missing]: { privacy_enforced: true }
						}
					})
				}
			})
		);
		await expect
			.element(page.getByRole('button', { name: `Remove unavailable target ${missing}` }))
			.toBeEnabled();
	});
	it.each([
		[null, 'degraded'],
		[null, 'pending'],
		['away', 'degraded'],
		['away', 'pending']
	] as const)(
		'reapplies active %s after %s without selecting another policy',
		async (id, state) => {
			const { client, runtimeProfiles } = recoveryClient(id, state);
			await render(RuntimeProfilesSectionFixture, { props: { client, cameras: [] } });
			const retry = page.getByRole('button', { name: 'Reapply active policy' });
			await expect.element(retry).toBeEnabled();
			await retry.click();
			expect(runtimeProfiles.activate).toHaveBeenCalledExactlyOnceWith(id, '3');
			await expect.element(page.getByRole('status')).toHaveTextContent('Active policy reapplied.');
		}
	);

	it('keeps recovery available after a failed request refreshes the accepted revision', async () => {
		const { client, runtimeProfiles, status } = recoveryClient('away');
		runtimeProfiles.activate.mockRejectedValueOnce(new Error('Activation conflicted.'));
		await render(RuntimeProfilesSectionFixture, { props: { client, cameras: [] } });
		const retry = page.getByRole('button', { name: 'Reapply active policy' });
		await expect.element(retry).toBeEnabled();
		runtimeProfiles.getActive.mockResolvedValue({ profileId: 'away', revision: '5' });
		runtimeProfiles.getStatus.mockResolvedValue({ ...status, revision: '5' });
		await retry.click();
		await expect.element(page.getByRole('alert')).toHaveTextContent('Activation conflicted.');
		await expect.element(retry).toBeEnabled();
		await retry.click();
		expect(runtimeProfiles.activate).toHaveBeenCalledTimes(2);
		expect(runtimeProfiles.activate).toHaveBeenNthCalledWith(1, 'away', '3');
		expect(runtimeProfiles.activate).toHaveBeenNthCalledWith(2, 'away', '5');
	});

	it('does not reapply with unsaved edits', async () => {
		const { client, runtimeProfiles } = recoveryClient('away');
		await render(RuntimeProfilesSectionFixture, { props: { client, cameras: [] } });
		const retry = page.getByRole('button', { name: 'Reapply active policy' });
		await expect.element(retry).toBeEnabled();
		await page.getByLabelText('Display name').fill('Unsaved name');
		await expect.element(retry).toBeDisabled();
		expect(runtimeProfiles.activate).not.toHaveBeenCalled();
	});

	it('blocks recovery if the registry schema cannot be read', async () => {
		const { client, runtimeProfiles } = recoveryClient('away');
		runtimeProfiles.getRegistry.mockRejectedValue(new Error('Unsupported runtime profile schema.'));
		await render(RuntimeProfilesSectionFixture, { props: { client, cameras: [] } });
		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent('Unsupported runtime profile schema.');
		await expect
			.element(page.getByRole('button', { name: 'Reapply active policy' }))
			.toBeDisabled();
		expect(runtimeProfiles.activate).not.toHaveBeenCalled();
	});

	it('does not offer recovery for Users', async () => {
		const { client, runtimeProfiles } = recoveryClient('away');
		await render(RuntimeProfilesSectionFixture, {
			props: { client, cameras: [], administrator: false }
		});
		await expect
			.element(page.getByRole('region', { name: 'Runtime profile convergence' }))
			.toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Reapply active policy' }))
			.not.toBeInTheDocument();
		expect(runtimeProfiles.getRegistry).not.toHaveBeenCalled();
		expect(runtimeProfiles.activate).not.toHaveBeenCalled();
	});

	it.each(['revision', 'identity', 'applied'])(
		'does not retry an unrelated or healthy %s observation',
		async (scenario) => {
			const { client, runtimeProfiles, status } = recoveryClient('away');
			if (scenario === 'revision') status.revision = '2';
			else if (scenario === 'identity') status.active_profile_id = null;
			else status.cameras['192.0.2.10']!.state = 'applied';
			await render(RuntimeProfilesSectionFixture, { props: { client, cameras: [] } });
			await expect
				.element(page.getByRole('region', { name: 'Runtime profile convergence' }))
				.toBeVisible();
			await expect
				.element(page.getByRole('button', { name: 'Reapply active policy' }))
				.not.toBeInTheDocument();
			expect(runtimeProfiles.activate).not.toHaveBeenCalled();
		}
	);

	it('shows a degraded runtime reason for a removed active target', async () => {
		const registry: RuntimeProfileRegistry = {
			revision: '3',
			active_profile_id: 'away',
			profiles: {
				away: {
					id: 'away',
					name: 'Away',
					revision: '2',
					cameras: { '192.0.2.10': { availability: false } }
				}
			}
		};
		const client = {
			runtimeProfiles: {
				getActive: vi.fn().mockResolvedValue({ profileId: 'away', revision: '3' }),
				getStatus: vi.fn().mockResolvedValue({
					revision: '3',
					active_profile_id: 'away',
					cameras: {
						'192.0.2.10': {
							camera_id: '192.0.2.10',
							state: 'degraded',
							reason: 'configured camera is missing',
							policy: {
								available: false,
								recording_mode: 'event-boost',
								privacy_enforced: false,
								notifications_enabled: true,
								available_source: 'profile',
								recording_mode_source: 'base',
								privacy_source: 'base',
								notifications_source: 'base'
							}
						}
					}
				}),
				getRegistry: vi.fn().mockResolvedValue(registry),
				getCameras: vi.fn().mockResolvedValue({}),
				watchActive: vi.fn().mockResolvedValue({ close: vi.fn() }),
				watchStatus: vi.fn().mockResolvedValue({ close: vi.fn() }),
				watchRegistry: vi.fn().mockResolvedValue({ close: vi.fn() }),
				saveRegistry: vi.fn(),
				activate: vi.fn()
			}
		} as unknown as ControlClient;

		await render(RuntimeProfilesSectionFixture, {
			props: { client, cameras: [] as CameraSettings[] }
		});

		const convergence = page.getByRole('region', { name: 'Runtime profile convergence' });
		await expect.element(convergence).toBeVisible();
		await expect.element(convergence.getByText('192.0.2.10', { exact: true })).toBeVisible();
		await expect.element(page.getByLabelText('degraded policy application')).toBeVisible();
		await expect.element(convergence.getByText('configured camera is missing')).toBeVisible();
	});

	it('shows and removes a profile target that is no longer configured', async () => {
		const registry: RuntimeProfileRegistry = {
			revision: '1',
			active_profile_id: null,
			profiles: {
				away: {
					id: 'away',
					name: 'Away',
					revision: '1',
					cameras: { '192.0.2.10': { availability: false } }
				}
			}
		};
		const saveRegistry = vi.fn().mockImplementation(async (candidate: RuntimeProfileRegistry) => ({
			...candidate,
			revision: '2'
		}));
		const client = {
			runtimeProfiles: {
				getActive: vi.fn().mockResolvedValue({ profileId: null, revision: '0' }),
				getStatus: vi.fn().mockResolvedValue({
					revision: '0',
					active_profile_id: null,
					cameras: {}
				}),
				getRegistry: vi.fn().mockResolvedValue(registry),
				getCameras: vi
					.fn()
					.mockResolvedValue({ '00000000-0000-4000-8000-000000000011': '192.0.2.11' }),
				watchActive: vi.fn().mockResolvedValue({ close: vi.fn() }),
				watchStatus: vi.fn().mockResolvedValue({ close: vi.fn() }),
				watchRegistry: vi.fn().mockResolvedValue({ close: vi.fn() }),
				saveRegistry,
				activate: vi.fn()
			}
		} as unknown as ControlClient;
		const cameras = [
			{
				id: '192.0.2.11',
				ip: '192.0.2.11',
				display_name: 'Front door'
			} as CameraSettings
		];

		await render(RuntimeProfilesSectionFixture, { props: { client, cameras } });
		await expect.element(page.getByRole('heading', { name: 'Runtime profiles' })).toBeVisible();
		await expect
			.element(page.getByRole('region', { name: 'Unavailable profile targets' }))
			.toBeVisible();
		await expect.element(page.getByText('192.0.2.10', { exact: true })).toBeVisible();

		await userEvent.click(
			page.getByRole('button', { name: 'Remove unavailable target 192.0.2.10' })
		);
		await userEvent.click(page.getByRole('button', { name: 'Save profile changes' }));

		expect(saveRegistry).toHaveBeenCalledWith(
			expect.objectContaining({
				profiles: {
					away: expect.objectContaining({ cameras: {} })
				}
			})
		);
		await expect
			.element(page.getByRole('region', { name: 'Unavailable profile targets' }))
			.not.toBeInTheDocument();
	});
});
