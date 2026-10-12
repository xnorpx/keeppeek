import { page, userEvent } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { ControlClient } from '$lib/control-client';
import type { RuntimeProfileRegistry } from '$lib/runtime-profiles';
import type { CameraSettings } from '$lib/types';
import RuntimeProfilesSectionFixture from './RuntimeProfilesSection.fixture.svelte';

describe('RuntimeProfilesSection', () => {
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
