import { create } from '@bufbuild/protobuf';
import {
	AccessRole,
	ExternalAuthenticationSettingsSchema,
	ExternalAuthenticationProviderSchema,
	OidcAuthenticationSettingsSchema,
	ProxyAuthenticationSettingsSchema,
	ExternalRoleMappingSchema,
	CameraAccessPolicySchema,
	ConfigurationCommandSchema,
	GetExternalAuthenticationConfigurationSchema,
	PlanConfigurationChangeSchema,
	ConfigurationChangeSchema,
	ExternalAuthenticationUpdateSchema,
	ApplyConfigurationPlanSchema,
	AdministratorConfirmationSchema,
	ServerCommandSchema,
	ExternalAuthenticationCommandSchema,
	ListExternalIdentitiesSchema,
	ListBrowserSessionsSchema,
	RevokeExternalIdentitySchema,
	RevokeBrowserSessionSchema,
	VerifyAdministratorBearerSchema,
	PrepareAdministratorVerificationSchema,
	GetAdministratorVerificationSchema,
	type Request,
	type Ok,
	type ExternalAuthenticationSettings,
	type ConfigurationPlan,
	type ExternalAuthenticationCommand,
	type ExternalAuthenticationProvider,
	type ExternalIdentity,
	type ExternalIdentityList
} from './proto/webrtc_pb';

type SendRequest = (command: Request['command']) => Promise<Ok['result']>;

export class ExternalAuthenticationAdmin {
	constructor(private readonly send: SendRequest) {}

	async getSettings() {
		const result = await this.send({
			case: 'configurationCommand',
			value: create(ConfigurationCommandSchema, {
				action: {
					case: 'getExternalAuthentication',
					value: create(GetExternalAuthenticationConfigurationSchema)
				}
			})
		});
		if (
			result.case !== 'configurationResult' ||
			result.value.result.case !== 'externalAuthentication'
		)
			throw new Error('Unexpected authentication configuration response.');
		return result.value.result.value;
	}

	async plan(
		settings: ExternalAuthenticationSettings | null,
		expectedConfigurationRevision: string
	) {
		if (settings) validateExternalSettings(settings);
		const result = await this.send({
			case: 'configurationCommand',
			value: create(ConfigurationCommandSchema, {
				action: {
					case: 'plan',
					value: create(PlanConfigurationChangeSchema, {
						expectedConfigurationRevision,
						change: create(ConfigurationChangeSchema, {
							change: {
								case: 'externalAuthentication',
								value: create(ExternalAuthenticationUpdateSchema, {
									value: settings
										? { case: 'set', value: settings }
										: { case: 'clear', value: true }
								})
							}
						})
					})
				}
			})
		});
		if (result.case !== 'configurationResult' || result.value.result.case !== 'plan')
			throw new Error('Unexpected authentication plan response.');
		return result.value.result.value;
	}

	async apply(plan: ConfigurationPlan, verificationId?: string) {
		if (!plan.valid || Number(plan.expiresAtMs) <= Date.now())
			throw new Error('Preview a fresh valid plan before applying.');
		if (plan.requiresAdministratorConfirmation && !verificationId)
			throw new Error('Fresh administrator verification is required.');
		const result = await this.send({
			case: 'configurationCommand',
			value: create(ConfigurationCommandSchema, {
				action: {
					case: 'apply',
					value: create(ApplyConfigurationPlanSchema, {
						planId: plan.planId,
						expectedConfigurationRevision: plan.configurationRevision,
						administratorConfirmation: verificationId
							? create(AdministratorConfirmationSchema, { verificationId, confirm: true })
							: undefined
					})
				}
			})
		});
		if (result.case !== 'configurationResult' || result.value.result.case !== 'applied')
			throw new Error('Unexpected authentication apply response.');
		return result.value.result.value;
	}

	private async command(action: ExternalAuthenticationCommand['action']) {
		const result = await this.send({
			case: 'serverCommand',
			value: create(ServerCommandSchema, {
				action: {
					case: 'externalAuthentication',
					value: create(ExternalAuthenticationCommandSchema, { action })
				}
			})
		});
		if (result.case !== 'externalAuthenticationResult')
			throw new Error('Unexpected external authentication response.');
		return result.value.result;
	}

	async listIdentities(pageToken = '') {
		const result = await this.command({
			case: 'listIdentities',
			value: create(ListExternalIdentitiesSchema, { pageSize: 32, pageToken })
		});
		if (result.case !== 'identities') throw new Error('Unexpected identity directory response.');
		return result.value;
	}

