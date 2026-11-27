import type { DemoScenarioDefinition } from '../src/lib/storybook/demo';

export const digitalZoomDemo = {
	metadata: {
		storyId: 'demos-digital-zoom--zoom-and-pan',
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: 'MX-0',
			frameId: 'N7-0',
			scenarioId: 'peek.desktop.live-wall'
		},
		demo: {
			assetId: 'viewer.desktop.digital-zoom',
			title: 'Inspect details with digital zoom and pan',
			purpose:
				'Show browser-side digital zoom from 1.0x to 4.0x, bounded pointer drag panning, 40 px keyboard arrow panning, and instant reset to 1.0x without moving physical PTZ.',
			narration: {
				voice: 'coral',
				instructions: 'Use a calm, concise product-demo voice with crisp pacing and clear stops.',
				cues: [
					{
						atMs: 0,
						text: 'Digital zoom inspects already-decoded video in Viewer, Keep, and Events without moving the camera or switching streams.',
						pauseAfterMs: 250
					},
					{
						atMs: 2_500,
						text: 'Each click on Digital zoom in doubles the magnification. Two clicks take it from one times to four times.',
						pauseAfterMs: 250
					},
					{
						atMs: 6_000,
						text: 'Drag inside the zoomed frame to pan toward details while staying clamped to the fitted image bounds.',
						pauseAfterMs: 250
					},
					{
						atMs: 9_000,
						text: 'When the viewport has focus, arrow keys nudge the crop by forty pixels per press.',
						pauseAfterMs: 250
					},
					{
						atMs: 12_200,
						text: "Reset digital zoom returns to one times instantly. Recordings, exports, and the camera's PTZ stay untouched.",
						pauseAfterMs: 300
					}
				]
			},
			durationMs: 15_500,
			viewport: { width: 1280, height: 800 },
			captions: [
				{ atMs: 0, text: 'Digital zoom magnifies local pixels without moving PTZ.' },
				{ atMs: 2_500, text: 'Click Digital zoom in twice to reach 4.0x.' },
				{ atMs: 6_000, text: 'Drag inside the viewport to pan within fitted bounds.' },
				{ atMs: 9_000, text: 'Press arrow keys for 40 px keyboard pans while focused.' },
				{ atMs: 12_200, text: 'Reset returns to 1.0x. Recordings and PTZ remain untouched.' }
			],
			actions: [
				{ kind: 'click', atMs: 2_500, selector: 'button[aria-label="Digital zoom in"]' },
				{ kind: 'click', atMs: 4_300, selector: 'button[aria-label="Digital zoom in"]' },
				{
					kind: 'pointer-drag',
					atMs: 6_000,
					selector: '[aria-label="Digital zoom viewport"]',
					deltaX: -520,
					deltaY: -220,
					durationMs: 1_800,
					holdAfterMs: 400,
					steps: 24
				},
				{
					kind: 'press',
					atMs: 9_000,
					selector: '[aria-label="Digital zoom viewport"]',
					key: 'ArrowRight'
				},
				{
					kind: 'press',
					atMs: 9_700,
					selector: '[aria-label="Digital zoom viewport"]',
					key: 'ArrowDown'
				},
				{
					kind: 'press',
					atMs: 10_400,
					selector: '[aria-label="Digital zoom viewport"]',
					key: 'ArrowRight'
				},
				{ kind: 'click', atMs: 12_200, selector: 'button[aria-label="Reset digital zoom"]' }
			],
			completionSignal: {
				selector: '[data-demo-digital-zoom-complete="true"]',
				state: 'visible'
			}
		}
	},
	previewScenarioId: 'peek.desktop.live-wall',
	storySource: 'visual-harness/stories/DemoDigitalZoom.stories.ts',
	fixtureSources: [
		'visual-harness/stories/DemoDigitalZoomStory.svelte',
		'visual-harness/stories/DemoDigitalZoom.stories.ts',
		'src/lib/components/FocusedMediaViewport.svelte',
		'src/lib/focused-media-gestures.ts',
		'src/lib/focused-media-geometry.ts'
	]
} as const satisfies DemoScenarioDefinition;

