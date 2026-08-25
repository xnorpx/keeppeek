import { fetchLogSnapshot, fetchMetricsSnapshot } from './api';
import { createDiagnosticRedactor, type DiagnosticPrivateValue } from './diagnostic-redaction';
import type { ControlClient } from './control-client';
import type {
	BrowserLogEntry,
	CameraListItem,
	LoggingSettings,
	LogSnapshot,
	SanitizedConfig,
	ServerHealthResponse
} from './types';

export interface DiagnosticsBundleInput {
	server: LogSnapshot;
	browser: BrowserLogEntry[];
	logging?: LoggingSettings;
	health: ServerHealthResponse;
	config: SanitizedConfig;
	cameras: CameraListItem[];
	metrics?: string;
	collectionErrors?: Record<string, string>;
	generatedAt?: Date;
	client?: Record<string, unknown>;
}

type DiagnosticsControlClient = Pick<
	ControlClient,
	'getLoggingSettings' | 'getHealth' | 'getRuntimeConfiguration' | 'getCameras'
>;

export async function collectDiagnosticsBundle(
	controlClient: DiagnosticsControlClient,
	browser: BrowserLogEntry[],
	generatedAt = new Date()
): Promise<string> {
	const server = await fetchLogSnapshot();
	const [loggingResult, healthResult, configResult, camerasResult, metricsResult] =
		await Promise.allSettled([
			controlClient.getLoggingSettings(),
			controlClient.getHealth(),
			controlClient.getRuntimeConfiguration(),
			controlClient.getCameras(),
			fetchMetricsSnapshot()
		]);
	const collectionErrors: Record<string, string> = {};
	const logging = settledValue(loggingResult, 'logging-settings', collectionErrors);
	const health = settledValue(healthResult, 'health', collectionErrors);
	const config = settledValue(configResult, 'runtime-config', collectionErrors);
	const cameras = settledValue(camerasResult, 'cameras', collectionErrors);
	const metrics = settledValue(metricsResult, 'metrics', collectionErrors);
	if (!health) throw new Error('Health evidence is required to scrub the diagnostics package.');
	if (!config) {
		throw new Error('Runtime configuration is required to scrub the diagnostics package.');
	}
	if (!cameras) throw new Error('Camera inventory is required to scrub the diagnostics package.');

	return buildDiagnosticsBundle({
		server,
		browser,
		logging,
		health,
		config,
		cameras,
		metrics,
		collectionErrors,
		generatedAt,
		client: browserEvidence()
	});
}

export function buildDiagnosticsBundle(input: DiagnosticsBundleInput): string {
	if (!input.health) {
		throw new Error('Health evidence is required to scrub the diagnostics package.');
	}
	if (!input.config) {
		throw new Error('Runtime configuration is required to scrub the diagnostics package.');
	}
	if (!input.cameras) {
		throw new Error('Camera inventory is required to scrub the diagnostics package.');
	}
	const generatedAt = input.generatedAt ?? new Date();
	const redactor = createDiagnosticRedactor(privateValues(input));
	const artifacts = [
		'server_logs',
		'browser_logs',
		'log_buffer',
		...(input.logging ? ['logging_settings'] : []),
		...(input.health ? ['health'] : []),
		...(input.config ? ['runtime_config'] : []),
		...(input.cameras ? ['cameras'] : []),
		...(input.metrics ? ['metrics'] : []),
		...(input.client ? ['browser_environment'] : []),
		...(input.collectionErrors && Object.keys(input.collectionErrors).length > 0
			? ['collection_errors']
			: [])
	];
	const document = {
		manifest: {
			format: 'keeppeek-diagnostics',
			format_version: 1,
			generated_at: generatedAt.toISOString(),
			privacy: 'scrubbed',
			redaction_context: 'complete',
			server_log_entries: input.server.entries.length,
			browser_log_entries: input.browser.length,
			server_snapshot_truncated: input.server.truncated,
			artifacts,
			notice:
				'Known private values and recognized sensitive patterns were scrubbed before compression. Review before sharing because no automated scrubber can guarantee removal of every free-form private value.'
		},
		server_logs: input.server.entries,
		browser_logs: input.browser,
		log_buffer: {
			oldest_sequence: input.server.oldest_sequence,
			newest_sequence: input.server.newest_sequence,
			truncated: input.server.truncated,
			stats: input.server.stats
		},
		logging_settings: input.logging,
		health: input.health,
		runtime_config: input.config,
		cameras: input.cameras,
		metrics: input.metrics ? redactor.text(input.metrics) : undefined,
		browser_environment: input.client,
		collection_errors: input.collectionErrors
	};

	return `${JSON.stringify(redactor.value(document), null, 2)}\n`;
}

