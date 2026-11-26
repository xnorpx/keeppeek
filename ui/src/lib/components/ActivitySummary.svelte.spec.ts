import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { page } from 'vitest/browser';
import ActivitySummary, { type ActivityUiControls } from './ActivitySummary.svelte';
import type { ActivitySummaryData } from '$lib/activity-analytics';

const controls: ActivityUiControls = {
	rangePreset: '7d',
	granularity: 'day',
	timezone: 'America/New_York',
	weekStart: 'monday',
	comparison: 'previous_duration',
	cameraId: '',
	eventType: '',
	origin: '',
	zone: '',
	missingZone: false,
	confidenceBand: '',
	image: 'all',
	review: 'all',
	bookmarks: 'all'
};

const summary: ActivitySummaryData = {
	queryId: 'q-ui',
	generatedAtMs: Date.parse('2026-05-10T12:00:00Z'),
	startMs: Date.parse('2026-05-09T04:00:00Z'),
	endMs: Date.parse('2026-05-10T04:00:00Z'),
	timezone: 'America/New_York',
	tzdbVersion: '2025b',
	granularity: 'day',
	weekStart: 'monday',
	analyticsGeneration: 12,
	groupPolicyRevision: 1,
	dataSource: 'rollups',
	rollupState: 'ready',
	totals: {
		eventCount: 14,
		distinctEventCount: 14,
		intervalEventCount: 10,
		openIntervalEventCount: 1,
		totalDurationMs: 250_000,
		averageDurationMs: 25_000,
		withImageCount: 7,
		withConfidenceCount: 12,
		withZoneCount: 11,
		percentWithImage: 50,
		percentWithConfidence: 85.7,
		percentWithZone: 78.6
	},
	comparison: {
		mode: 'previous_duration',
		startMs: Date.parse('2026-05-08T04:00:00Z'),
		endMs: Date.parse('2026-05-09T04:00:00Z'),
		totals: {
			eventCount: 10,
			distinctEventCount: 10,
			intervalEventCount: 8,
			openIntervalEventCount: 0,
			totalDurationMs: 160_000,
			averageDurationMs: 20_000,
			withImageCount: 5,
			withConfidenceCount: 8,
			withZoneCount: 8,
			percentWithImage: 50,
			percentWithConfidence: 80,
			percentWithZone: 80
		},
		eventCountDelta: 4,
		eventCountDeltaPercent: 40,
		completeness: 'complete',
		retentionPartial: false,
		sourceHealthGap: false
	},
	buckets: [
		{
			startMs: Date.parse('2026-05-09T04:00:00Z'),
			endMs: Date.parse('2026-05-10T04:00:00Z'),
			label: '2026-05-09',
			utcOffsetSeconds: -14_400,
			dstFold: 0,
			partialCurrent: false,
			completeness: 'partial_retention',
			retentionPartial: true,
			sourceHealthGap: true,
			measures: {
				eventCount: 14,
				distinctEventCount: 14,
				intervalEventCount: 10,
				openIntervalEventCount: 1,
				totalDurationMs: 250_000,
				averageDurationMs: 25_000,
				withImageCount: 7,
				withConfidenceCount: 12,
				withZoneCount: 11,
				percentWithImage: 50,
				percentWithConfidence: 85.7,
				percentWithZone: 78.6
			}
		}
	],
	series: [],
	seriesTruncated: false,
	heatmap: [
		{
			weekday: 6,
			localHour: 14,
			eventCount: 14,
			distinctEventCount: 14,
			completeness: 'partial_retention'
		}
	],
	byCamera: [
		{
			key: 'cam-front',
			label: 'Front Porch',
			isUnknown: false,
			measures: {
				eventCount: 14,
				distinctEventCount: 14,
				intervalEventCount: 10,
				openIntervalEventCount: 1,
				totalDurationMs: 250_000,
				averageDurationMs: 25_000,
				withImageCount: 7,
				withConfidenceCount: 12,
				withZoneCount: 11,
				percentWithImage: 50,
				percentWithConfidence: 85.7,
				percentWithZone: 78.6
			},
			sharePercent: 100
		}
	],
	byEventType: [],
	byZone: [
		{
			key: '',
			label: 'Unknown zone',
			isUnknown: true,
			measures: {
				eventCount: 3,
				distinctEventCount: 3,
				intervalEventCount: 2,
				openIntervalEventCount: 0,
				totalDurationMs: 30_000,
				averageDurationMs: 15_000,
				withImageCount: 1,
				withConfidenceCount: 1,
				withZoneCount: 0,
				percentWithImage: 33.3,
				percentWithConfidence: 33.3,
				percentWithZone: 0
			},
			sharePercent: 21.4
		}
	],
	byOrigin: [],
	byConfidenceBand: [],
	byGroup: [],
	retention: {
		partialRetention: true,
		oldestRetainedEventMs: Date.parse('2026-05-09T04:00:00Z'),
		deletedEventCountInRange: 2,
		deletedIntervals: []
	},
	sourceHealth: {
		authorizedCameras: 2,
		reportingCameras: 1,
		healthyNoReportsCameras: 0,
		missingHealthCameras: 1,
		noExpectedSourceCameras: 0,
		unknownHistoryCameras: 0,
		cameras: []
	},
	provenanceNotice:
		'Counts reflect producer-reported event metadata within current retention and authorization scope, not authoritative real-world identity, occupancy, or threat totals.',
	exact: true
};

