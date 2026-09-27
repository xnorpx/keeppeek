import { create } from '@bufbuild/protobuf';
import { mount, tick, unmount, type ComponentProps } from 'svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import {
	AccessRole,
	ConfigurationApplyResultSchema,
	ConfigurationPlanSchema,
	OidcAuthenticationSettingsSchema
} from '$lib/proto/webrtc_pb';
import type {
	AdministratorVerification,
	ExternalAuthenticationSettings
} from '$lib/proto/webrtc_pb';
import type { AccessConnectionState } from '$lib/access';
import ExternalAuthenticationPlan from './ExternalAuthenticationPlan.svelte';
import PeekDashboardAudiencePicker from './PeekDashboardAudiencePicker.svelte';
import '../../app.css';

const mounted: Array<ReturnType<typeof mount>> = [];
const targets: HTMLElement[] = [];
afterEach(async () => {
	for (const component of mounted.splice(0)) await unmount(component);
	for (const target of targets.splice(0)) target.remove();
	vi.restoreAllMocks();
	vi.useRealTimers();
});
function target() {
	const element = document.createElement('div');
	document.body.append(element);
	targets.push(element);
	return element;
}
function liveState(): AccessConnectionState {
	return {
		status: 'authenticated',
		generation: 1,
		message: null,
		session: {
			id: 'rtc-parent',
			principalId: 'admin',
			displayName: 'Admin',
			role: 'administrator',
			local: false,
			clientClassification: 'remote',
			createdAtMs: 0,
			lastActivityAtMs: 0,
			absoluteExpiresAtMs: Date.now() + 60_000,
			credentialExpiresAtMs: null
		}
	};
}
const proofId = '12345678-1234-1234-1234-123456789abc';
function browserSettings(origin: string): ExternalAuthenticationSettings {
	return {
		$typeName: 'keeppeek.webrtc.v1.ExternalAuthenticationSettings',
		allowedOrigins: [origin],
		bearerEnabled: false,
		providers: [
			{
				$typeName: 'keeppeek.webrtc.v1.ExternalAuthenticationProvider',
				providerId: 'company',
				name: 'Company',
				mappings: [],
				method: { case: undefined }
			}
		]
	};
}
function fixture(
	browser = false,
	origin = location.origin,
	candidate?: ExternalAuthenticationSettings
) {
	const plan = {
		...create(ConfigurationPlanSchema, {
			planId: 'plan',
			configurationRevision: 'revision',
			valid: true,
			expiresAtMs: BigInt(Date.now() + 60_000)
		}),
		requiresAdministratorConfirmation: true
	};
	const proof: AdministratorVerification = {
		$typeName: 'keeppeek.webrtc.v1.AdministratorVerification',
		verificationId: proofId,
		configurationPlanId: 'plan',
		expiresAtMs: plan.expiresAtMs,
		verified: true
	};
	const settings = candidate ?? (browser ? browserSettings(origin) : null);
	let notify: (state: AccessConnectionState) => void = () => {};
	const applied = vi
		.fn()
		.mockResolvedValue(create(ConfigurationApplyResultSchema, { configurationCommitted: true }));
	const controller: ComponentProps<typeof ExternalAuthenticationPlan>['controller'] = {
		onAccessState: (listener) => {
			notify = listener;
			listener(liveState());
			return () => {};
		},
		prepareAdministratorVerification: vi.fn().mockResolvedValue({
			...proof,
			verified: false,
			browserStart: {
				$typeName: 'keeppeek.webrtc.v1.AdministratorBrowserStart',
				origin,
				providerId: 'company',
				csrfToken: 'csrf'
			}
		}),
		getAdministratorVerification: vi.fn().mockResolvedValue(proof),
		verifyAdministratorBearer: vi.fn().mockResolvedValue(proof),
		applyExternalAuthentication: applied
	};
	const onapplied = vi.fn();
	const element = target();
	mounted.push(
		mount(ExternalAuthenticationPlan, {
			target: element,
			props: { controller, plan, settings, onapplied }
		})
	);
	return {
		element,
		plan,
		controller,
		applied,
		onapplied,
		disconnect: () => notify({ ...liveState(), status: 'sign-in-required', session: null })
	};
}