export function diagnosticsBundleFilename(generatedAt = new Date()): string {
	return `keeppeek-diagnostics-${generatedAt.toISOString().replace(/[:.]/g, '-')}.json.gz`;
}

export async function downloadDiagnosticsBundle(
	controlClient: DiagnosticsControlClient,
	browser: BrowserLogEntry[]
): Promise<void> {
	const generatedAt = new Date();
	const diagnosticsDocument = await collectDiagnosticsBundle(controlClient, browser, generatedAt);
	const blob = await compressDiagnosticsDocument(diagnosticsDocument);
	const url = URL.createObjectURL(blob);
	const anchor = document.createElement('a');
	anchor.href = url;
	anchor.download = diagnosticsBundleFilename(generatedAt);
	anchor.click();
	URL.revokeObjectURL(url);
}

export async function compressDiagnosticsDocument(document: string): Promise<Blob> {
	if (typeof CompressionStream === 'undefined') {
		throw new Error('This browser cannot create a compressed diagnostics package.');
	}
	const stream = new Blob([document]).stream().pipeThrough(new CompressionStream('gzip'));
	return new Blob([await new Response(stream).arrayBuffer()], { type: 'application/gzip' });
}

function settledValue<T>(
	result: PromiseSettledResult<T>,
	name: string,
	errors: Record<string, string>
): T | undefined {
	if (result.status === 'fulfilled') return result.value;
	errors[name] = result.reason instanceof Error ? result.reason.message : String(result.reason);
	return undefined;
}

function browserEvidence(): Record<string, unknown> {
	if (typeof window === 'undefined' || typeof navigator === 'undefined') return {};
	return {
		user_agent: navigator.userAgent,
		language: navigator.language,
		hardware_concurrency: navigator.hardwareConcurrency,
		viewport: {
			width: window.innerWidth,
			height: window.innerHeight,
			device_pixel_ratio: window.devicePixelRatio
		},
		origin: window.location.origin
	};
}

function privateValues(input: DiagnosticsBundleInput): DiagnosticPrivateValue[] {
	const values: DiagnosticPrivateValue[] = [];
	const add = (value: string | null | undefined, replacement: string) => {
		values.push({ value, replacement });
	};
	input.health?.cameras.forEach((camera, index) => {
		const alias = `camera-${String(index + 1).padStart(3, '0')}`;
		add(camera.id, alias);
		add(camera.name, alias);
		add(camera.ip, '[REDACTED_IP]');
	});
	input.cameras?.forEach((camera, index) => {
		const alias = `camera-${String(index + 1).padStart(3, '0')}`;
		add(camera.id, alias);
		add(camera.name, alias);
		add(camera.ip, '[REDACTED_IP]');
		add(camera.serial_number, '[REDACTED_IDENTIFIER]');
		add(camera.hardware_id, '[REDACTED_IDENTIFIER]');
		add(camera.hostname, '[REDACTED_HOST]');
		add(camera.mac_address, '[REDACTED_MAC]');
	});
	add(input.health?.system.host_name, '[REDACTED_HOST]');
	add(input.health?.system.process.executable, '[REDACTED_PATH]');
	add(input.health?.system.process.working_directory, '[REDACTED_PATH]');
	for (const disk of input.health?.system.disks ?? []) {
		add(disk.name, '[REDACTED_DISK]');
		add(disk.mount_point, '[REDACTED_PATH]');
	}
	for (const network of input.health?.system.networks ?? []) {
		add(network.name, '[REDACTED_NETWORK]');
	}
	add(input.health?.storage.medium_term_path, '[REDACTED_PATH]');
	add(input.health?.storage.long_term_path, '[REDACTED_PATH]');
	add(input.config?.host, '[REDACTED_HOST]');
	add(input.config?.storage.medium_term_path, '[REDACTED_PATH]');
	add(input.config?.storage.long_term_path, '[REDACTED_PATH]');
	add(input.config?.storage.recording_catalog_path, '[REDACTED_PATH]');
	add(input.config?.storage.event_thumbnail_path, '[REDACTED_PATH]');
	return values;
}