	async listSessions(pageToken = '') {
		const result = await this.command({
			case: 'listSessions',
			value: create(ListBrowserSessionsSchema, { pageSize: 32, pageToken })
		});
		if (result.case !== 'sessions') throw new Error('Unexpected browser session response.');
		return result.value;
	}

	async revokeIdentity(identityId: string, expectedRevision: bigint) {
		const result = await this.command({
			case: 'revokeIdentity',
			value: create(RevokeExternalIdentitySchema, { identityId, expectedRevision })
		});
		if (result.case !== 'identities') throw new Error('Unexpected identity revocation response.');
		return result.value;
	}

	async revokeSession(sessionId: string) {
		const result = await this.command({
			case: 'revokeSession',
			value: create(RevokeBrowserSessionSchema, { sessionId })
		});
		if (result.case !== 'sessions') throw new Error('Unexpected session revocation response.');
		return result.value;
	}

	async prepare(configurationPlanId: string, providerId: string, origin: string) {
		const result = await this.command({
			case: 'prepareAdministratorVerification',
			value: create(PrepareAdministratorVerificationSchema, {
				configurationPlanId,
				providerId,
				origin
			})
		});
		if (result.case !== 'administratorVerification')
			throw new Error('Unexpected verification response.');
		return result.value;
	}

	async getVerification(verificationId: string) {
		const result = await this.command({
			case: 'getAdministratorVerification',
			value: create(GetAdministratorVerificationSchema, { verificationId })
		});
		if (result.case !== 'administratorVerification')
			throw new Error('Unexpected verification response.');
		return result.value;
	}

	async verifyBearer(configurationPlanId: string, accessKey: string) {
		const result = await this.command({
			case: 'verifyAdministratorBearer',
			value: create(VerifyAdministratorBearerSchema, { configurationPlanId, accessKey })
		});
		if (result.case !== 'administratorVerification')
			throw new Error('Unexpected verification response.');
		return result.value;
	}
}

export function newExternalProvider(
	kind: 'oidc' | 'proxy' = 'oidc'
): ExternalAuthenticationProvider {
	return create(ExternalAuthenticationProviderSchema, {
		providerId: '',
		name: '',
		mappings: [
			create(ExternalRoleMappingSchema, {
				claim: 'role',
				value: '',
				role: AccessRole.USER,
				cameraAccess: create(CameraAccessPolicySchema)
			})
		],
		method:
			kind === 'oidc'
				? {
						case: 'oidc',
						value: create(OidcAuthenticationSettingsSchema, {
							scopes: ['openid'],
							displayNameClaim: 'name'
						})
					}
				: {
						case: 'proxy',
						value: create(ProxyAuthenticationSettingsSchema, {
							subjectHeader: 'X-Identity-Subject',
							roleHeader: 'X-Identity-Role'
						})
					}
	});
}

export function newExternalSettings() {
	return create(ExternalAuthenticationSettingsSchema, { providers: [newExternalProvider()] });
}

export function textList(value: string): string[] {
	return value
		.split(/[\n,]/)
		.map((part) => part.trim())
		.filter(Boolean);
}

export async function externalAudienceIdentities(
	list: (pageToken: string) => Promise<ExternalIdentityList>
): Promise<ExternalIdentity[]> {
	const identities: ExternalIdentity[] = [];
	const seen = new Set<string>();
	let token = '';
	for (let page = 0; page < 64; page++) {
		const result = await list(token);
		if (result.identities.length > 64) throw new Error('External identity page exceeds its limit.');
		identities.push(...result.identities);
		if (!result.nextPageToken) return identities;
		if (seen.has(result.nextPageToken)) throw new Error('External identity pagination repeated.');
		seen.add(result.nextPageToken);
		token = result.nextPageToken;
	}
	throw new Error('External identity directory exceeds 4096 entries.');
}

function exactOrigins(origins: string[], required: boolean): void {
	if (
		(required && origins.length === 0) ||
		origins.length > 16 ||
		new Set(origins).size !== origins.length
	)
		throw new Error('Enter 1–16 unique allowed origins.');
	for (const origin of origins) {
		const url = new URL(origin);
		if (url.protocol !== 'https:' || url.origin !== origin || url.hostname.includes('*'))
			throw new Error('Origins must be exact HTTPS origins without a path, query, or credentials.');
	}
}

