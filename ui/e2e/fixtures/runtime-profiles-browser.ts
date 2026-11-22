import type { RuntimeProfilesControlClient } from '../../src/lib/control-client-runtime-profiles';
import {
	runtimeProfileTargetForSource,
	type RuntimeProfileRegistry
} from '../../src/lib/runtime-profiles';
import type { ProfileSubscription } from '../../src/lib/runtime-profile-shared-watch';

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

export async function measureProfileActivations(size: { cameras: number; profiles: number }) {
	const writer = await createClient();
	const api = writer.runtimeProfiles;
	const now = () => performance.timeOrigin + performance.now();
	const received = new Map<string, { id: string | null; time: number }>();
	let watch: ProfileSubscription | undefined;
	try {
		const [registry, bindings, initial] = await Promise.all([
			api.getRegistry(),
			api.getCameras(),
			api.getActive()
		]);
		if (
			Object.keys(bindings).length !== size.cameras ||
			Object.keys(registry.profiles).length !== size.profiles ||
			initial.profileId !== null
		) {
			throw new Error('Release fixture does not match its declared capacity and Base selection.');
		}
		let revision = initial.revision;
		watch = await api.watchActive(({ revision, profileId }) => {
			if (!received.has(revision)) received.set(revision, { id: profileId, time: now() });
		});
		await until(() => received.has(revision));
		return {
			async activate(index: number) {
				const id = 'qual-' + String(index).padStart(2, '0');
				const start = now();
				revision = await api.activate(id, revision, { ...registry, revision });
				const ack = now();
				await until(() => received.get(revision)?.id === id);
				return { id, revision, start, ack, watch: received.get(revision)!.time };
			},
			async convergence(revision: string) {
				const deadline = performance.now() + 15_000;
				let status = await api.getStatus();
				while (
					performance.now() < deadline &&
					(status.revision !== revision ||
						Object.values(status.cameras).some((camera) => camera.state === 'pending'))
				) {
					await new Promise((resolve) => setTimeout(resolve, 100));
					status = await api.getStatus();
				}
				const cameras = Object.values(status.cameras);
				return {
					revision: status.revision,
					observed: now(),
					count: cameras.length,
					pending: cameras
						.filter((camera) => camera.state === 'pending')
						.map((camera) => camera.camera_id),
					degraded: cameras
						.filter((camera) => camera.state === 'degraded')
						.map((camera) => ({ id: camera.camera_id, reason: camera.reason }))
				};
			},
			async close() {
				try {
					await watch?.close();
				} finally {
					await writer.close();
				}
			}
		};
	} catch (error) {
		try {
			await watch?.close();
		} finally {
			await writer.close();
		}
		throw error;
	}
}

export async function activateFromAnotherClient() {
	const writer = await createClient();
	const reader = await createClient();
	const profileId = `e2e-${crypto.randomUUID()}`;
	const observed: Array<{ profileId: string | null; revision: string }> = [];
	let watch: ProfileSubscription | undefined;
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
		const activatedRevision = await writer.runtimeProfiles.activate(
			profileId,
			saved.revision,
			saved
		);
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
