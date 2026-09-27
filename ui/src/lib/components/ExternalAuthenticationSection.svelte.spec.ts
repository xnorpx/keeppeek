import { create } from '@bufbuild/protobuf';
import { mount, unmount, type ComponentProps } from 'svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import {
	ConfigurationPlanSchema,
	ExternalAuthenticationConfigurationSchema,
	ExternalIdentityListSchema,
	BrowserSessionListSchema,
	ExternalIdentitySchema,
	BrowserSessionSchema,
	AccessRole,
	ConfigurationApplyResultSchema,
	AdministratorVerificationSchema
} from '$lib/proto/webrtc_pb';
import { newExternalSettings, validateExternalSettings } from '$lib/external-authentication-admin';
import ExternalAuthenticationSection from './ExternalAuthenticationSection.svelte';
import '../../app.css';

const components: Array<ReturnType<typeof mount>> = [];
const targets: HTMLElement[] = [];
afterEach(async () => {
	for (const component of components.splice(0)) await unmount(component);
	for (const target of targets.splice(0)) target.remove();
	vi.restoreAllMocks();
});

function settingsFixture() {
	const settings = newExternalSettings();
	settings.allowedOrigins = ['https://recorder.example'];
	const provider = settings.providers[0];
	provider.providerId = 'company';
	provider.name = 'Company';
	provider.mappings[0].value = 'viewers';
	if (provider.method.case !== 'oidc') throw new Error('Expected OIDC');
	Object.assign(provider.method.value, {
		issuer: 'https://identity.example',
		clientId: 'keeppeek',
		clientSecretReference: '{secret:OIDC_CLIENT}',
		redirectUri: 'https://recorder.example/auth/callback'
	});
	return settings;
}

function fixture(
	configure?: (
		controller: ComponentProps<typeof ExternalAuthenticationSection>['controller']
	) => void
) {
	const settings = settingsFixture();
	const plan = create(ConfigurationPlanSchema, {
		planId: 'plan',
		configurationRevision: 'revision',
		valid: true,
		expiresAtMs: BigInt(Date.now() + 60_000),
		applySemantics: 'Browser sessions are invalidated.'
	});
	const controller: ComponentProps<typeof ExternalAuthenticationSection>['controller'] = {
		getExternalAuthentication: vi.fn().mockResolvedValue(
			create(ExternalAuthenticationConfigurationSchema, {
				configurationRevision: 'revision',
				settings
			})
		),
		listExternalIdentities: vi.fn().mockResolvedValue(create(ExternalIdentityListSchema)),
		listBrowserSessions: vi.fn().mockResolvedValue(create(BrowserSessionListSchema)),
		planExternalAuthentication: vi.fn(async (candidate) => {
			if (candidate) validateExternalSettings(candidate);
			return plan;
		}),
		revokeExternalIdentity: vi.fn().mockResolvedValue(create(ExternalIdentityListSchema)),
		revokeBrowserSession: vi.fn().mockResolvedValue(create(BrowserSessionListSchema)),
		onAccessState: () => () => {},
		prepareAdministratorVerification: vi
			.fn()
			.mockResolvedValue(create(AdministratorVerificationSchema)),
		getAdministratorVerification: vi
			.fn()
			.mockResolvedValue(create(AdministratorVerificationSchema)),
		verifyAdministratorBearer: vi.fn().mockResolvedValue(create(AdministratorVerificationSchema)),
		applyExternalAuthentication: vi
			.fn()
			.mockResolvedValue(create(ConfigurationApplyResultSchema, { configurationCommitted: true }))
	};
	configure?.(controller);
	const target = document.createElement('div');
	document.body.append(target);
	targets.push(target);
	components.push(mount(ExternalAuthenticationSection, { target, props: { controller } }));
	return { controller, target };
}