export function validateExternalSettings(settings: ExternalAuthenticationSettings): void {
	exactOrigins(settings.allowedOrigins, true);
	if (!settings.providers.length || settings.providers.length > 4)
		throw new Error('Configure 1–4 providers.');
	if (
		settings.bearerEnabled &&
		(!settings.bearerTransitionUntilMs || Number(settings.bearerTransitionUntilMs) <= Date.now())
	)
		throw new Error('Bearer transition requires a future deadline.');
	if (!settings.bearerEnabled && settings.bearerTransitionUntilMs !== undefined)
		throw new Error('Disable the bearer transition deadline with bearer access.');
	const ids = new Set<string>();
	let mappingCount = 0;
	for (const provider of settings.providers) {
		if (!/^[a-zA-Z0-9_-]{1,64}$/.test(provider.providerId) || ids.has(provider.providerId))
			throw new Error(
				'Provider IDs must be unique and contain only letters, digits, hyphens, or underscores.'
			);
		ids.add(provider.providerId);
		if (!provider.name.trim() || provider.name.length > 64)
			throw new Error('Provider names require 1–64 characters.');
		mappingCount += provider.mappings.length;
		if (!provider.mappings.length || mappingCount > 128)
			throw new Error('Each provider needs a mapping; at most 128 mappings are allowed.');
		for (const mapping of provider.mappings) {
			if (!mapping.claim.trim() || !mapping.value.trim())
				throw new Error('Each mapping needs a claim and exact value.');
			if (mapping.role === AccessRole.USER && !mapping.cameraAccess)
				throw new Error('User mappings need an explicit camera policy.');
		}
		validateProviderMethod(provider, settings.allowedOrigins);
	}
}

function httpsUrl(value: string): URL {
	const url = new URL(value);
	if (
		value.length > 2048 ||
		url.protocol !== 'https:' ||
		url.username ||
		url.password ||
		url.hash ||
		url.search
	)
		throw new Error(
			'Provider URLs require HTTPS without credentials, query strings, or fragments.'
		);
	return url;
}

function validateProviderMethod(provider: ExternalAuthenticationProvider, origins: string[]): void {
	const method = provider.method;
	if (!method.case) throw new Error('Choose an authentication method.');
	const secret =
		method.case === 'oidc'
			? method.value.clientSecretReference
			: method.value.sharedSecretReference;
	if (secret !== undefined && !/^\{secret:[A-Z_][A-Z0-9_]*(?:\|url)?\}$/.test(secret))
		throw new Error('Use a complete {secret:KEY} reference, never a secret value.');
	if (method.case === 'oidc') {
		const oidc = method.value;
		httpsUrl(oidc.issuer);
		if (!oidc.clientId.trim() || !oidc.displayNameClaim.trim())
			throw new Error('Enter a client ID and display name claim.');
		if (
			!oidc.scopes.includes('openid') ||
			oidc.scopes.length > 16 ||
			new Set(oidc.scopes).size !== oidc.scopes.length
		)
			throw new Error('Enter unique scopes including openid (maximum 16).');
		exactOrigins(oidc.endpointOrigins, false);
		const callback = httpsUrl(oidc.redirectUri);
		if (!origins.includes(callback.origin) || callback.pathname !== '/auth/callback')
			throw new Error('Redirect URI must be an allowed origin followed by /auth/callback.');
		if (oidc.logoutUri) httpsUrl(oidc.logoutUri);
		if (oidc.privateNetworks.length > 64)
			throw new Error('At most 64 private issuer networks are allowed.');
	} else {
		const proxy = method.value;
		if (!proxy.trustedPeers.length || proxy.trustedPeers.length > 64)
			throw new Error('Enter 1–64 trusted immediate proxy peer CIDRs.');
		if (Boolean(proxy.secretHeader) !== Boolean(proxy.sharedSecretReference))
			throw new Error('A shared secret needs both its header and secret reference.');
		const headers = [
			proxy.subjectHeader,
			proxy.roleHeader,
			proxy.nameHeader,
			proxy.secretHeader
		].filter((header): header is string => header !== undefined);
		if (
			headers.some(
				(header) => !/^x-[a-z0-9-]{1,62}$/i.test(header) || /^x-forwarded-/i.test(header)
			) ||
			new Set(headers.map((header) => header.toLowerCase())).size !== headers.length
		)
			throw new Error('Identity headers must be distinct non-forwarding X- headers.');
	}
}
