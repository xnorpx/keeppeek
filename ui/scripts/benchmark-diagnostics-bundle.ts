import { performance } from 'node:perf_hooks';
import {
	buildDiagnosticsBundle,
	compressDiagnosticsDocument,
	type DiagnosticsBundleInput
} from '../src/lib/diagnostics-bundle';
import type { BrowserLogEntry, CameraListItem, ServerLogEntry } from '../src/lib/types';

type Sample = {
	assemblyMs: number;
	compressionMs: number;
	totalMs: number;
	inputBytes: number;
	compressedBytes: number;
};

function positiveInteger(name: string, fallback: number): number {
	const index = process.argv.indexOf(`--${name}`);
	const value = Number(index >= 0 ? process.argv[index + 1] : fallback);
	if (!Number.isSafeInteger(value) || value <= 0) throw new Error(`--${name} must be positive`);
	return value;
}

function message(sequence: number, size: number): string {
	const prefix = `camera Front Door at 192.168.1.22 sequence=${sequence} `;
	let state = sequence || 1;
	let payload = '';
	while (prefix.length + payload.length < size) {
		state = (state * 1_664_525 + 1_013_904_223) >>> 0;
		payload += state.toString(16).padStart(8, '0');
	}
	return `${prefix}${payload}`.slice(0, size);
}

function serverEntry(sequence: number, messageBytes: number): ServerLogEntry {
	return {
		sequence,
		timestamp_ms: 1_777_000_000_000 + sequence,
		level: sequence % 17 === 0 ? 'warn' : 'info',
		target: sequence % 3 === 0 ? 'keeppeek::storage' : 'keeppeek::camera',
		message: message(sequence, messageBytes),
		fields: {
			camera_id: 'front-door-private',
			path: `/Users/operator/recordings/${sequence}.mp4`,
			queue_depth: sequence % 512
		},
		file: '/Users/operator/Code/keeppeek/src/server.rs',
		line: 1_000 + (sequence % 500)
	};
}

function browserEntry(sequence: number, messageBytes: number): BrowserLogEntry {
	return {
		...serverEntry(sequence, messageBytes),
		target: 'browser.console.info',
		source: 'console'
	};
}

function fixture(serverEntries: number, browserEntries: number, messageBytes: number) {
	const cameras = [
		{
			id: 'front-door-private',
			ip: '192.168.1.22',
			name: 'Front Door',
			serial_number: 'private-serial',
			hardware_id: 'private-hardware',
			hostname: 'camera-private.local',
			mac_address: '00:11:22:33:44:55',
			profiles: []
		}
	] as unknown as CameraListItem[];
	return {
		server: {
			entries: Array.from({ length: serverEntries }, (_, index) =>
				serverEntry(index + 1, messageBytes)
			),
			oldest_sequence: 1,
			newest_sequence: serverEntries,
			truncated: false,
			stats: {
				entry_count: serverEntries,
				byte_count: serverEntries * messageBytes,
				evicted_entries: 0,
				max_entries: 10_000,
				max_bytes: 8_388_608,
				active_streams: 0,
				max_streams: 8
			}
		},
		browser: Array.from({ length: browserEntries }, (_, index) =>
			browserEntry(index + 1, Math.min(messageBytes, 256))
		),
		logging: {
			active_filter: 'info,keeppeek=debug',
			default_filter: 'info,keeppeek=debug',
			filter_error: null,
			version: 'benchmark',
			buffer: {
				entry_count: serverEntries,
				byte_count: serverEntries * messageBytes,
				evicted_entries: 0,
				max_entries: 10_000,
				max_bytes: 8_388_608,
				active_streams: 0,
				max_streams: 8
			}
		},
		health: {
			status: 'healthy',
			generated_at_ms: 1_777_000_000_000,
			uptime_seconds: 86_400,
			version: 'benchmark',
			totals: {},
			system: {
				host_name: 'keeppeek-private.local',
				process: {
					executable: '/Users/operator/bin/keeppeek',
					working_directory: '/Users/operator/keeppeek'
				},
				disks: [{ name: 'Private recordings', mount_point: '/Volumes/Private' }],
				networks: [{ name: 'Private LAN' }]
			},
			storage: {
				medium_term_path: '/Volumes/Private/medium',
				long_term_path: '/Volumes/Private/long'
			},
			webrtc: {},
			cameras: [
				{
					id: 'front-door-private',
					ip: '192.168.1.22',
					name: 'Front Door',
					configured_profiles: [],
					streams: []
				}
			],
			issues: []
		},
		config: {
			host: 'keeppeek-private.local',
			port: 8081,
			storage: {
				medium_term_path: '/Volumes/Private/medium',
				long_term_path: '/Volumes/Private/long',
				recording_catalog_path: '/Users/operator/recordings.db',
				event_thumbnail_path: '/Users/operator/event-thumbnails'
			},
			camera_count: 1,
			recording_estimate: {}
		},
		cameras,
		metrics: 'keeppeek_camera_info{camera="front-door-private",ip="192.168.1.22"} 1\n',
		generatedAt: new Date('2026-08-25T12:00:00.000Z'),
		client: { origin: 'https://keeppeek-private.local', user_agent: 'Benchmark' }
	} as unknown as DiagnosticsBundleInput;
}