describe('ActivitySummary component', () => {
	it('renders provenance, partial/health notices, accessible table toggle, Events links, shared filters, and exports', async () => {
		const onExport = vi.fn();
		const onRebuild = vi.fn();
		const onControlsChange = vi.fn();
		render(ActivitySummary, {
			summary,
			status: null,
			cameras: [
				{
					id: 'cam-front',
					ip: '192.0.2.10',
					name: 'Front Porch',
					manufacturer: null,
					model: null,
					firmware_version: null,
					is_reolink: false,
					group_ids: [],
					profiles: []
				}
			],
			controls,
			isAdmin: true,
			onControlsChange,
			onRefresh: vi.fn(),
			onRebuild,
			onExport
		});

		await expect
			.element(page.getByText('Counts reflect producer-reported event metadata', { exact: false }))
			.toBeVisible();
		await expect.element(page.getByText(/Partial retention:/i)).toBeVisible();
		await expect.element(page.getByText(/Event-source health gap:/i)).toBeVisible();
		await expect.element(page.getByRole('combobox', { name: 'Event type' })).toBeVisible();
		await expect.element(page.getByRole('combobox', { name: 'Zone', exact: true })).toBeVisible();
		await expect.element(page.getByRole('combobox', { name: 'Zone presence' })).toBeVisible();
		await expect.element(page.getByRole('combobox', { name: 'Image' })).toBeVisible();
		await expect.element(page.getByRole('combobox', { name: 'Review state' })).toBeVisible();
		await expect.element(page.getByRole('combobox', { name: 'Bookmarks' })).toBeVisible();

		await page.getByRole('combobox', { name: 'Image' }).selectOptions('with');
		expect(onControlsChange).toHaveBeenCalledWith(expect.objectContaining({ image: 'with' }));

		await page.getByRole('button', { name: 'Table', exact: true }).click();
		await expect.element(page.getByText('Open matching events')).toBeVisible();

		await page.getByRole('button', { name: 'Export CSV' }).click();
		expect(onExport).toHaveBeenCalledTimes(1);
		expect(onExport.mock.calls[0]?.[0]).toBe('csv');
		expect(onExport.mock.calls[0]?.[2]).toContain('# timezone,America/New_York');

		await page.getByRole('button', { name: 'Rebuild rollups' }).click();
		expect(onRebuild).toHaveBeenCalledTimes(1);
	});
});