export const keepTimelineExportDemo = {
	metadata: {
		storyId: 'demos-keep-timeline--review-and-export',
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: 'C4-0',
			frameId: 'CE-0',
			scenarioId: 'keep.desktop.timeline-anatomy'
		},
		demo: {
			assetId: 'keep.desktop.timeline-export',
			title: 'Review recorded footage and export evidence in Keep',
			purpose:
				'Walk through Keep Timeline UTC scrubbing and gaps, Stories and Footage calendar, multi-camera Swimlanes, evidence MP4 export with SHA-256 verification, and bounded silent Timelapse review.',
			narration: {
				voice: 'coral',
				instructions:
					'Use a clear, factual product-demo voice that emphasizes UTC timekeeping and verified evidence.',
				cues: [
					{
						atMs: 0,
						text: 'Keep opens recorded footage on one UTC timeline, keeping recorded coverage and explicit gaps clearly separated.',
						pauseAfterMs: 250
					},
					{
						atMs: 2_200,
						text: 'Select an event on the timeline to jump to its exact UTC time.',
						pauseAfterMs: 250
					},
					{
						atMs: 4_800,
						text: 'Stories lists summaries published by your event sources, next to a footage calendar where only days with recordings can be selected.',
						pauseAfterMs: 250
					},
					{
						atMs: 8_400,
						text: 'Swimlanes compares indexed coverage and events across authorized cameras on one shared UTC clock.',
						pauseAfterMs: 250
					},
					{
						atMs: 11_000,
						text: 'In Export, choose timestamp burn-in if needed and start the server job. Keep tracks progress from Running to Ready with a SHA-256 checksum.',
						pauseAfterMs: 300
					},
					{
						atMs: 17_600,
						text: 'Timelapse samples sparse frames across quiet periods, marks omitted gaps, and generates a silent MP4 with a provenance manifest.',
						pauseAfterMs: 300
					}
				]
			},
			durationMs: 22_500,
			viewport: { width: 1280, height: 800 },
			captions: [
				{ atMs: 0, text: 'Keep Timeline separates recorded UTC coverage from explicit gaps.' },
				{ atMs: 2_200, text: 'Select an event on the timeline to jump to its UTC moment.' },
				{ atMs: 4_800, text: 'Stories and Footage calendar: only days with dots are selectable.' },
				{ atMs: 8_400, text: 'Swimlanes aligns indexed coverage across authorized cameras.' },
				{ atMs: 11_000, text: 'Export transitions Running to Ready with verified SHA-256.' },
				{ atMs: 17_600, text: 'Timelapse plans sparse frames, gaps, and silent MP4 export.' }
			],
			actions: [
				{ kind: 'click', atMs: 2_200, selector: '[data-timeline-event="delivery"]' },
				{ kind: 'click', atMs: 4_800, selector: '[data-demo-action="mode-stories"]' },
				{ kind: 'click', atMs: 6_800, selector: '[data-calendar-date="2026-08-17"]' },
				{ kind: 'click', atMs: 8_400, selector: '[data-demo-action="mode-swimlanes"]' },
				{ kind: 'click', atMs: 11_000, selector: '[data-demo-action="mode-export"]' },
				{ kind: 'click', atMs: 12_600, selector: '[data-keep-export] input[type="checkbox"]' },
				{
					kind: 'click',
					atMs: 13_600,
					selector: '[data-keep-export] button:has-text("Create export")'
				},
				{ kind: 'click', atMs: 17_600, selector: '[data-demo-action="mode-timelapse"]' },
				{ kind: 'click', atMs: 19_200, selector: '[data-timelapse-allow-partial]' },
				{ kind: 'click', atMs: 20_000, selector: '[data-timelapse-generate]' }
			],
			completionSignal: {
				selector: '[data-demo-keep-complete="true"]',
				state: 'visible'
			}
		}
	},
	previewScenarioId: 'keep.desktop.timeline-anatomy',
	storySource: 'visual-harness/stories/DemoKeepTimeline.stories.ts',
	fixtureSources: [
		'visual-harness/stories/DemoKeepTimelineStory.svelte',
		'visual-harness/stories/DemoKeepTimeline.stories.ts',
		'src/lib/components/VerticalTimeline.svelte',
		'src/lib/components/KeepStories.svelte',
		'src/lib/components/KeepSwimlanes.svelte',
		'src/lib/components/KeepExportPanel.svelte',
		'src/lib/components/TimelapseReview.svelte'
	]
} as const satisfies DemoScenarioDefinition;

