<script lang="ts">
	import type {
		AccessAuditEvent,
		AccessCredential,
		AccessCredentialInput,
		AccessSession,
		CameraAccessSettings,
		IssuedAccessCredential
	} from '$lib/access';
	import AccessRuntime from '$lib/components/AccessRuntime.svelte';
	import DesktopPaperRail from '$lib/components/DesktopPaperRail.svelte';
	import { ControlClient } from '$lib/control-client';
	import { cameraAccessCapability } from '$lib/control-client-camera-access';
	import type { CameraListItem } from '$lib/types';
	import CheckIcon from '@lucide/svelte/icons/check';
	import ShieldCheckIcon from '@lucide/svelte/icons/shield-check';

	const baseTimestampMs = Date.parse('2026-08-18T08:15:00Z');
	const initialAdminId = '11111111-1111-4111-8111-111111111111';
	const frontDeskId = '22222222-2222-4222-8222-222222222222';
	const demoCameras: CameraListItem[] = [
		{
			id: 'front-door',
			ip: '192.0.2.61',
			name: 'Front Door',
			manufacturer: 'Reolink',
			model: 'RLC-811A',
			firmware_version: 'v3.1.0',
			is_reolink: true,
			profiles: []
		},
		{
			id: 'driveway',
			ip: '192.0.2.62',
			name: 'Driveway',
			manufacturer: 'Reolink',
			model: 'RLC-811A',
			firmware_version: 'v3.1.0',
			is_reolink: true,
			profiles: []
		},
		{
			id: 'back-door',
			ip: '192.0.2.63',
			name: 'Back Door',
			manufacturer: 'ONVIF',
			model: 'Doorway Cam',
			firmware_version: null,
			is_reolink: false,
			profiles: []
		},
		{
			id: 'hallway',
			ip: '192.0.2.64',
			name: 'Hallway',
			manufacturer: 'ONVIF',
			model: 'Indoor Dome',
			firmware_version: null,
			is_reolink: false,
			profiles: []
		}
	];

	class DemoAccessControlClient extends ControlClient {
		#credentials: AccessCredential[] = [
			{
				id: initialAdminId,
				name: 'Initial Administrator',
				description: 'Local bootstrap recovery key',
				role: 'administrator',
				createdAtMs: baseTimestampMs - 86_400_000 * 4,
				rotatedAtMs: null,
				lastUsedAtMs: baseTimestampMs - 600_000,
				expiresAtMs: null,
				disabled: false,
				revokedAtMs: null,
				revision: 1n,
				initialAccessKeyPending: false
			},
			{
				id: frontDeskId,
				name: 'Front Desk Tablet',
				description: 'Reception wall monitor',
				role: 'user',
				createdAtMs: baseTimestampMs - 86_400_000 * 2,
				rotatedAtMs: null,
				lastUsedAtMs: baseTimestampMs - 120_000,
				expiresAtMs: null,
				disabled: false,
				revokedAtMs: null,
				revision: 2n,
				initialAccessKeyPending: false
			}
		];

		#sessions: AccessSession[] = [
			{
				id: 'sess-local-admin-01',
				principalId: initialAdminId,
				displayName: 'Local Administrator',
				role: 'administrator',
				local: true,
				clientClassification: 'local',
				authentication: { method: 'trusted-local', providerId: null, identityId: null },
				createdAtMs: baseTimestampMs - 1_800_000,
				lastActivityAtMs: baseTimestampMs - 5_000,
				absoluteExpiresAtMs: baseTimestampMs + 82_800_000,
				credentialExpiresAtMs: null
			},
			{
				id: 'sess-front-desk-02',
				principalId: frontDeskId,
				displayName: 'Front Desk Tablet',
				role: 'user',
				local: false,
				clientClassification: 'remote-proxy',
				authentication: { method: 'bearer', providerId: null, identityId: null },
				createdAtMs: baseTimestampMs - 900_000,
				lastActivityAtMs: baseTimestampMs - 15_000,
				absoluteExpiresAtMs: baseTimestampMs + 84_600_000,
				credentialExpiresAtMs: null
			}
		];

		#audit: AccessAuditEvent[] = [
			{
				id: 'audit-01',
				timestampMs: baseTimestampMs - 900_000,
				principalId: frontDeskId,
				role: 'user',
				action: 'session_created',
				targetId: 'sess-front-desk-02',
				result: 'allowed',
				clientClassification: 'remote-proxy'
			},
			{
				id: 'audit-02',
				timestampMs: baseTimestampMs - 300_000,
				principalId: initialAdminId,
				role: 'administrator',
				action: 'access_policy_inspected',
				targetId: initialAdminId,
				result: 'allowed',
				clientClassification: 'local'
			}
		];

		#cameraAccess = new Map<string, CameraAccessSettings>([
			[
				frontDeskId,
				{
					credentialId: frontDeskId,
					allCameras: false,
					groupIds: ['outdoor'],
					cameraIds: [],
					availableGroupIds: ['outdoor', 'indoor'],
					revision: 1n
				}
			]
		]);

		#nextCredentialIndex = 3;
		#nextAuditIndex = 3;
		#onGrantSaved: (settings: CameraAccessSettings) => void;
		#onSessionRevoked: (sessionId: string) => void;

		constructor(
			onGrantSaved: (settings: CameraAccessSettings) => void,
			onSessionRevoked: (sessionId: string) => void
		) {
			super();
			this.#onGrantSaved = onGrantSaved;
			this.#onSessionRevoked = onSessionRevoked;
		}

		override onCapabilities(listener: (capabilityIds: readonly string[]) => void): () => void {
			listener([cameraAccessCapability]);
			return () => {};
		}

		override async listAccessCredentials(): Promise<AccessCredential[]> {
			return [...this.#credentials];
		}

		override async listAccessSessions(): Promise<AccessSession[]> {
			return [...this.#sessions];
		}

		override async listAccessAudit(limit = 40): Promise<AccessAuditEvent[]> {
			return this.#audit.slice(-limit);
		}

		override async getCameras(): Promise<CameraListItem[]> {
			return demoCameras;
		}

		override async createAccessCredential(
			input: AccessCredentialInput
		): Promise<IssuedAccessCredential> {
			const slot = this.#nextCredentialIndex++;
			const id = `${slot}${slot}${slot}${slot}${slot}${slot}${slot}${slot}-3333-4333-8333-333333333333`;
			const credential: AccessCredential = {
				id,
				name: input.name.trim(),
				description: input.description?.trim() || null,
				role: input.role,
				createdAtMs: baseTimestampMs + slot * 1_000,
				rotatedAtMs: null,
				lastUsedAtMs: null,
				expiresAtMs: input.expiresAtMs ?? null,
				disabled: false,
				revokedAtMs: null,
				revision: 1n,
				initialAccessKeyPending: false
			};
			this.#credentials = [...this.#credentials, credential];
			this.#cameraAccess.set(id, {
				credentialId: id,
				allCameras: true,
				groupIds: [],
				cameraIds: [],
				availableGroupIds: ['outdoor', 'indoor'],
				revision: 1n
			});
			this.#appendAudit('credential_created', id, 'allowed');
			return {
				credential,
				accessKey: '9b7c4e21-3a8f-4c12-b6d4-5e9a1f2c8d30'
			};
		}

		override async getCameraAccess(credentialId: string): Promise<CameraAccessSettings> {
			const existing = this.#cameraAccess.get(credentialId);
			if (existing) {
				return {
					...existing,
					groupIds: [...existing.groupIds],
					cameraIds: [...existing.cameraIds],
					availableGroupIds: [...existing.availableGroupIds]
				};
			}
			return {
				credentialId,
				allCameras: true,
				groupIds: [],
				cameraIds: [],
				availableGroupIds: ['outdoor', 'indoor'],
				revision: 1n
			};
		}

		override async saveCameraAccess(settings: CameraAccessSettings): Promise<CameraAccessSettings> {
			const saved: CameraAccessSettings = {
				...settings,
				groupIds: [...settings.groupIds],
				cameraIds: [...settings.cameraIds],
				availableGroupIds: [...settings.availableGroupIds],
				revision: settings.revision + 1n
			};
			this.#cameraAccess.set(settings.credentialId, saved);
			this.#sessions = this.#sessions.filter(
				(session) => session.principalId !== settings.credentialId
			);
			this.#appendAudit('camera_access_updated', settings.credentialId, 'allowed');
			this.#onGrantSaved(saved);
			return saved;
		}

		override async revokeAccessSession(sessionId: string): Promise<void> {
			this.#sessions = this.#sessions.filter((session) => session.id !== sessionId);
			this.#appendAudit('session_revoked', sessionId, 'allowed');
			this.#onSessionRevoked(sessionId);
		}

		override async rotateAccessCredential(credentialId: string): Promise<IssuedAccessCredential> {
			const target = this.#credentials.find((item) => item.id === credentialId);
			if (!target) throw new Error('Credential was not found.');
			const rotated: AccessCredential = {
				...target,
				rotatedAtMs: baseTimestampMs + 20_000,
				revision: target.revision + 1n
			};
			this.#credentials = this.#credentials.map((item) =>
				item.id === credentialId ? rotated : item
			);
			this.#appendAudit('credential_rotated', credentialId, 'allowed');
			return {
				credential: rotated,
				accessKey: '4c2d8a90-7e1b-4f53-a210-6b3e9c8f1d45'
			};
		}

		override async setAccessCredentialEnabled(
			credentialId: string,
			enabled: boolean
		): Promise<AccessCredential> {
			const target = this.#credentials.find((item) => item.id === credentialId);
			if (!target) throw new Error('Credential was not found.');
			const updated: AccessCredential = {
				...target,
				disabled: !enabled,
				revision: target.revision + 1n
			};
			this.#credentials = this.#credentials.map((item) =>
				item.id === credentialId ? updated : item
			);
			this.#appendAudit(
				enabled ? 'credential_enabled' : 'credential_disabled',
				credentialId,
				'allowed'
			);
			return updated;
		}

		override async revokeAccessCredential(credentialId: string): Promise<AccessCredential> {
			const target = this.#credentials.find((item) => item.id === credentialId);
			if (!target) throw new Error('Credential was not found.');
			const revoked: AccessCredential = {
				...target,
				revokedAtMs: baseTimestampMs + 30_000,
				revision: target.revision + 1n
			};
			this.#credentials = this.#credentials.map((item) =>
				item.id === credentialId ? revoked : item
			);
			this.#appendAudit('credential_revoked', credentialId, 'allowed');
			return revoked;
		}

		#appendAudit(action: string, targetId: string, result: string): void {
			const index = this.#nextAuditIndex++;
			this.#audit = [
				...this.#audit,
				{
					id: `audit-${index}`,
					timestampMs: baseTimestampMs + index * 1_000,
					principalId: initialAdminId,
					role: 'administrator',
					action,
					targetId,
					result,
					clientClassification: 'local'
				}
			];
		}
	}

	let savedGrantSummary = $state<string | null>(null);
	let sessionRevoked = $state(false);
	let demoCompleted = $derived(savedGrantSummary !== null && sessionRevoked);

	const controller = new DemoAccessControlClient(
		(settings) => {
			savedGrantSummary = settings.allCameras
				? 'Everything'
				: `${settings.groupIds.length} group · ${settings.cameraIds.length} camera`;
		},
		() => {
			sessionRevoked = true;
		}
	);
