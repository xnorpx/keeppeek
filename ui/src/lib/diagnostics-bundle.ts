import { boundedReportEvidence } from './report-evidence';
import { createDiagnosticRedactor, type DiagnosticPrivateValue } from './diagnostic-redaction';
import type { ControlClient } from './control-client';
import type { ReportLogWindow } from './report-log-window';
import type {
	BrowserLogEntry,
	CameraListItem,
	LoggingSettings,
	LogSnapshot,
	SanitizedConfig,
	ServerHealthResponse
} from './types';

export interface DiagnosticsBundleInput {
	report?: ReportLogWindow;
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
	| 'getLogSnapshot'
	| 'getMetricsSnapshot'
	| 'getLoggingSettings'
	| 'getHealth'
	| 'getRuntimeConfiguration'
	| 'getCameras'
>;

export async function collectDiagnosticsBundle(
	controlClient: DiagnosticsControlClient,
	browser: BrowserLogEntry[],
	generatedAt = new Date()
): Promise<string> {
	return (
		await collectEvidence(controlClient, await controlClient.getLogSnapshot(), browser, generatedAt)
	).document;
}

export type ReportDiagnostics = {
	diagnostics: Blob | null;
	redactText: ((value: string) => string) | null;
	warnings: string[];
};

export async function collectReportDiagnosticsBundle(
	controlClient: DiagnosticsControlClient,
	server: LogSnapshot | null,
	report: ReportLogWindow,
	signal: AbortSignal
): Promise<ReportDiagnostics> {
	// The report manifest accounts for missing server logs. The existing health,
	// configuration, and inventory checks still apply to every downloadable package.
	const buffer = server ?? {
		entries: [],
		oldest_sequence: null,
		newest_sequence: null,
		truncated: true,
		stats: {
			entry_count: 0,
			byte_count: 0,
			evicted_entries: 0,
			max_entries: 0,
			max_bytes: 0,
			active_streams: 0,
			max_streams: 0
		}
	};
	const collectionErrors: Record<string, string> = {};
	let diagnostics: Blob | null = null;
	let redactText: ReportDiagnostics['redactText'] = null;
	try {
		const evidence = await collectEvidence(
			controlClient,
			buffer,
			report.browserEntries,
			new Date(report.manifest.stopUtc),
			report,
			signal,
			collectionErrors
		);
		signal.throwIfAborted();
		diagnostics = await compressDiagnosticsDocument(evidence.document, signal);
		redactText = evidence.redactText;
	} catch {
		signal.throwIfAborted();
	}
	const warnings = Object.keys(collectionErrors).map(
		(source) => `diagnostics-${source}-unavailable`
	);
	if (!diagnostics) warnings.push('diagnostics-package-unavailable');
	return { diagnostics, redactText, warnings };
}

async function collectEvidence(
	controlClient: DiagnosticsControlClient,
	server: LogSnapshot,
	browser: BrowserLogEntry[],
	generatedAt: Date,
	report?: ReportLogWindow,
	signal?: AbortSignal,
	collectionErrors: Record<string, string> = {}
) {
	const collect = <T>(operation: (signal?: AbortSignal) => Promise<T>) =>
		signal ? boundedReportEvidence(signal, operation) : operation();
	const [loggingResult, healthResult, configResult, camerasResult, metricsResult] =
		await Promise.allSettled([
			collect((active) => controlClient.getLoggingSettings(active)),
			collect((active) => controlClient.getHealth(active)),
			collect((active) => controlClient.getRuntimeConfiguration(active)),
			collect((active) => controlClient.getCameras(active)),
			collect((active) => controlClient.getMetricsSnapshot(active))
		]);
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

	return buildEvidence({
		report,
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
	return buildEvidence(input).document;
}

function buildEvidence(input: DiagnosticsBundleInput) {
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
	const serverEntries = input.report?.serverEntries ?? input.server.entries;
	const browserEntries = input.report?.browserEntries ?? input.browser;
	const artifacts = [
		'server_logs',
		'browser_logs',
		'log_buffer',
		...(input.report ? ['report_window'] : []),
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
			server_log_entries: serverEntries.length,
			browser_log_entries: browserEntries.length,
			server_snapshot_truncated: input.server.truncated,
			artifacts,
			notice:
				'Known private values and recognized sensitive patterns were scrubbed before compression. Review before sharing because no automated scrubber can guarantee removal of every free-form private value.'
		},
		...(input.report ? { report_window: input.report.manifest } : {}),
		server_logs: serverEntries,
		browser_logs: browserEntries,
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

	return {
		document: `${JSON.stringify(redactor.value(document), null, 2)}\n`,
		redactText: redactor.text
	};
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
	downloadDiagnosticsArtifact(blob, diagnosticsBundleFilename(generatedAt));
}

export function downloadDiagnosticsArtifact(blob: Blob, filename: string): void {
	const url = URL.createObjectURL(blob);
	const anchor = document.createElement('a');
	anchor.href = url;
	anchor.download = filename;
	try {
		anchor.click();
	} finally {
		URL.revokeObjectURL(url);
	}
}

export async function compressDiagnosticsDocument(
	document: string,
	signal?: AbortSignal
): Promise<Blob> {
	if (typeof CompressionStream === 'undefined') {
		throw new Error('This browser cannot create a compressed diagnostics package.');
	}
	const stream = new Blob([document])
		.stream()
		.pipeThrough(new CompressionStream('gzip'), { signal });
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