export const eventsSearchReviewDemo = {
	metadata: {
		storyId: 'demos-events-search--filter-detail-activity',
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '1GP-0',
			frameId: '1GZ-0',
			scenarioId: 'events.desktop.browse'
		},
		demo: {
			assetId: 'events.desktop.search-review',
			title: 'Filter events, inspect detail evidence, and review Activity',
			purpose:
				'Show the Events grid, boolean Advanced search filtering, EventDetailDrawer canonical preview with bounding box and review/bookmark controls, and Activity summary trends, heatmap, and CSV/JSON export.',
			narration: {
				voice: 'coral',
				instructions:
					'Use a calm, concise product-demo voice emphasizing factual event evidence and bounded summaries.',
				cues: [
					{
						atMs: 0,
						text: 'Events lists reported detections across authorized cameras with UTC timestamps, origins, and canonical previews.',
						pauseAfterMs: 250
					},
					{
						atMs: 2_500,
						text: 'Open Advanced search to compose boolean predicates on event type, origin, or confidence threshold.',
						pauseAfterMs: 250
					},
					{
						atMs: 6_800,
						text: 'Run the search to narrow the grid to matching events from cameras you can access.',
						pauseAfterMs: 250
					},
					{
						atMs: 9_200,
						text: 'Open an event card to inspect its canonical image, bounding box, origin, and review or bookmark controls.',
						pauseAfterMs: 250
					},
					{
						atMs: 15_200,
						text: 'Switch to Activity summary for bucket trends, a weekday-by-hour heatmap, and CSV or JSON exports.',
						pauseAfterMs: 300
					}
				]
			},
			durationMs: 20_500,
			viewport: { width: 1280, height: 800 },
			captions: [
				{ atMs: 0, text: 'Browse reported Events across authorized cameras in UTC.' },
				{ atMs: 2_500, text: 'Add a confidence predicate in Advanced boolean search.' },
				{ atMs: 6_800, text: 'Apply the boolean expression to filter matching events.' },
				{ atMs: 9_200, text: 'Inspect canonical preview, bounding box, review, and bookmark.' },
				{ atMs: 15_200, text: 'Activity summary: bucket trends, heatmap, and CSV/JSON export.' }
			],
			actions: [
				{ kind: 'click', atMs: 2_500, selector: '[data-demo-action="toggle-advanced-search"]' },
				{
					kind: 'click',
					atMs: 4_200,
					selector: '[data-testid="event-expression-builder"] button:has-text("Add condition")'
				},
				{
					kind: 'select',
					atMs: 5_400,
					selector:
						'[data-testid="event-expression-builder"] [data-expression-predicate]:nth-of-type(2) select[aria-label="Predicate field"]',
					value: 'confidence'
				},
				{
					kind: 'click',
					atMs: 6_800,
					selector:
						'[data-testid="event-expression-builder"] button:has-text("Run advanced search")'
				},
				{
					kind: 'click',
					atMs: 9_200,
					selector: '[data-event-card="front-door:evt-person-porch"]'
				},
				{
					kind: 'click',
					atMs: 11_600,
					selector: '[data-event-detail] button[aria-label="Mark event reviewed"]'
				},
				{
					kind: 'click',
					atMs: 12_800,
					selector: '[data-event-detail] button[aria-label="Bookmark event"]'
				},
				{
					kind: 'click',
					atMs: 14_300,
					selector: '[data-event-detail] button[aria-label="Close event detail"]'
				},
				{ kind: 'click', atMs: 15_200, selector: '[data-demo-action="tab-activity"]' },
				{
					kind: 'click',
					atMs: 16_800,
					selector: '[data-demo-activity] button:has-text("Export CSV")'
				},
				{ kind: 'click', atMs: 17_800, selector: '[data-demo-action="focus-heatmap"]' }
			],
			completionSignal: {
				selector: '[data-demo-events-complete="true"]',
				state: 'visible'
			}
		}
	},
	previewScenarioId: 'events.desktop.browse',
	storySource: 'visual-harness/stories/DemoEventsSearch.stories.ts',
	fixtureSources: [
		'visual-harness/stories/DemoEventsSearchStory.svelte',
		'visual-harness/stories/DemoEventsSearch.stories.ts',
		'src/lib/components/EventExpressionBuilder.svelte',
		'src/lib/components/EventResultCard.svelte',
		'src/lib/components/EventDetailDrawer.svelte',
		'src/lib/components/ActivitySummary.svelte'
	]
} as const satisfies DemoScenarioDefinition;

