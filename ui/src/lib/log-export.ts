import type { BrowserLogEntry, LoggingSettings, LogSnapshot, ServerLogEntry } from './types';

export interface BugReportInput {
	settings: LoggingSettings;
	server: LogSnapshot;
	browser: BrowserLogEntry[];
	viewerFilters: Record<string, unknown>;
	generatedAt?: Date;
	userAgent?: string;
	origin?: string;
}

export function buildBugReportJsonl(input: BugReportInput): string {
	const generatedAt = input.generatedAt ?? new Date();
	const records: unknown[] = [
		{
			type: 'metadata',
			format: 'keeppeek-bug-report',
			format_version: 1,
			generated_at: generatedAt.toISOString(),
			keeppeek_version: input.settings.version,
			active_filter: input.settings.active_filter,
			server_buffer: input.server.stats,
			server_snapshot_truncated: input.server.truncated,
			browser_entries: input.browser.length,
			viewer_filters: input.viewerFilters,
			user_agent: input.userAgent ?? browserUserAgent(),
			origin: input.origin ?? browserOrigin()
		},
		...input.server.entries.map((entry) => ({ type: 'server_log', ...entry })),
		...input.browser.map((entry) => ({ type: 'browser_log', ...entry }))
	];
	return `${records.map((record) => safeJsonLine(record)).join('\n')}\n`;
}

export function bugReportFilename(generatedAt = new Date()): string {
	return `keeppeek-bug-report-${generatedAt.toISOString().replace(/[:.]/g, '-')}.jsonl`;
}

export function downloadBugReport(input: BugReportInput): void {
	const generatedAt = input.generatedAt ?? new Date();
	const blob = new Blob([buildBugReportJsonl({ ...input, generatedAt })], {
		type: 'application/x-ndjson;charset=utf-8'
	});
	const url = URL.createObjectURL(blob);
	const anchor = document.createElement('a');
	anchor.href = url;
	anchor.download = bugReportFilename(generatedAt);
	anchor.click();
	URL.revokeObjectURL(url);
}

function safeJsonLine(record: unknown): string {
	const seen = new WeakSet<object>();
	return JSON.stringify(record, (key, value) => {
		if (isSensitiveKey(key)) return '[REDACTED]';
		if (typeof value === 'string') return redactText(value);
		if (value && typeof value === 'object') {
			if (seen.has(value)) return '[Circular]';
			seen.add(value);
		}
		return value;
	});
}

function redactText(value: string): string {
	return value
		.replace(/([a-z][a-z0-9+.-]*:\/\/)[^@\s/]+@/gi, '$1[REDACTED]@')
		.replace(
			/\b(password|passwd|secret|token|authorization|api[_-]?key)\s*=\s*([^\s,;&]+)/gi,
			'$1=[REDACTED]'
		);
}

function isSensitiveKey(key: string): boolean {
	return /password|passwd|secret|token|authorization|credential|api[_-]?key|cookie/i.test(key);
}

function browserUserAgent(): string {
	return typeof navigator === 'undefined' ? 'unknown' : navigator.userAgent;
}

function browserOrigin(): string {
	return typeof window === 'undefined' ? 'unknown' : window.location.origin;
}

export type ExportedLogEntry = ServerLogEntry | BrowserLogEntry;