</script>

<main
	data-paper-scenario="settings.desktop.access"
	class="flex h-[800px] w-[1280px] overflow-hidden bg-ground text-foreground [font-synthesis:none]"
>
	<DesktopPaperRail active="settings" />

	<div class="flex min-h-0 min-w-0 flex-1 flex-col">
		<header
			class="flex h-[56px] shrink-0 items-center justify-between gap-4 border-b border-hairline bg-surface px-5"
		>
			<div class="flex items-baseline gap-3">
				<h1 class="text-base font-semibold">Settings</h1>
				<span class="font-mono text-2xs tracking-caps text-primary-soft">ACCESS</span>
				<span class="text-xs text-text-muted">
					Local networks: Administrator · Remote: named bearer credentials
				</span>
			</div>

			<div class="flex items-center gap-2">
				{#if savedGrantSummary}
					<span
						class="inline-flex items-center gap-1.5 rounded-full border border-primary/40 bg-primary/10 px-2.5 py-1 font-mono text-2xs text-primary-soft"
					>
						<ShieldCheckIcon class="size-3" /> GRANT: {savedGrantSummary.toUpperCase()}
					</span>
				{/if}
				{#if demoCompleted}
					<span
						data-demo-access-complete="true"
						class="inline-flex items-center gap-1.5 rounded-full border border-healthy/40 bg-healthy/10 px-2.5 py-1 font-mono text-2xs text-healthy"
					>
						<CheckIcon class="size-3" /> GRANTS SAVED · SESSION REVOKED
					</span>
				{/if}
			</div>
		</header>

		<div class="min-h-0 flex-1 overflow-y-auto bg-surface">
			<AccessRuntime {controller} />
		</div>
	</div>
</main>
