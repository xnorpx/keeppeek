import type { BrowserLogEntry, LogSnapshot, ServerLogEntry } from './types';

type ServerCheckpoint = {
	sequence: number;
	generation?: string;
	filter?: string;
};

export type ReportLogCheckpoint = {
	utcMs: number;
	browserSequence: number;
	server: ServerCheckpoint | null;
};

type Warning =
	| 'browser-log-window-incomplete'
	| 'server-log-window-incomplete'
	| 'server-log-filter-active'
	| 'server-log-stream-gap'
	| 'server-log-generation-changed'
	| 'server-log-generation-unverified'
	| 'server-logs-unavailable';

function selectWindow<Entry extends ServerLogEntry>(
	entries: readonly Entry[],
	start: number,
	stop: number
): { entries: Entry[]; missing: boolean } {
	const unique = new Map<number, Entry>();
	for (const entry of entries) {
		if (entry.sequence > start && entry.sequence <= stop && !unique.has(entry.sequence)) {
			unique.set(entry.sequence, entry);
		}
	}
	return {
		entries: [...unique.values()].sort((left, right) => left.sequence - right.sequence),
		missing: stop < start || unique.size !== stop - start
	};
}

export function reportLogWindow(
	start: ReportLogCheckpoint,
	stop: ReportLogCheckpoint,
	browserEntries: readonly BrowserLogEntry[],
	serverSnapshot: LogSnapshot | null,
	streamGap: boolean
) {
	const warnings: Warning[] = [];
	const browser = selectWindow(browserEntries, start.browserSequence, stop.browserSequence);
	if (browser.missing) warnings.push('browser-log-window-incomplete');
	if (streamGap) warnings.push('server-log-stream-gap');
	if (start.server?.filter || stop.server?.filter) warnings.push('server-log-filter-active');
	let serverEntries: ServerLogEntry[] = [];
	if (!start.server || !stop.server || !serverSnapshot) {
		warnings.push('server-logs-unavailable');
	} else if (!start.server.generation || !stop.server.generation) {
		// A restarted recorder can reuse every sequence in a previously captured range.
		warnings.push('server-log-generation-unverified');
	} else if (
		start.server.generation !== stop.server.generation ||
		stop.server.sequence < start.server.sequence
	) {
		warnings.push('server-log-generation-changed');
	} else {
		const server = selectWindow(
			serverSnapshot.entries,
			start.server.sequence,
			stop.server.sequence
		);
		serverEntries = server.entries;
		if (server.missing) warnings.push('server-log-window-incomplete');
	}
	return {
		browserEntries: browser.entries,
		serverEntries,
		manifest: {
			version: 1 as const,
			startUtc: new Date(start.utcMs).toISOString(),
			stopUtc: new Date(stop.utcMs).toISOString(),
			browserCursors: { start: start.browserSequence, stop: stop.browserSequence },
			serverCursors: { start: start.server?.sequence ?? null, stop: stop.server?.sequence ?? null },
			serverFilters: { start: start.server?.filter ?? null, stop: stop.server?.filter ?? null },
			warnings
		}
	};
}

export type ReportLogWindow = ReturnType<typeof reportLogWindow>;