describe('administrator confirmation lifecycle', () => {
	it('selects the OIDC redirect origin even when the current origin is also allowed', async () => {
		const origin = 'https://replacement.example';
		const candidate = browserSettings(origin);
		candidate.allowedOrigins.unshift(location.origin);
		candidate.providers[0].method = {
			case: 'oidc',
			value: create(OidcAuthenticationSettingsSchema, { redirectUri: `${origin}/auth/callback` })
		};
		const view = fixture(true, origin, candidate);
		await page.getByLabelText('Verification provider').selectOptions('company');
		await page.getByRole('button', { name: 'Prepare verification' }).click();
		expect(view.controller.prepareAdministratorVerification).toHaveBeenCalledWith(
			view.plan.planId,
			'company',
			origin
		);
		await expect.element(page.getByText(`Verification origin: ${origin}`)).toBeVisible();
	});
	it('prepares a replacement on its candidate origin instead of the current hostname', async () => {
		const view = fixture(true, 'https://replacement.example');
		await page.getByRole('combobox').selectOptions('company');
		await page.getByRole('button', { name: 'Prepare verification' }).click();
		expect(view.controller.prepareAdministratorVerification).toHaveBeenCalledWith(
			view.plan.planId,
			'company',
			'https://replacement.example'
		);
	});
	it('requires fresh proof and explicit confirmation, then sends the bound proof over the controller', async () => {
		const view = fixture();
		await expect
			.element(page.getByRole('button', { name: 'Apply authentication changes' }))
			.toBeDisabled();
		await page.getByLabelText('Replacement Administrator access key').fill('replacement-key');
		await page.getByRole('button', { name: 'Verify replacement key' }).click();
		await expect
			.element(page.getByRole('status'))
			.toHaveTextContent('Replacement Administrator verified for this plan.');
		expect(view.controller.verifyAdministratorBearer).toHaveBeenCalledWith(
			'plan',
			'replacement-key'
		);
		expect(
			view.element.querySelector<HTMLInputElement>('input[type="password"]')?.value ?? ''
		).toBe('');
		await expect
			.element(page.getByRole('button', { name: 'Apply authentication changes' }))
			.toBeDisabled();
		await page.getByRole('checkbox').click();
		await tick();
		expect(view.element.querySelector<HTMLInputElement>('input[type="checkbox"]')?.checked).toBe(
			true
		);
		expect(view.element.textContent).not.toContain('expired');
		await expect
			.element(page.getByRole('button', { name: 'Apply authentication changes' }))
			.toBeEnabled();
		await page.getByRole('button', { name: 'Apply authentication changes' }).click();
		expect(view.applied).toHaveBeenCalledWith(view.plan, proofId);
		expect(view.onapplied).toHaveBeenCalledOnce();
	});
	it('discards proof when the parent control session disconnects', async () => {
		const view = fixture();
		await page.getByLabelText('Replacement Administrator access key').fill('replacement-key');
		await page.getByRole('button', { name: 'Verify replacement key' }).click();
		view.disconnect();
		await tick();
		await expect
			.element(page.getByRole('button', { name: 'Apply authentication changes' }))
			.toBeDisabled();
		expect(view.applied).not.toHaveBeenCalled();
		expect(view.element.textContent).toContain('control session changed');
	});
	it('opens verification on an explicit click and polls the bound proof through the same controller', async () => {
		const open = vi.spyOn(window, 'open').mockReturnValue(window);
		const post = vi.spyOn(window, 'postMessage').mockImplementation(() => {});
		const view = fixture(true);
		await page.getByRole('combobox').selectOptions('company');
		await page.getByRole('button', { name: 'Prepare verification' }).click();
		expect(open).not.toHaveBeenCalled();
		await page.getByRole('button', { name: 'Open verification window' }).click();
		window.dispatchEvent(
			new MessageEvent('message', {
				origin: location.origin,
				source: window,
				data: { type: 'keeppeek-verification-ready' }
			})
		);
		expect(post).toHaveBeenCalledWith(
			{
				type: 'keeppeek-verification-start',
				start: {
					origin: location.origin,
					provider_id: 'company',
					csrf_token: 'csrf',
					candidate_plan_id: proofId
				}
			},
			location.origin
		);
		expect(view.controller.getAdministratorVerification).toHaveBeenCalledWith(proofId);
		await expect
			.element(page.getByRole('status'))
			.toHaveTextContent('Replacement Administrator verified for this plan.');
		expect(view.applied).not.toHaveBeenCalled();
	});
	it('fails closed when verification polling is denied or expires', async () => {
		vi.spyOn(window, 'open').mockReturnValue(window);
		const view = fixture(true);
		vi.mocked(view.controller.getAdministratorVerification).mockRejectedValue(new Error('denied'));
		await page.getByRole('combobox').selectOptions('company');
		await page.getByRole('button', { name: 'Prepare verification' }).click();
		await page.getByRole('button', { name: 'Open verification window' }).click();
		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent('Verification failed, expired, or was revoked. Prepare it again.');
		await expect
			.element(page.getByRole('button', { name: 'Apply authentication changes' }))
			.toBeDisabled();
		expect(view.applied).not.toHaveBeenCalled();
	});
	it('expires the plan without attempting an apply', async () => {
		vi.useFakeTimers();
		const view = fixture();
		await tick();
		vi.advanceTimersByTime(61_000);
		await tick();
		expect(view.element.textContent).toContain('This plan expired');
		expect(view.element.querySelector<HTMLButtonElement>('button:last-of-type')?.disabled).toBe(
			true
		);
		expect(view.applied).not.toHaveBeenCalled();
	});
	it('offers enabled external Users in the existing audience ID field and disables revoked identities', async () => {
		const onchange = vi.fn();
		const element = target();
		const identity = {
			$typeName: 'keeppeek.webrtc.v1.ExternalIdentity' as const,
			identityId: 'external-user',
			providerId: 'company',
			providerName: 'Company',
			subjectFingerprint: 'fingerprint',
			displayName: 'Alice',
			role: AccessRole.USER,
			revision: 1n,
			enabled: true,
			createdAtMs: 0n
		};
		mounted.push(
			mount(PeekDashboardAudiencePicker, {
				target: element,
				props: {
					credentials: [],
					externalIdentities: [
						identity,
						{ ...identity, identityId: 'revoked', displayName: 'Former user', enabled: false }
					],
					audience: { everyone: false, credentialIds: [] },
					onchange
				}
			})
		);
		await page.getByText('Alice', { exact: true }).click();
		expect(onchange).toHaveBeenCalledWith({ everyone: false, credentialIds: ['external-user'] });
		expect(element.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')[2].disabled).toBe(
			true
		);
	});
});
