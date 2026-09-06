import { execFile } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { copyFile, mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { promisify, stripVTControlCharacters } from 'node:util';
import {
	cardAdminKey,
	cardTestKeys,
	startHomeAssistantServer
} from '../e2e/fixtures/home-assistant-server';

export const homeAssistantImage =
	'ghcr.io/home-assistant/home-assistant:2026.9.1@sha256:612d76760b544cb40b7ba01387fdac964c59a6a550a50a4d30b4773c822d2918';
const execute = promisify(execFile);
const repositoryRoot = resolve(import.meta.dirname, '../..');

export async function docker(argumentsList: string[], timeoutMs = 30_000): Promise<string> {
	try {
		const { stdout, stderr } = await execute('docker', argumentsList, {
			timeout: timeoutMs,
			maxBuffer: 1_048_576
		});
		return (argumentsList[0] === 'logs' ? `${stdout}\n${stderr}` : stdout).trim();
	} catch {
		throw new Error(
			`Docker ${argumentsList[0]} failed. Check the Docker engine and pinned image availability.`
		);
	}
}

export function sanitizedLog(text: string): string {
	let sanitized = stripVTControlCharacters(text);
	for (const key of [...cardTestKeys, cardAdminKey, 'local-ha-test-password'])
		sanitized = sanitized.replaceAll(key, '[redacted]');
	return sanitized.replace(
		/((?:Bearer\s+)|(?:(?:access_token|refresh_token|password|token)["']?\s*[:=]\s*["']?))[^\s,"'}]+/gi,
		'$1[redacted]'
	);
}

async function configure(directory: string): Promise<void> {
	await mkdir(resolve(directory, 'www'));
	await copyFile(
		resolve(repositoryRoot, 'target/home-assistant-card/dist/keeppeek.js'),
		resolve(directory, 'www/keeppeek.js')
	);
	await writeFile(
		resolve(directory, 'configuration.yaml'),
		JSON.stringify({
			homeassistant: {
				name: 'KeepPeek Test',
				latitude: 0,
				longitude: 0,
				elevation: 0,
				unit_system: 'metric',
				time_zone: 'UTC',
				country: 'SE'
			},
			frontend: {},
			http: {},
			api: {},
			websocket_api: {},
			config: {},
			onboarding: {},
			person: {},
			analytics: {},
			logger: { default: 'warning' },
			lovelace: {
				resource_mode: 'yaml',
				resources: [{ url: '/local/keeppeek.js?v=container-test', type: 'module' }],
				dashboards: {
					'dashboard-keeppeek': {
						mode: 'yaml',
						title: 'KeepPeek',
						icon: 'mdi:cctv',
						show_in_sidebar: true,
						filename: 'keeppeek-dashboard.yaml'
					}
				}
			}
		}),
		{ mode: 0o600 }
	);
	await writeFile(
		resolve(directory, 'secrets.yaml'),
		JSON.stringify({ keeppeek_card_token: cardTestKeys[0] }),
		{ mode: 0o600 }
	);
}

async function writeDashboard(directory: string, endpoint: string): Promise<void> {
	await copyFile(
		resolve(import.meta.dirname, 'dashboard.yaml'),
		resolve(directory, 'keeppeek-dashboard.yaml')
	);
	await writeFile(
		resolve(directory, 'secrets.yaml'),
		JSON.stringify({ keeppeek_card_token: cardTestKeys[0], keeppeek_endpoint: endpoint }),
		{ mode: 0o600 }
	);
}

async function waitForHomeAssistant(url: string): Promise<void> {
	for (let attempt = 0; attempt < 120; attempt += 1) {
		try {
			const response = await fetch(`${url}/api/onboarding`, { signal: AbortSignal.timeout(1000) });
			await response.body?.cancel();
			if (response.ok) return;
		} catch (error) {
			if (!(error instanceof TypeError) && !(error instanceof DOMException)) throw error;
		}
		await delay(500);
	}
	throw new Error('Home Assistant did not become ready within the startup budget.');
}

class HomeAssistantContainer {
	#directory: string;
	#name = `keeppeek-ha-test-${randomUUID()}`;
	#owner = process.env.KEEPPEEK_HA_TEST_OWNER ?? this.#name;
	#created = false;
	#volumeCreated = false;
	#keeppeek: Awaited<ReturnType<typeof startHomeAssistantServer>> | undefined;
	#url = '';

	constructor(directory: string) {
		this.#directory = directory;
	}
	get url(): string {
		return this.#url;
	}
	get keeppeekURL(): string {
		return this.#keeppeek?.url ?? '';
	}

	async start(): Promise<void> {
		await configure(this.#directory);
		await this.createResources();
		await docker(['cp', `${this.#directory}/.`, `${this.#name}:/config`]);
		await docker(['start', this.#name]);
		const inspection = JSON.parse(await docker(['inspect', this.#name])) as Array<{
			NetworkSettings: { Ports: Record<string, Array<{ HostIp: string; HostPort: string }>> };
		}>;
		const binding = inspection[0]?.NetworkSettings.Ports['8123/tcp']?.[0];
		if (binding?.HostIp !== '127.0.0.1' || !/^\d+$/.test(binding.HostPort)) {
			throw new Error('Home Assistant did not bind an isolated loopback port.');
		}
		this.#url = `http://127.0.0.1:${binding.HostPort}`;
		this.#keeppeek = await startHomeAssistantServer(this.#url);
		await writeDashboard(this.#directory, this.#keeppeek.url);
		for (const filename of ['keeppeek-dashboard.yaml', 'secrets.yaml']) {
			await docker(['cp', resolve(this.#directory, filename), `${this.#name}:/config/${filename}`]);
		}
		await waitForHomeAssistant(this.#url);
	}

	private async createResources(): Promise<void> {
		await docker(['volume', 'create', '--label', `keeppeek.test-owner=${this.#owner}`, this.#name]);
		this.#volumeCreated = true;
		await docker([
			'create',
			'--name',
			this.#name,
			'--label',
			'keeppeek.test=home-assistant',
			'--label',
			`keeppeek.test-owner=${this.#owner}`,
			'--publish',
			'127.0.0.1::8123',
			'--cpus',
			'2',
			'--memory',
			'2g',
			'--pids-limit',
			'256',
			'--security-opt',
			'no-new-privileges',
			'--stop-timeout',
			'20',
			'--mount',
			`type=volume,source=${this.#name},target=/config`,
			homeAssistantImage
		]);
		this.#created = true;
	}

	async activeSessions(): Promise<number> {
		if (!this.#keeppeek) throw new Error('KeepPeek is not running.');
		const response = await fetch(`${this.#keeppeek.url}/metrics`, {
			headers: { Authorization: `Bearer ${cardAdminKey}` },
			signal: AbortSignal.timeout(5000)
		});
		if (!response.ok) throw new Error('KeepPeek fixture metrics are unavailable.');
		const metric = /^keeppeek_webrtc_active_sessions (\d+)$/m.exec(await response.text());
		if (!metric) throw new Error('KeepPeek active-session metric is missing.');
		return Number(metric[1]);
	}

	async logs(): Promise<string> {
		return sanitizedLog(
			`${this.#created ? await docker(['logs', '--tail', '200', this.#name]) : ''}\n${this.#keeppeek?.logs() ?? ''}`
		);
	}

	async close(): Promise<void> {
		try {
			if (this.#created) {
				try {
					await docker(['stop', '--time', '20', this.#name]);
				} finally {
					await docker(['rm', '--force', this.#name]);
					this.#created = false;
				}
			}
		} finally {
			try {
				if (this.#volumeCreated) {
					await docker(['volume', 'rm', '--force', this.#name]);
					this.#volumeCreated = false;
				}
			} finally {
				try {
					await this.#keeppeek?.close();
				} finally {
					await rm(this.#directory, { recursive: true, force: true });
				}
			}
		}
	}
}

export async function createHomeAssistantContainer(): Promise<HomeAssistantContainer> {
	await docker(['image', 'inspect', homeAssistantImage]);
	const directory = await mkdtemp(resolve(repositoryRoot, 'target/home-assistant-container-'));
	return new HomeAssistantContainer(directory);
}
