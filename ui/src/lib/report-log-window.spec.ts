import { expect, it } from 'vitest';
import { BrowserLogStore } from './browser-logs';
import { reportLogWindow, type ReportLogCheckpoint } from './report-log-window';
import type { LogSnapshot, ServerLogEntry } from './types';

function checkpoint(sequence: number, generation?: string): ReportLogCheckpoint {
	return { utcMs: sequence, browserSequence: sequence, server: { sequence, generation } };
}

function snapshot(...sequences: number[]): LogSnapshot {
	const entries: ServerLogEntry[] = sequences.map((sequence) => ({
		sequence,
		timestamp_ms: sequence,
		level: 'info',
		target: 'test',
		message: 'fixture',
		fields: {}
	}));
	return {
		entries,
		oldest_sequence: Math.min(...sequences),
		newest_sequence: Math.max(...sequences),
		truncated: false,
		stats: {
			entry_count: entries.length,
			byte_count: 0,
			evicted_entries: 0,
			max_entries: 10_000,
			max_bytes: 8_388_608,
			active_streams: 0,
			max_streams: 3
		}
	};
}

it('reports exclusive start and inclusive stop sorted by each source sequence', () => {
	const store = new BrowserLogStore({ storage: null });
	const entries = [1, 2, 3, 4, 5].map(() => store.append('info', 'test', []));
	const result = reportLogWindow(
		checkpoint(2, 'one'),
		checkpoint(4, 'one'),
		[entries[3], entries[1], entries[4], entries[2], entries[3]],
		snapshot(4, 2, 3, 4, 5),
		false
	);
	expect(result.browserEntries.map((entry) => entry.sequence)).toEqual([3, 4]);
	expect(result.serverEntries.map((entry) => entry.sequence)).toEqual([3, 4]);
	expect(result.manifest.browserCursors).toEqual({ start: 2, stop: 4 });
	expect(result.manifest.warnings).toEqual([]);
});

it('retains cursor evidence after losing entries and reports streamed or filtered evidence gaps', () => {
	const store = new BrowserLogStore({ storage: null, maxEntries: 1 });
	store.append('info', 'test', []);
	store.append('info', 'test', []);
	expect(store.latestSequence).toBe(2);
	expect(store.snapshot()).toHaveLength(1);
	store.clear();
	expect(store.latestSequence).toBe(2);
	const start = checkpoint(0, 'one');
	const stop = checkpoint(2, 'one');
	if (stop.server) stop.server.filter = 'info';
	const result = reportLogWindow(start, stop, store.snapshot(), snapshot(2), true);
	expect(result.manifest.warnings).toEqual([
		'browser-log-window-incomplete',
		'server-log-stream-gap',
		'server-log-filter-active',
		'server-log-window-incomplete'
	]);
	expect(result.manifest.serverFilters.stop).toBe('info');
});

it('excludes reused cursors after restart or without an authenticated generation', () => {
	const start = checkpoint(2, 'one');
	const newServer = reportLogWindow(start, checkpoint(5, 'two'), [], snapshot(3, 4, 5), false);
	expect(newServer.serverEntries).toEqual([]);
	expect(newServer.manifest.warnings).toContain('server-log-generation-changed');
	const legacy = reportLogWindow(start, checkpoint(5), [], snapshot(3, 4, 5), false);
	expect(legacy.serverEntries).toEqual([]);
	expect(legacy.manifest.warnings).toContain('server-log-generation-unverified');
	const failed = reportLogWindow(start, { ...checkpoint(5), server: null }, [], null, false);
	expect(failed.serverEntries).toEqual([]);
	expect(failed.manifest.warnings).toContain('server-logs-unavailable');
});
