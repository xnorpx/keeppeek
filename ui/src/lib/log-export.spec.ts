import { describe, expect, it } from 'vitest';
import { bugReportFilename, buildBugReportJsonl } from './log-export';
import type { BugReportInput } from './log-export';

function reportInput(): BugReportInput {
	return {
		settings: {
			active_filter: 'info,str0m=warn',
			default_filter: 'info',
			filter_error: null,
			version: '0.1.0',
			buffer: {
				entry_count: 1,
				byte_count: 100,
				evicted_entries: 0,
				max_entries: 10_000,
				max_bytes: 8_388_608,
				active_streams: 1,
				max_streams: 8
			}
		},
		server: {
			entries: [
				{
					sequence: 1,
					timestamp_ms: 1,
					level: 'info',
					target: 'keeppeek::test',
					message: 'opening rtsp://operator:camera@192.0.2.1/live',
					fields: { password: 'server-secret' }
				}
			],
			oldest_sequence: 1,
			newest_sequence: 1,
			truncated: false,
			stats: {
				entry_count: 1,
				byte_count: 100,
				evicted_entries: 0,
				max_entries: 10_000,
				max_bytes: 8_388_608,
				active_streams: 1,
				max_streams: 8
			}
		},
		browser: [
			{
				sequence: 1,
				timestamp_ms: 2,
				level: 'error',
				target: 'browser.test',
				message: 'token=browser-secret',
				fields: {},
				source: 'console'
			}
		],
		viewerFilters: { text: 'camera' },
		generatedAt: new Date('2026-08-12T12:34:56.000Z'),
		userAgent: 'KeepPeek Test',
		origin: 'http://keeppeek.test'
	};
}

describe('bug report export', () => {
	it('writes parseable metadata, server, and browser JSONL records', () => {
		const lines = buildBugReportJsonl(reportInput())
			.trim()
			.split('\n')
			.map((line) => JSON.parse(line));

		expect(lines.map((line) => line.type)).toEqual(['metadata', 'server_log', 'browser_log']);
		expect(lines[0]).toMatchObject({
			keeppeek_version: '0.1.0',
			active_filter: 'info,str0m=warn',
			browser_entries: 1
		});
		expect(lines[1].message).toBe('opening rtsp://[REDACTED]@192.0.2.1/live');
		expect(lines[1].fields.password).toBe('[REDACTED]');
		expect(lines[2].message).toBe('token=[REDACTED]');
	});

	it('uses an ISO timestamp in the download filename', () => {
		expect(bugReportFilename(new Date('2026-08-12T12:34:56.000Z'))).toBe(
			'keeppeek-bug-report-2026-08-12T12-34-56-000Z.jsonl'
		);
	});
});