export const guestReviewDemo = {
	metadata: {
		storyId: 'demos-guest-review--policy-share-revoke',
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '4VX-0',
			frameId: '4WA-0',
			scenarioId: 'keep.desktop.export-lifecycle'
		},
		demo: {
			assetId: 'share.desktop.guest-review',
			title: 'Configure, issue, preview, and revoke a guest review link',
			purpose:
				'Show enabling Guest sharing with a canonical HTTPS origin, creating a one-time link from a ready export, previewing the isolated /share guest page, and revoking the grant with a reason in Guest links.',
			narration: {
				voice: 'coral',
				instructions:
					'Use a clear, security-minded product-demo voice highlighting one-time link disclosure and strict guest isolation.',
				cues: [
					{
						atMs: 0,
						text: 'Guest sharing defaults off. Configure a canonical external HTTPS origin and save the policy in Access settings.',
						pauseAfterMs: 250
					},
					{
						atMs: 5_200,
						text: 'From a ready export, inspect measured file bounds, set a label and view limit, and create a link whose secret is shown only once.',
						pauseAfterMs: 250
					},
					{
						atMs: 12_200,
						text: 'Recipients open the isolated share page with no KeepPeek account or app shell, limited strictly to this clip.',
						pauseAfterMs: 250
					},
					{
						atMs: 16_200,
						text: 'In Guest links, inspect usage counters and revoke access with a reason. Link secrets are never shown again.',
						pauseAfterMs: 300
					}
				]
			},
			durationMs: 21_500,
			viewport: { width: 1280, height: 800 },
			captions: [
				{ atMs: 0, text: 'Enable Guest sharing with a canonical HTTPS origin.' },
				{
					atMs: 5_200,
					text: 'Inspect export bounds, set a view limit, and reveal the link secret once.'
				},
				{ atMs: 12_200, text: 'Isolated /share view: one clip, no app shell or account.' },
				{ atMs: 16_200, text: 'Track views and revoke the grant with a reason.' }
			],
			actions: [
				{ kind: 'click', atMs: 1_800, selector: '[data-demo-guest-policy] input[type="checkbox"]' },
				{
					kind: 'fill',
					atMs: 2_600,
					selector: '[data-demo-guest-policy] input[placeholder="https://recorder.example"]',
					value: 'https://review.example'
				},
				{ kind: 'click', atMs: 3_600, selector: '[data-demo-guest-policy] button[type="submit"]' },
				{ kind: 'click', atMs: 5_200, selector: '[data-demo-action="stage-export"]' },
				{ kind: 'click', atMs: 6_200, selector: '[data-demo-action="open-share-dialog"]' },
				{
					kind: 'fill',
					atMs: 7_600,
					selector: 'dialog input[maxlength="128"]',
					value: 'Delivery arrival — Front Door'
				},
				{
					kind: 'fill',
					atMs: 8_500,
					selector: 'dialog input[placeholder="Unlimited"]:not([disabled])',
					value: '3'
				},
				{ kind: 'click', atMs: 9_400, selector: 'dialog button[type="submit"]' },
				{
					kind: 'click',
					atMs: 11_400,
					selector: 'dialog button[aria-label="Close guest sharing"]'
				},
				{ kind: 'click', atMs: 12_200, selector: '[data-demo-action="stage-guest"]' },
				{ kind: 'click', atMs: 14_800, selector: '[data-demo-action="guest-end-review"]' },
				{ kind: 'click', atMs: 16_200, selector: '[data-demo-action="stage-grants"]' },
				{
					kind: 'click',
					atMs: 17_600,
					selector: '[data-demo-guest-grants] button:has-text("Revoke link")'
				},
				{
					kind: 'fill',
					atMs: 18_400,
					selector: 'dialog input[maxlength="256"]',
					value: 'Review completed by recipient'
				},
				{
					kind: 'click',
					atMs: 19_200,
					selector: 'dialog button:has-text("Confirm revocation")'
				}
			],
			completionSignal: {
				selector: '[data-demo-guest-review-complete="true"]',
				state: 'visible'
			}
		}
	},
	previewScenarioId: 'keep.desktop.export-lifecycle',
	storySource: 'visual-harness/stories/DemoGuestReview.stories.ts',
	fixtureSources: [
		'visual-harness/stories/DemoGuestReviewStory.svelte',
		'visual-harness/stories/DemoGuestReview.stories.ts',
		'src/lib/components/GuestSharingPolicy.svelte',
		'src/lib/components/GuestShareDialog.svelte',
		'src/lib/components/GuestShareGrants.svelte'
	]
} as const satisfies DemoScenarioDefinition;

export const watchDemoScenarios: readonly DemoScenarioDefinition[] = [
	digitalZoomDemo,
	keepTimelineExportDemo,
	eventsSearchReviewDemo,
	guestReviewDemo
];