function percentile(values: number[], quantile: number): number {
	const ordered = values.toSorted((left, right) => left - right);
	return ordered[Math.max(0, Math.ceil(ordered.length * quantile) - 1)]!;
}

async function sample(input: DiagnosticsBundleInput): Promise<Sample> {
	const startedAt = performance.now();
	const document = buildDiagnosticsBundle(input);
	const assembledAt = performance.now();
	const compressed = await compressDiagnosticsDocument(document);
	const completedAt = performance.now();
	return {
		assemblyMs: assembledAt - startedAt,
		compressionMs: completedAt - assembledAt,
		totalMs: completedAt - startedAt,
		inputBytes: new TextEncoder().encode(document).byteLength,
		compressedBytes: compressed.size
	};
}

const serverEntries = positiveInteger('server-entries', 10_000);
const browserEntries = positiveInteger('browser-entries', 2_000);
const messageBytes = positiveInteger('message-bytes', 512);
const warmups = positiveInteger('warmups', 3);
const runs = positiveInteger('runs', 15);
const p95BudgetMs = positiveInteger('p95-budget-ms', 1_500);
const input = fixture(serverEntries, browserEntries, messageBytes);

for (let index = 0; index < warmups; index += 1) await sample(input);
const samples: Sample[] = [];
for (let index = 0; index < runs; index += 1) samples.push(await sample(input));

const last = samples.at(-1)!;
const result = {
	environment: {
		platform: process.platform,
		architecture: process.arch,
		bun: Bun.version
	},
	workload: { serverEntries, browserEntries, messageBytes, warmups, runs },
	assembly_ms: {
		p50: Number(
			percentile(
				samples.map((entry) => entry.assemblyMs),
				0.5
			).toFixed(2)
		),
		p95: Number(
			percentile(
				samples.map((entry) => entry.assemblyMs),
				0.95
			).toFixed(2)
		)
	},
	compression_ms: {
		p50: Number(
			percentile(
				samples.map((entry) => entry.compressionMs),
				0.5
			).toFixed(2)
		),
		p95: Number(
			percentile(
				samples.map((entry) => entry.compressionMs),
				0.95
			).toFixed(2)
		)
	},
	total_ms: {
		p50: Number(
			percentile(
				samples.map((entry) => entry.totalMs),
				0.5
			).toFixed(2)
		),
		p95: Number(
			percentile(
				samples.map((entry) => entry.totalMs),
				0.95
			).toFixed(2)
		),
		budget_p95: p95BudgetMs
	},
	bytes: {
		uncompressed: last.inputBytes,
		compressed: last.compressedBytes,
		compression_ratio: Number((last.inputBytes / last.compressedBytes).toFixed(2))
	}
};

console.log(JSON.stringify(result, null, 2));
if (result.total_ms.p95 > p95BudgetMs) {
	throw new Error(`diagnostics package p95 ${result.total_ms.p95}ms exceeds ${p95BudgetMs}ms`);
}