describe('external authentication settings form', () => {
	it('shows stable identity fingerprints and browser session age without raw subjects', async () => {
		const now = Date.now();
		vi.spyOn(Date, 'now').mockReturnValue(now);
		fixture((controller) => {
			vi.mocked(controller.listExternalIdentities).mockResolvedValue(
				create(ExternalIdentityListSchema, {
					identities: [
						create(ExternalIdentitySchema, {
							identityId: 'identity',
							providerId: 'company',
							providerName: 'Company',
							displayName: 'Alice',
							role: AccessRole.USER,
							enabled: true,
							subjectFingerprint: 'stable-subject-fingerprint'
						})
					]
				})
			);
			vi.mocked(controller.listBrowserSessions).mockResolvedValue(
				create(BrowserSessionListSchema, {
					sessions: [
						create(BrowserSessionSchema, {
							sessionId: 'browser',
							identityId: 'identity',
							createdAtMs: BigInt(now - 180_000),
							lastActivityAtMs: BigInt(now),
							absoluteExpiresAtMs: BigInt(now + 60_000)
						})
					]
				})
			);
		});
		await expect
			.element(page.getByText('Subject fingerprint: stable-subject-fingerprint'))
			.toBeVisible();
		await expect.element(page.getByText('Session age: 3 min')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Revoke identity Alice' })).toBeEnabled();
	});
	it('preserves secret references in previews and discards the plan after an edit', async () => {
		const { controller, target } = fixture();
		await page.getByLabelText('Client secret reference').fill('{secret:NEW_CLIENT}');
		await page.getByRole('button', { name: 'Preview authentication changes' }).click();
		await expect
			.element(page.getByRole('heading', { name: 'Review authentication changes' }))
			.toBeVisible();
		const candidate = vi.mocked(controller.planExternalAuthentication).mock.calls[0][0];
		if (candidate?.providers[0].method.case !== 'oidc') throw new Error('Expected OIDC candidate');
		expect(candidate.providers[0].method.value.clientSecretReference).toBe('{secret:NEW_CLIENT}');
		expect(controller.applyExternalAuthentication).not.toHaveBeenCalled();
		await page.getByLabelText('Client ID', { exact: true }).fill('new-client');
		await expect
			.element(page.getByRole('heading', { name: 'Review authentication changes' }))
			.not.toBeInTheDocument();
		expect(target.textContent).not.toContain('Replacement Administrator verified');
	});
	it('shows validation errors for inline secrets without applying', async () => {
		const { controller } = fixture();
		await page.getByLabelText('Client secret reference').fill('not-a-reference');
		await page.getByRole('button', { name: 'Preview authentication changes' }).click();
		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent('Use a complete {secret:KEY} reference, never a secret value.');
		expect(controller.applyExternalAuthentication).not.toHaveBeenCalled();
	});
	it('edits trusted proxy headers, peers, and explicit User camera grants', async () => {
		const { controller } = fixture();
		await page.getByRole('combobox', { name: /^Method/ }).selectOptions('proxy');
		await page.getByLabelText('Trusted immediate peers (CIDRs)').fill('203.0.113.10/32');
		await page.getByLabelText('Shared secret header (optional)').fill('X-Identity-Secret');
		await page.getByLabelText('Shared secret reference').fill('{secret:PROXY_SECRET}');
		await page.getByLabelText('Camera IDs (comma separated)').fill('front-door');
		await page.getByRole('button', { name: 'Preview authentication changes' }).click();
		await expect
			.element(page.getByRole('heading', { name: 'Review authentication changes' }))
			.toBeVisible();
		const candidate = vi.mocked(controller.planExternalAuthentication).mock.calls[0][0];
		if (candidate?.providers[0].method.case !== 'proxy')
			throw new Error('Expected proxy candidate');
		expect(candidate.providers[0].method.value.sharedSecretReference).toBe('{secret:PROXY_SECRET}');
		expect(candidate.providers[0].mappings[0].cameraAccess?.cameraIds).toEqual(['front-door']);
		expect(controller.applyExternalAuthentication).not.toHaveBeenCalled();
	});
});
