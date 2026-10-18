import type { RuntimeProfilesControlClient } from '../../src/lib/control-client-runtime-profiles';
import {
	runtimeProfileTargetForSource,
	type RuntimeProfileRegistry
} from '../../src/lib/runtime-profiles';
import type { StateStoreWatch } from '../../src/lib/control-client-state-store';

type BrowserControlClient = {
	runtimeProfiles: RuntimeProfilesControlClient;
	close(): Promise<void>;
};

async function createClient(): Promise<BrowserControlClient> {
	const modulePath = '/src/lib/control-client.ts';
	const loaded = (await import(/* @vite-ignore */ modulePath)) as {
		ControlClient: new () => BrowserControlClient;
	};
	return new loaded.ControlClient();
}

async function until(predicate: () => boolean, timeoutMs = 15_000): Promise<void> {
	const deadline = performance.now() + timeoutMs;
	while (!predicate()) {
		if (performance.now() >= deadline) {
			throw new Error('Runtime-profile watch did not converge.');
		}
		await new Promise((resolve) => setTimeout(resolve, 20));
	}
}

export async function activateFromAnotherClient() {
	const writer = await createClient();
	const reader = await createClient();
	const profileId = `e2e-${crypto.randomUUID()}`;
	const observed: Array<{ profileId: string | null; revision: string }> = [];
	let watch: StateStoreWatch | undefined;
	let registrySaved = false;
	try {
		let registry = await writer.runtimeProfiles.getRegistry();
		if (registry.profiles && Object.keys(registry.profiles).length !== 0) {
			const active = await writer.runtimeProfiles.getActive();
			if (active.profileId !== null) {
				await writer.runtimeProfiles.activate(null, active.revision);
			}
			registry = await writer.runtimeProfiles.getRegistry();
			await writer.runtimeProfiles.saveRegistry({
				...registry,
				active_profile_id: null,
				profiles: {}
			});
		}
		const bindings = await writer.runtimeProfiles.getCameras();
		const source = Object.values(bindings)[0];
		const target = source ? runtimeProfileTargetForSource(bindings, source) : null;
		const candidate: RuntimeProfileRegistry = {
			...registry,
			active_profile_id: null,
			profiles: {
				[profileId]: {
					id: profileId,
					name: 'External E2E',
					revision: '0',
					cameras: target ? { [target]: { availability: true } } : {}
				}
			}
		};
		const saved = await writer.runtimeProfiles.saveRegistry(candidate);
		registrySaved = true;
		watch = await reader.runtimeProfiles.watchActive((active) => observed.push(active));
		const activatedRevision = await writer.runtimeProfiles.activate(profileId, saved.revision);
		await until(() =>
			observed.some(
				(update) => update.profileId === profileId && update.revision === activatedRevision
			)
		);
		const readerActive = await reader.runtimeProfiles.getActive();
		return { activeProfileId: profileId, activatedRevision, observed, readerActive };
	} finally {
		try {
			if (registrySaved) {
				const active = await writer.runtimeProfiles.getActive();
				if (active.profileId === profileId) {
					await writer.runtimeProfiles.activate(null, active.revision);
				}
				const current = await writer.runtimeProfiles.getRegistry();
				const profiles = { ...current.profiles };
				delete profiles[profileId];
				await writer.runtimeProfiles.saveRegistry({
					...current,
					active_profile_id: null,
					profiles
				});
			}
		} finally {
			if (watch) await watch.close();
			await Promise.all([writer.close(), reader.close()]);
		}
	}
}
