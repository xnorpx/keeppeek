<script lang="ts">
	import type {
		ActivityAnalyticsStatusData,
		ActivityBreakdownRow,
		ActivityHeatmapCell,
		ActivityMeasures,
		ActivitySummaryData,
		ActivityTimeBucket
	} from '$lib/activity-analytics';
	import { setCapabilityState } from '$lib/capability-context';
	import ActivitySummary, { type ActivityUiControls } from '$lib/components/ActivitySummary.svelte';
	import DesktopPaperRail from '$lib/components/DesktopPaperRail.svelte';
	import EventDetailDrawer from '$lib/components/EventDetailDrawer.svelte';
	import EventExpressionBuilder from '$lib/components/EventExpressionBuilder.svelte';
	import EventResultCard from '$lib/components/EventResultCard.svelte';
	import type { EventPlannerDiagnostics } from '$lib/control-client';
	import type { EventBrowserRecord } from '$lib/event-browser';
	import type { EventExpressionNode } from '$lib/event-expression';
	import {
		eventWorkflowKey,
		type EventBookmarkChange,
		type EventReviewChange,
		type EventWorkflowIdentity,
		type EventWorkflowResult,
		type EventWorkflowState,
		type EventWorkflowTarget
	} from '$lib/event-workflow';
	import { EventWorkflow } from '$lib/event-workflow.svelte';
	import type { CameraListItem, RecordingEvent } from '$lib/types';
	import BarChart3Icon from '@lucide/svelte/icons/bar-chart-3';
	import CheckIcon from '@lucide/svelte/icons/check';
	import SearchIcon from '@lucide/svelte/icons/search';
	import SlidersHorizontalIcon from '@lucide/svelte/icons/sliders-horizontal';

	setCapabilityState([
		'keeppeek.event-search.advanced.v1',
		'keeppeek.event-workflow.v1',
		'keeppeek.activity-analytics.v1',
		'keeppeek.media-export.v1',
		'keeppeek.bookmarks.v1'
	]);

	const dayStartMs = Date.parse('2026-08-18T00:00:00Z');
	const at = (time: string) => Date.parse(`2026-08-18T${time}Z`);

	function makeSceneDataUri(
		title: string,
		timeUtc: string,
		accent: string,
		subject: 'person' | 'vehicle' | 'story'
	): string {
		const figure =
			subject === 'person'
				? `<circle cx="294" cy="112" r="24" fill="${accent}"/><rect x="266" y="142" width="56" height="114" rx="14" fill="${accent}" fill-opacity="0.88"/><rect x="234" y="210" width="38" height="28" rx="3" fill="#c08d56"/>`
				: subject === 'vehicle'
					? `<rect x="210" y="156" width="236" height="104" rx="18" fill="${accent}" fill-opacity="0.85"/><rect x="238" y="174" width="180" height="40" rx="6" fill="#0f172a"/><rect x="292" y="226" width="72" height="20" rx="3" fill="#f8fafc"/>`
					: `<rect x="176" y="106" width="120" height="128" rx="8" fill="${accent}" fill-opacity="0.3" stroke="${accent}" stroke-width="3"/><rect x="332" y="128" width="132" height="96" rx="8" fill="#38bdf8" fill-opacity="0.25" stroke="#38bdf8" stroke-width="3"/>`;
		const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="640" height="360" viewBox="0 0 640 360">
			<rect width="640" height="215" fill="#162133"/>
			<rect y="215" width="640" height="145" fill="#1d2430"/>
			<polygon points="150,215 510,215 620,360 35,360" fill="#273142"/>
			<rect x="44" y="68" width="184" height="148" rx="4" fill="#293446" stroke="#475569" stroke-width="2"/>
			<rect x="116" y="114" width="54" height="102" rx="2" fill="#3b2f2a" stroke="#64748b"/>
			${figure}
			<rect x="18" y="16" width="248" height="34" rx="4" fill="#090c12" fill-opacity="0.78"/>
			<text x="28" y="38" fill="#f8fafc" font-family="monospace" font-size="15">${title} · ${timeUtc} UTC</text>
		</svg>`;
		return `data:image/svg+xml;utf8,${encodeURIComponent(svg)}`;
	}

	const cameras: CameraListItem[] = [
		{
			id: 'front-door',
			ip: '192.0.2.41',
			name: 'Front Door',
			manufacturer: 'Reolink',
			model: 'RLC-811A',
			firmware_version: null,
			is_reolink: true,
			group_ids: ['perimeter'],
			profiles: []
		},
		{
			id: 'driveway',
			ip: '192.0.2.42',
			name: 'Driveway',
			manufacturer: 'Reolink',
			model: 'RLC-823A',
			firmware_version: null,
			is_reolink: true,
			group_ids: ['perimeter'],
			profiles: []
		},
		{
			id: 'side-gate',
			ip: '192.0.2.43',
			name: 'Side Gate',
			manufacturer: 'ONVIF',
			model: 'IPC-4K',
			firmware_version: null,
			is_reolink: false,
			group_ids: ['perimeter'],
			profiles: []
		}
	];

	const workflowIdentity: EventWorkflowIdentity = {
		actorId: 'operator@local',
		localWorkspaceId: '00000000-0000-4000-8000-000000000001',
		administrator: true
	};

	const initialWorkflowStates = new Map<string, EventWorkflowState>();
	function makeWorkflowState(sourceId: string, eventId: string): EventWorkflowState {
		const state: EventWorkflowState = {
			sourceId,
			eventId,
			reviewed: false,
			dismissed: false,
			reviewRevision: '1',
			reviewedAtMs: null,
			dismissedAtMs: null,
			updatedAtMs: at('06:37:23'),
			bookmark: null,
			eventPresent: true,
			mediaAvailable: true,
			sourceAvailable: true
		};
		initialWorkflowStates.set(eventWorkflowKey(state), state);
		return state;
	}

	const allRecords: EventBrowserRecord[] = [
		{
			camera: cameras[0]!,
			event: {
				id: 'evt-person-porch',
				source_id: 'front-door',
				revision: 2,
				source: 'camera',
				kind: 'person',
				start_time_ms: at('06:37:23'),
				end_time_ms: at('06:37:41'),
				confidence: 0.94,
				bbox: [0.35, 0.22, 0.22, 0.56],
				bbox_attachment_id: 'att-porch-1',
				canonical_attachment_id: 'att-porch-1',
				zone: 'porch',
				thumbnail_url: makeSceneDataUri('FRONT DOOR', '06:37:23', '#3b82f6', 'person'),
				attachments: [
					{
						id: 'att-porch-1',
						type: 'snapshot',
						content_type: 'image/png',
						byte_length: 48_200,
						ordinal: 0,
						timestamp_ms: at('06:37:23')
					}
				],
				workflow: makeWorkflowState('front-door', 'evt-person-porch')
			}
		},
		{
			camera: cameras[2]!,
			event: {
				id: 'evt-person-gate',
				source_id: 'side-gate',
				revision: 3,
				source: 'keeppeek',
				kind: 'person',
				start_time_ms: at('06:24:11'),
				end_time_ms: at('06:24:29'),
				confidence: 0.89,
				bbox: [0.36, 0.24, 0.22, 0.52],
				bbox_attachment_id: 'att-gate-1',
				canonical_attachment_id: 'att-gate-1',
				zone: 'walkway',
				thumbnail_url: makeSceneDataUri('SIDE GATE', '06:24:11', '#60a5fa', 'person'),
				attachments: [
					{
						id: 'att-gate-1',
						type: 'snapshot',
						content_type: 'image/png',
						byte_length: 44_100,
						ordinal: 0,
						timestamp_ms: at('06:24:11')
					}
				],
				workflow: makeWorkflowState('side-gate', 'evt-person-gate')
			}
		},
		{
			camera: cameras[1]!,
			event: {
				id: 'evt-vehicle-van',
				source_id: 'driveway',
				revision: 1,
				source: 'camera',
				kind: 'vehicle',
				start_time_ms: at('06:12:04'),
				end_time_ms: at('06:12:38'),
				confidence: 0.86,
				bbox: null,
				zone: 'driveway',
				thumbnail_url: makeSceneDataUri('DRIVEWAY', '06:12:04', '#22c55e', 'vehicle'),
				workflow: makeWorkflowState('driveway', 'evt-vehicle-van')
			}
		},
		{
			camera: cameras[0]!,
			event: {
				id: 'evt-person-low',
				source_id: 'front-door',
				revision: 1,
				source: 'camera',
				kind: 'person',
				start_time_ms: at('05:54:18'),
				end_time_ms: at('05:54:25'),
				confidence: 0.52,
				bbox: null,
				zone: 'street',
				thumbnail_url: makeSceneDataUri('FRONT DOOR', '05:54:18', '#94a3b8', 'person'),
				workflow: makeWorkflowState('front-door', 'evt-person-low')
			}
		},
		{
			camera: cameras[0]!,
			event: {
				id: 'evt-story-morning',
				source_id: 'front-door',
				revision: 1,
				source: 'keeppeek',
				kind: 'story',
				start_time_ms: at('05:31:00'),
				end_time_ms: at('05:33:00'),
				confidence: null,
				bbox: null,
				zone: 'porch',
				thumbnail_url: makeSceneDataUri('STORY', '05:31:00', '#a855f7', 'story'),
				workflow: makeWorkflowState('front-door', 'evt-story-morning')
			}
		},
		{
			camera: cameras[1]!,
			event: {
				id: 'evt-motion-raw',
				source_id: 'driveway',
				revision: 1,
				source: 'camera',
				kind: 'motion',
				start_time_ms: at('04:48:51'),
				end_time_ms: at('04:49:10'),
				confidence: null,
				bbox: null,
				zone: null,
				thumbnail_url: null,
				workflow: makeWorkflowState('driveway', 'evt-motion-raw')
			}
		}
	];

	let eventReviewed = $state(false);
	let eventBookmarked = $state(false);

	const workflow = new EventWorkflow({
		async reviewEvents(changes: readonly EventReviewChange[]): Promise<EventWorkflowResult> {
			const updated: EventWorkflowState[] = [];
			for (const change of changes) {
				const key = eventWorkflowKey(change);
				const prev = workflow.stateFor(change) ?? initialWorkflowStates.get(key);
				if (!prev) continue;
				const nextRev = String(Number(prev.reviewRevision) + 1);
				const next: EventWorkflowState = {
					...prev,
					reviewed: change.reviewed,
					dismissed: change.dismissed,
					reviewRevision: nextRev,
					reviewedAtMs: change.reviewed ? at('06:38:00') : null,
					dismissedAtMs: change.dismissed ? at('06:38:00') : null,
					updatedAtMs: at('06:38:00')
				};
				if (next.reviewed) eventReviewed = true;
				updated.push(next);
			}
			return {
				states: updated,
				actorId: workflowIdentity.actorId,
				localWorkspace: true,
				bookmarksIncluded: true
			};
		},
		async bookmarkEvent(change: EventBookmarkChange): Promise<EventWorkflowResult> {
			const key = eventWorkflowKey(change);
			const prev = workflow.stateFor(change) ?? initialWorkflowStates.get(key);
			if (!prev) {
				return {
					states: [],
					actorId: workflowIdentity.actorId,
					localWorkspace: true,
					bookmarksIncluded: true
				};
			}
			const nextRev = String(Number(prev.bookmark?.revision ?? '0') + 1);
			const next: EventWorkflowState = {
				...prev,
				bookmark: {
					active: change.active,
					note: change.note,
					revision: nextRev,
					createdBy: workflowIdentity.actorId,
					createdAtMs: at('06:38:10'),
					updatedBy: workflowIdentity.actorId,
					updatedAtMs: at('06:38:10'),
					eventStartMs: at('06:37:23'),
					eventKind: 'person',
					audit: [
						{
							revision: nextRev,
							actorId: workflowIdentity.actorId,
							occurredAtMs: at('06:38:10'),
							action: change.active ? 'bookmarked' : 'removed'
						}
					]
				}
			};
			if (change.active) eventBookmarked = true;
			return {
				states: [next],
				actorId: workflowIdentity.actorId,
				localWorkspace: true,
				bookmarksIncluded: true
			};
		},
		async getEventWorkflow(targets: readonly EventWorkflowTarget[]): Promise<EventWorkflowResult> {
			const states = targets.flatMap((target) => {
				const found =
					workflow.stateFor(target) ?? initialWorkflowStates.get(eventWorkflowKey(target));
				return found ? [found] : [];
			});
			return {
				states,
				actorId: workflowIdentity.actorId,
				localWorkspace: true,
				bookmarksIncluded: true
			};
		}
	});
	workflow.setActor(workflowIdentity.actorId);
	workflow.hydrate([...initialWorkflowStates.values()], true);

	function matchesExpression(node: EventExpressionNode, record: EventBrowserRecord): boolean {
		if (node.op === 'and') return node.children.every((child) => matchesExpression(child, record));
		if (node.op === 'or') return node.children.some((child) => matchesExpression(child, record));
		if (node.op === 'not') return node.child ? !matchesExpression(node.child, record) : true;
		const pred = node.predicate;
		const ev = record.event;
		switch (pred.kind) {
			case 'camera_ids':
				return pred.values.includes(record.camera.id);
			case 'event_types':
				return pred.values.includes(ev.kind);
			case 'origins':
				return pred.values.includes(ev.source);
			case 'zones':
				return ev.zone !== null && pred.values.includes(ev.zone);
			case 'missing_zone':
				return ev.zone === null;
			case 'confidence':
				if (ev.confidence === null) return false;
				return pred.operator === 'at_least'
					? ev.confidence >= pred.threshold
					: ev.confidence <= pred.threshold;
			case 'missing_confidence':
				return ev.confidence === null;
			case 'has_image':
				return ev.thumbnail_url !== null;
			default:
				return true;
		}
	}

	function measures(
		eventCount: number,
		avgMs: number | null,
		imagePct: number,
		confPct: number,
		zonePct: number
	): ActivityMeasures {
		return {
			eventCount,
			distinctEventCount: eventCount,
			intervalEventCount: eventCount,
			openIntervalEventCount: 0,
			totalDurationMs: (avgMs ?? 0) * eventCount,
			averageDurationMs: avgMs,
			withImageCount: Math.round((eventCount * imagePct) / 100),
			withConfidenceCount: Math.round((eventCount * confPct) / 100),
			withZoneCount: Math.round((eventCount * zonePct) / 100),
			percentWithImage: imagePct,
			percentWithConfidence: confPct,
			percentWithZone: zonePct
		};
	}

	const dailyBuckets: ActivityTimeBucket[] = [
		['2026-08-12', 24, 'partial_retention'],
		['2026-08-13', 31, 'complete'],
		['2026-08-14', 28, 'complete'],
		['2026-08-15', 42, 'complete'],
		['2026-08-16', 38, 'complete'],
		['2026-08-17', 47, 'complete'],
		['2026-08-18', 29, 'partial_current']
	].map(([dateStr, count, completeness], index) => {
		const startMs = Date.parse(`${dateStr}T00:00:00Z`);
		return {
			startMs,
			endMs: startMs + 86_400_000,
			label: String(dateStr),
			utcOffsetSeconds: 0,
			dstFold: 0,
			partialCurrent: completeness === 'partial_current',
			completeness: completeness as ActivityTimeBucket['completeness'],
			retentionPartial: completeness === 'partial_retention',
			sourceHealthGap: false,
			measures: measures(Number(count), 16_400 + index * 900, 92, 86, 78)
		};
	});

	const heatmapCells: ActivityHeatmapCell[] = [];
	for (let weekday = 1; weekday <= 7; weekday += 1) {
		for (let localHour = 0; localHour < 24; localHour += 1) {
			const morningPeak = localHour >= 6 && localHour <= 9 ? 5 : 0;
			const eveningPeak = localHour >= 17 && localHour <= 20 ? 4 : 0;
			const weekendBump = weekday >= 6 && localHour >= 10 && localHour <= 16 ? 3 : 0;
			const base = (weekday + localHour) % 3 === 0 ? 1 : 0;
			heatmapCells.push({
				weekday,
				localHour,
				eventCount: morningPeak + eveningPeak + weekendBump + base,
				distinctEventCount: morningPeak + eveningPeak + weekendBump + base,
				completeness: 'complete'
			});
		}
	}

	function breakdownRow(
		key: string,
		label: string,
		count: number,
		sharePercent: number
	): ActivityBreakdownRow {
		return {
			key,
			label,
			isUnknown: false,
			measures: measures(count, 18_200, 94, 88, 82),
			sharePercent
		};
	}

	const activitySummaryData: ActivitySummaryData = {
		queryId: 'activity-demo-1',
		generatedAtMs: at('06:40:00'),
		startMs: dayStartMs - 6 * 86_400_000,
		endMs: dayStartMs + 86_400_000,
		timezone: 'UTC',
		tzdbVersion: '2026a',
		granularity: 'day',
		weekStart: 'monday',
		analyticsGeneration: 12,
		groupPolicyRevision: 4,
		dataSource: 'rollups',
		rollupState: 'ready',
		totals: measures(239, 18_400, 92.5, 86.2, 79.1),
		comparison: {
			mode: 'previous_duration',
			startMs: dayStartMs - 13 * 86_400_000,
			endMs: dayStartMs - 6 * 86_400_000,
			totals: measures(214, 17_100, 91.0, 84.5, 76.0),
			eventCountDelta: 25,
			eventCountDeltaPercent: 11.7,
			completeness: 'complete',
			retentionPartial: false,
			sourceHealthGap: false
		},
		buckets: dailyBuckets,
		series: [],
		seriesTruncated: false,
		heatmap: heatmapCells,
		byCamera: [
			breakdownRow('front-door', 'Front Door', 112, 46.9),
			breakdownRow('driveway', 'Driveway', 84, 35.1),
			breakdownRow('side-gate', 'Side Gate', 43, 18.0)
		],
		byEventType: [
			breakdownRow('person', 'person', 118, 49.4),
			breakdownRow('vehicle', 'vehicle', 76, 31.8),
			breakdownRow('motion', 'motion', 45, 18.8)
		],
		byZone: [
			breakdownRow('porch', 'porch', 102, 42.7),
			breakdownRow('driveway', 'driveway', 84, 35.1)
		],
		byOrigin: [
			breakdownRow('camera', 'Camera', 178, 74.5),
			breakdownRow('keeppeek', 'KeepPeek / service', 61, 25.5)
		],
		byConfidenceBand: [
			breakdownRow('high', 'High (0.80-1.00)', 142, 59.4),
			breakdownRow('medium', 'Medium (0.50-0.79)', 64, 26.8)
		],
		byGroup: [breakdownRow('perimeter', 'Perimeter', 239, 100.0)],
		retention: {
			partialRetention: false,
			oldestRetainedEventMs: dayStartMs - 14 * 86_400_000,
			deletedEventCountInRange: 0,
			deletedIntervals: []
		},
		sourceHealth: {
			authorizedCameras: 3,
			reportingCameras: 3,
			healthyNoReportsCameras: 0,
			missingHealthCameras: 0,
			noExpectedSourceCameras: 0,
			unknownHistoryCameras: 0,
			cameras: []
		},
		provenanceNotice:
			'Counts reflect producer-reported event metadata within current retention and authorization scope, not authoritative real-world identity, occupancy, or threat totals.',
		exact: true
	};

	const activityStatusData: ActivityAnalyticsStatusData = {
		state: 'ready',
		analyticsGeneration: 12,
		schemaVersion: 1,
		siteTimezone: 'UTC',
		weekStart: 'monday',
		tzdbVersion: '2026a',
		rollupRowCount: 4_820,
		maxRollupRows: 500_000,
		oldestRetainedEventMs: dayStartMs - 14 * 86_400_000,
		lastRebuildCompletedAtMs: at('04:00:00'),
		statusDetail: null
	};

	let activeTab = $state<'events' | 'activity'>('events');
	let showAdvanced = $state(false);
	let expressionDraft = $state<EventExpressionNode>({
		op: 'and',
		children: [{ op: 'predicate', predicate: { kind: 'event_types', values: ['person'] } }]
	});
	let appliedExpression = $state<EventExpressionNode | null>(null);
	let diagnostics = $state<EventPlannerDiagnostics | null>(null);
	let selectedRecordId = $state<string | null>(null);
	let searchApplied = $state(false);
	let csvExported = $state(false);
	let exportBanner = $state<string | null>(null);
	let heatmapFocused = $state(false);
	let activityScroller = $state<HTMLDivElement | null>(null);
	let activityControls = $state<ActivityUiControls>({
		rangePreset: '7d',
		granularity: 'day',
		timezone: 'UTC',
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
	});

	let records = $derived.by(() => {
		const liveRecords = allRecords.map((record) => {
			const wf = workflow.stateFor({ sourceId: record.camera.id, eventId: record.event.id });
			return wf ? { ...record, event: { ...record.event, workflow: wf } } : record;
		});
		if (!appliedExpression) return liveRecords;
		return liveRecords.filter((record) => matchesExpression(appliedExpression!, record));
	});

	let selectedRecord = $derived(
		selectedRecordId ? (records.find((r) => r.event.id === selectedRecordId) ?? null) : null
	);

	let demoComplete = $derived(
		searchApplied && eventReviewed && eventBookmarked && csvExported && heatmapFocused
	);

	function applyExpression(): void {
		appliedExpression = expressionDraft;
		searchApplied = true;
		diagnostics = {
			planCategory: 'indexed_expression',
			estimatedCandidates: allRecords.length,
			scannedCandidates: allRecords.length,
			wallTimeMs: 2
		};
	}

	function focusHeatmap(): void {
		const scroller = activityScroller;
		if (!scroller) return;
		const trend = scroller.querySelector<HTMLElement>('#activity-trend-heading');
		if (trend) {
			const top = trend.getBoundingClientRect().top - scroller.getBoundingClientRect().top;
			scroller.scrollTop += Math.max(0, top - 16);
		} else {
			scroller.scrollTop = scroller.scrollHeight;
		}
		heatmapFocused = true;
	}
</script>

<main
	data-paper-scenario="events.desktop.browse"
	data-demo-events-search
	data-demo-events-complete={demoComplete ? 'true' : undefined}
	class="flex h-[800px] w-[1280px] overflow-hidden bg-ground text-foreground [font-synthesis:none]"
>
	<DesktopPaperRail active="events" paperFull />

	<section class="flex min-h-0 min-w-0 flex-1 flex-col">
		<header
			class="flex h-14 shrink-0 items-center justify-between gap-4 border-b border-hairline bg-surface px-5"
		>
			<div class="flex items-center gap-3">
				<h1 class="text-sm font-semibold">
					{activeTab === 'events' ? 'Events' : 'Activity summary'}
				</h1>
				<span class="rounded-sm border border-hairline bg-raised px-2 py-0.5 font-mono text-2xs">
					2026-08-18 UTC
				</span>
				{#if activeTab === 'events'}
					<span class="font-mono text-2xs text-text-muted">
						{records.length} matching events
					</span>
				{/if}
			</div>

			<div class="flex items-center gap-2">
				{#if activeTab === 'events'}
					<button
						type="button"
						data-demo-action="toggle-advanced-search"
						aria-pressed={showAdvanced}
						class="inline-flex h-8 items-center gap-1.5 rounded-sm border px-3 text-xs font-medium {showAdvanced
							? 'border-primary bg-primary/15 text-primary-soft'
							: 'border-hairline-strong bg-raised text-foreground'}"
						onclick={() => (showAdvanced = !showAdvanced)}
					>
						<SlidersHorizontalIcon class="size-3.5" />
						Advanced search
					</button>
				{:else}
					<button
						type="button"
						data-demo-action="focus-heatmap"
						class="inline-flex h-8 items-center gap-1.5 rounded-sm border border-primary/50 bg-primary/10 px-3 text-xs font-medium text-primary-soft"
						onclick={focusHeatmap}
					>
						<BarChart3Icon class="size-3.5" />
						Focus trend &amp; heatmap
					</button>
				{/if}

				<div class="flex items-center gap-1 rounded-sm bg-raised p-1">
					<button
						type="button"
						data-demo-action="tab-events"
						aria-pressed={activeTab === 'events'}
						class="inline-flex h-7 items-center gap-1.5 rounded-xs px-2.5 text-xs font-medium {activeTab ===
						'events'
							? 'bg-primary text-on-primary'
							: 'text-text-muted hover:text-foreground'}"
						onclick={() => (activeTab = 'events')}
					>
						<SearchIcon class="size-3.5" />
						Events
					</button>
					<button
						type="button"
						data-demo-action="tab-activity"
						aria-pressed={activeTab === 'activity'}
						class="inline-flex h-7 items-center gap-1.5 rounded-xs px-2.5 text-xs font-medium {activeTab ===
						'activity'
							? 'bg-primary text-on-primary'
							: 'text-text-muted hover:text-foreground'}"
						onclick={() => (activeTab = 'activity')}
					>
						<BarChart3Icon class="size-3.5" />
						Activity
					</button>
				</div>
			</div>
		</header>

		{#if activeTab === 'events'}
			<div class="min-h-0 flex-1 space-y-4 overflow-y-auto p-4">
				{#if showAdvanced}
					<EventExpressionBuilder
						draft={expressionDraft}
						{cameras}
						canConvertToCommon={true}
						semanticAvailable={false}
						{diagnostics}
						onchange={(next) => (expressionDraft = next)}
						onapply={applyExpression}
					/>
				{/if}

				<div class="grid grid-cols-3 gap-3">
					{#each records as record (record.event.id)}
						<EventResultCard
							{record}
							selected={selectedRecord?.event.id === record.event.id}
							onclick={() => (selectedRecordId = record.event.id)}
						/>
					{/each}
				</div>
			</div>
		{:else}
			<div
				bind:this={activityScroller}
				data-demo-activity
				class="min-h-0 flex-1 space-y-3 overflow-y-auto p-4"
			>
				{#if exportBanner}
					<p
						role="status"
						class="inline-flex items-center gap-2 rounded-sm border border-healthy/40 bg-healthy/10 px-3 py-1.5 font-mono text-xs text-healthy"
					>
						<CheckIcon class="size-3.5" />
						{exportBanner}
					</p>
				{/if}
				<ActivitySummary
					summary={activitySummaryData}
					status={activityStatusData}
					{cameras}
					controls={activityControls}
					eventsPath="/events"
					onControlsChange={(next) => (activityControls = next)}
					onRefresh={() => {}}
					onExport={(format, filename, content) => {
						const rowCount = content.trim().split('\n').length;
						csvExported = true;
						exportBanner = `Exported ${format.toUpperCase()} · ${filename} (${rowCount} lines)`;
					}}
				/>
			</div>
		{/if}
	</section>

	{#if selectedRecord}
		<EventDetailDrawer
			record={selectedRecord}
			{workflow}
			{workflowIdentity}
			onclose={() => (selectedRecordId = null)}
		/>
	{/if}
</main>
