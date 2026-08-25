import { expect, test } from '@playwright/test';
import type { Locator } from '@playwright/test';
import { mockControlPeer, type HealthFixture } from './fixtures/control-peer';

const healthSnapshot: HealthFixture = {
	status: 'degraded',
	health_contract_version: 1,
	generated_at_ms: Date.UTC(2026, 7, 10, 12),
	uptime_seconds: 3_661,
	version: '0.1.0',
	totals: {
		configured_cameras: 9,
		connected_cameras: 8,
		fresh_cameras: 8,
		decodable_cameras: 8,
		recording_requested_cameras: 9,
		recording_cameras: 8,
		unknown_cameras: 0,
		configured_video_streams: 18,
		connected_video_streams: 16,
		fresh_video_streams: 16,
		decodable_video_streams: 16,
		recording_requested_video_streams: 18,
		recording_video_streams: 16,
		ingress_fps: 280,
		ingress_bitrate_bps: 42_000_000,
		frames: 1_234_567,
		keyframes: 12_345,
		drops: 3,
		errors: 4,
		reconnects: 5
	},
	system: {
		host_name: 'keeppeek.local',
		os_name: 'macOS',
		os_version: 'macOS 15.0',
		kernel_version: '24.0.0',
		architecture: 'aarch64',
		system_uptime_seconds: 86_400,
		boot_time_seconds: 1_723_204_800,
		logical_cores: 8,
		physical_cores: 8,
		cpu_brand: 'Apple M-series',
		system_cpu_percent: 32.5,
		process: {
			pid: 1234,
			name: 'keeppeek',
			executable: '/opt/keeppeek/bin/keeppeek',
			working_directory: '/opt/keeppeek',
			cpu_percent: 148,
			cpu_capacity_percent: 18.5,
			cpu_core_equivalents: 1.48,
			resident_memory_bytes: 536_870_912,
			memory_capacity_percent: 3.125,
			virtual_memory_bytes: 1_073_741_824,
			started_at_seconds: 1_723_204_800,
			uptime_seconds: 3_661,
			tasks: 24,
			read_bytes_per_second: 2_000_000,
			write_bytes_per_second: 8_000_000,
			total_read_bytes: 10_000_000_000,
			total_written_bytes: 20_000_000_000
		},
		memory: {
			total_bytes: 17_179_869_184,
			used_bytes: 10_000_000_000,
			available_bytes: 7_179_869_184,
			total_swap_bytes: 4_294_967_296,
			used_swap_bytes: 500_000_000
		},
		load: { one_minute: 2.1, five_minutes: 1.8, fifteen_minutes: 1.5 },
		cpus: [{ name: 'cpu0', usage_percent: 30, frequency_mhz: 3_200 }],
		network_egress_bps: 18_765_432,
		networks: [
			{
				name: 'en0',
				received_bytes_per_second: 8_000_000,
				transmitted_bytes_per_second: 2_000_000,
				received_packets_per_second: 5_000,
				transmitted_packets_per_second: 2_000,
				receive_errors: 0,
				transmit_errors: 0,
				total_received_bytes: 1_000_000_000,
				total_transmitted_bytes: 500_000_000
			}
		],
		disks: [
			{
				name: 'Data',
				kind: 'ssd',
				file_system: 'apfs',
				mount_point: '/',
				total_bytes: 1_000_000_000_000,
				available_bytes: 400_000_000_000,
				used_bytes: 600_000_000_000,
				removable: false,
				stores_recordings: true
			}
		],
		temperatures: [{ label: 'CPU', current_celsius: 52, max_celsius: 70, critical_celsius: 100 }]
	},
	storage: {
		medium_term_path: '/recordings',
		long_term_path: '/recordings',
		paths_are_same: true,
		short_term_seconds: 30,
		medium_term_seconds: 60,
		flush_interval_seconds: 10,
		write_buffer_bytes: 1_048_576,
		long_term_max_bytes: 500_000_000_000,
		catalog_bytes: 8_388_608,
		catalog: {
			recording_files: 1_000,
			finalized_files: 984,
			active_files: 16,
			fragments: 50_000,
			fragment_bytes: 400_000_000_000,
			events: 400,
			open_events: 2,
			event_thumbnails: 350
		},
		demand: {
			active_streams: 1,
			total_viewers: 1,
			leased_streams: 1,
			streams: [{ stream_id: 'front-door/main', viewers: 1, lease_remaining_ms: 20_000 }]
		}
	},
	webrtc: {
		active_sessions: 3,
		adaptive_sessions: 1,
		browser_sessions: 1,
		browser_tracks: 2,
		fixed_sessions: 1,
		active_main: 2,
		active_sub: 1,
		requested_auto: 1,
		requested_high: 1,
		requested_low: 1,
		estimated_bitrate_min_bps: 3_000_000,
		estimated_bitrate_avg_bps: 6_000_000,
		estimated_bitrate_max_bps: 9_000_000,
		source_bitrate_bps: 42_000_000,
		published_frames: 1_234_567,
		published_bytes: 456_000_000_000,
		delivered_frames: 23_456,
		written_frames: 22_222,
		queue_capacity: 1_000,
		queued_frames: 3,
		queue_depth_max: 3,
		queue_high_water: 17,
		queue_drops: 7,
		queue_discarded_frames: 1_111,
		queue_recovery_drops: 13,
		session_queues: [
			{
				session_id: 42,
				track_id: 'camera-0',
				camera_ip: '192.168.137.199',
				stream: 'sub',
				depth: 3,
				high_water: 17,
				written_frames: 2_222,
				full_drops: 5,
				discarded_frames: 7,
				recovery_drops: 9
			}
		],
		sources: [
			{
				camera_ip: '192.168.137.199',
				stream: 'main',
				subscribers: 1,
				bitrate_bps: 8_000_000,
				has_keyframe: true,
				keyframe_age_ms: 400
			}
		]
	},
	cameras: [
		{
			id: '192.168.137.121',
			ip: '192.168.137.121',
			name: 'North Courtyard',
			manufacturer: 'Reolink',
			model: 'RLC-820A',
			firmware_version: 'v1',
			backend: 'reo-proto',
			transport: 'tcp',
			state: 'offline',
			reason: 'transport_disconnected',
			reason_codes: ['transport_disconnected'],
			detail: 'Camera transport is disconnected',
			dimensions: {
				configured: true,
				expected: true,
				configured_video_streams: 2,
				connected_video_streams: 0,
				reporting_video_streams: 0,
				fresh_video_streams: 0,
				decodable_video_streams: 0,
				transport_connected: false,
				frames_fresh: false,
				decodable: false,
				recording_requested: true,
				recording_video_streams: 2,
				recording_streams_progressing: 0,
				recording_progressing: false
			},
			lifecycle: 'reconnecting',
			last_error: null,
			configured_profiles: [
				{
					name: 'mainStream',
					stream: 'main',
					encoding: 'h265',
					resolution: '3840x2160',
					framerate: 25
				},
				{ name: 'subStream', stream: 'sub', encoding: 'h264', resolution: '640x360', framerate: 15 }
			],
			streams: []
		},
		{
			id: '192.168.137.199',
			ip: '192.168.137.199',
			name: 'Kitchen Deck',
			manufacturer: 'Reolink',
			model: 'RLC-820A',
			firmware_version: 'v1',
			backend: 'retina',
			transport: 'udp',
			state: 'healthy',
			reason: 'healthy',
			reason_codes: ['healthy'],
			detail: 'Transport, media, keyframe, and recording evidence is current',
			dimensions: {
				configured: true,
				expected: true,
				configured_video_streams: 1,
				connected_video_streams: 1,
				reporting_video_streams: 1,
				fresh_video_streams: 1,
				decodable_video_streams: 1,
				transport_connected: true,
				frames_fresh: true,
				decodable: true,
				recording_requested: true,
				recording_video_streams: 1,
				recording_streams_progressing: 1,
				recording_progressing: true,
				recording_progress_age_ms: 400
			},
			lifecycle: 'connected',
			last_error: null,
			configured_profiles: [
				{
					name: 'mainStream',
					stream: 'main',
					encoding: 'h265',
					resolution: '3840x2160',
					framerate: 25
				}
			],
			streams: [
				{
					type: 'video_main',
					codec: 'h265',
					resolution: '3840x2160',
					fps: 25,
					expected_fps: 25,
					kf_fps: 1,
					kbps: 8_000,
					max_frame_kb: 800,
					gap_min_ms: 39,
					gap_avg_ms: 40,
					gap_max_ms: 50,
					jitter_samples: 249,
					jitter_p50_ms: 1.2,
					jitter_p99_ms: 14.5,
					frames: 100_000,
					bytes: 10_000_000_000,
					keyframes: 4_000,
					reconnects: 1,
					drops: 0,
					errors: 0,
					updated_at_ms: Date.UTC(2026, 7, 10, 12),
					report_age_ms: 2_000,
					frame_age_ms: 200,
					keyframe_age_ms: 400,
					state: 'healthy',
					reason: 'healthy',
					reason_codes: ['healthy'],
					detail: 'Transport, media, keyframe, and recording evidence is current',
					dimensions: {
						expected: true,
						transport_connected: true,
						report_fresh: true,
						frames_fresh: true,
						decodable: true,
						recording_requested: true,
						recording_progressing: true,
						recording_progress_age_ms: 400
					}
				},
				{
					type: 'audio',
					codec: 'aac',
					fps: 15.6,
					kbps: 64,
					max_frame_kb: 0.4,
					frames: 156,
					bytes: 64_000,
					updated_at_ms: Date.UTC(2026, 7, 10, 12),
					report_age_ms: 2_000,
					state: 'healthy',
					reason: 'healthy',
					reason_codes: ['healthy'],
					detail: 'Audio frames are current',
					dimensions: {
						expected: true,
						transport_connected: true,
						report_fresh: true,
						frames_fresh: true,
						decodable: true,
						recording_requested: false
					}
				}
			]
		}
	],
	issues: [
		{
			severity: 'warning',
			scope: 'North Courtyard',
			message: 'Camera transport is disconnected'
		}
	]
};

/* eslint-disable-next-line @typescript-eslint/no-unused-vars */
function transitionHealth(state: 'stale' | 'healthy'): HealthFixture {
	const recovered = state === 'healthy';
	const reason = recovered ? 'healthy' : 'stream_report_stale';
	const detail = recovered
		? 'Transport, media, keyframe, and recording evidence is current'
		: 'One or more stream health reports are stale';
	return {
		...healthSnapshot,
		status: recovered ? 'healthy' : 'degraded',
		totals: {
			...healthSnapshot.totals,
			configured_cameras: 1,
			connected_cameras: 1,
			fresh_cameras: recovered ? 1 : 0,
			decodable_cameras: recovered ? 1 : 0,
			recording_requested_cameras: 1,
			recording_cameras: recovered ? 1 : 0,
			configured_video_streams: 1,
			connected_video_streams: 1,
			fresh_video_streams: recovered ? 1 : 0,
			decodable_video_streams: recovered ? 1 : 0,
			recording_requested_video_streams: 1,
			recording_video_streams: recovered ? 1 : 0
		},
		cameras: [
			{
				id: 'front-door',
				ip: '192.0.2.10',
				name: 'Front Door',
				backend: 'retina',
				transport: 'tcp',
				state,
				reason,
				reason_codes: [reason],
				detail,
				lifecycle: 'connected',
				configured_profiles: [{ name: 'Main', stream: 'main', encoding: 'h264' }],
				dimensions: {
					configured: true,
					expected: true,
					configured_video_streams: 1,
					connected_video_streams: 1,
					reporting_video_streams: 1,
					fresh_video_streams: recovered ? 1 : 0,
					decodable_video_streams: recovered ? 1 : 0,
					transport_connected: true,
					frames_fresh: recovered,
					decodable: recovered,
					recording_requested: true,
					recording_video_streams: 1,
					recording_streams_progressing: recovered ? 1 : 0,
					recording_progressing: recovered
				},
				streams: [
					{
						type: 'video_main',
						codec: 'h264',
						resolution: '1920x1080',
						fps: recovered ? 15 : 0,
						expected_fps: 15,
						updated_at_ms: Date.UTC(2026, 7, 10, 12),
						report_age_ms: recovered ? 100 : 31_000,
						frame_age_ms: recovered ? 100 : 31_000,
						keyframe_age_ms: recovered ? 100 : 31_000,
						state,
						reason,
						reason_codes: [reason],
						detail,
						dimensions: {
							expected: true,
							transport_connected: true,
							report_fresh: recovered,
							frames_fresh: recovered,
							decodable: recovered,
							recording_requested: true,
							recording_progressing: recovered
						}
					}
				]
			}
		],
		issues: recovered ? [] : [{ severity: 'warning', scope: 'Front Door', message: detail }]
	};
}

async function expectMetric(scope: Locator, label: string, value: string | RegExp) {
	const metric = scope.locator(`[data-health-metric="${label}"]`);
	await expect(metric).toBeVisible();
	await expect(metric).toContainText(value);
}

async function expectTexts(scope: Locator, values: Array<string | RegExp>) {
	for (const value of values) await expect(scope).toContainText(value);
}

test('Board 15 shows comprehensive server health and camera outages', async ({ page }) => {
	await mockControlPeer(page, { health: healthSnapshot });

	await page.goto('/system-health');

	await expect(page).toHaveTitle('Health - KeepPeek');
	await expect(page.getByRole('heading', { name: 'Health', exact: true })).toBeVisible();
	await expect(page.getByRole('link', { name: 'Health', exact: true })).toHaveAttribute(
		'aria-current',
		'page'
	);
	const priorityIssue = page.getByRole('region', { name: 'Highest priority health issue' });
	await expect(priorityIssue).toContainText('North Courtyard · Camera transport is disconnected');
	await expect(
		page.getByRole('link', { name: 'Diagnose North Courtyard', exact: true })
	).toHaveAttribute('href', '/system-health/camera/192.168.137.121');
	expect(
		await priorityIssue.evaluate((element) => {
			const tablist = document.querySelector('[role="tablist"][aria-label="Health scope"]');
			return Boolean(
				tablist && element.compareDocumentPosition(tablist) & Node.DOCUMENT_POSITION_FOLLOWING
			);
		})
	).toBe(true);
	const clientTab = page.getByRole('tab', { name: 'Client' });
	const serverTab = page.getByRole('tab', { name: 'Server' });
	await expect(serverTab).toHaveAttribute('aria-selected', 'true');
	await clientTab.click();
	await expect(clientTab).toHaveAttribute('aria-selected', 'true');
	await expect(page.getByRole('heading', { name: 'Current client' })).toBeVisible();
	await expect(page.getByText('No active client streams')).toBeVisible();
	await expect(page.getByRole('heading', { name: 'Process and host' })).toHaveCount(0);
	await serverTab.click();
	await expect(page.getByText('degraded', { exact: true })).toBeVisible();
	const summary = page.getByRole('region', { name: 'Health summary' });
	const findings = page.locator('section').filter({
		has: page.getByRole('heading', { name: 'Current findings' })
	});
	expect(
		await findings.evaluate((element) => {
			const summaryElement = document.querySelector('[aria-label="Health summary"]');
			return Boolean(
				summaryElement &&
				element.compareDocumentPosition(summaryElement) & Node.DOCUMENT_POSITION_FOLLOWING
			);
		})
	).toBe(true);
	await expectMetric(summary, 'Server egress', '18.8 Mbps');
	await expect(summary).toContainText('Non-loopback host traffic');
	await expectMetric(summary, 'Process CPU', '18.5%');
	await expect(summary.locator('[data-health-metric="Process CPU"]')).toContainText(
		'1.48 cores · host 32.5%'
	);
	await expectMetric(summary, 'Process memory', '537 MB');
	await expect(summary.locator('[data-health-metric="Process memory"]')).toContainText(
		'3.1% of 17.2 GB RAM'
	);
	const cameraDimensions = page.getByRole('region', { name: 'Camera health dimensions' });
	await expectTexts(cameraDimensions, [
		/Configured\s*9/,
		/Connected\s*8 \/ 9/,
		/Fresh\s*8 \/ 9/,
		/Decodable\s*8 \/ 9/,
		/Recording\s*8 \/ 9/
	]);
	await expect(page.getByText('North Courtyard', { exact: true }).first()).toBeVisible();
	await expect(page.getByText('retina / udp', { exact: true }).first()).toBeVisible();
	await expect(
		page.getByRole('link', { name: 'Open Kitchen Deck camera information' })
	).toHaveAttribute('href', '/camera?camera=192.168.137.199');
	await expect(page.getByText('offline', { exact: true })).toBeVisible();
	await expect(
		findings.getByText('Camera transport is disconnected', { exact: true })
	).toBeVisible();
	const streams = page.locator('section').filter({
		has: page.getByRole('heading', { name: 'Camera streams' })
	});
	await expectTexts(streams, [
		'8 connected · 8 fresh · 8 decodable · 8 of 9 recording',
		'1.2M frames',
		'12.3K keyframes',
		'3 drops',
		'4 errors',
		'5 reconnects'
	]);
	const videoRow = streams.getByRole('row').filter({ hasText: 'Kitchen Deck' });
	await expectTexts(videoRow, [
		/h265/i,
		'3840x2160',
		'25 / 25',
		'8.0 Mbps',
		'max 800 kB',
		'4K total',
		'100K frames · 10.0 GB',
		'Frames Current',
		'Decodable Current',
		'Recording Current',
		'min 39 · avg 40 ms',
		'max 50 ms',
		'jitter p50 1.2 ms · p99 14.5 ms · 249 samples'
	]);
	const process = page.locator('section').filter({
		has: page.getByRole('heading', { name: 'Process and host' })
	});
	await expectTexts(process, [
		'keeppeek',
		'1234',
		'CPU host capacity',
		'18.5%',
		'CPU core-equivalent',
		'148.0% · 1.48 cores',
		'Host CPU',
		'32.5%',
		'Resident memory',
		'Host RAM share',
		'3.1%',
		'Virtual address space',
		'537 MB',
		'1.07 GB',
		'24',
		'1h 1m',
		'2.00 MB/s',
		'8.00 MB/s',
		'10.0 GB',
		'20.0 GB',
		'7.18 GB available',
		'10.0 GB / 17.2 GB',
		'500 MB / 4.29 GB',
		'2.10',
		'1.80',
		'1.50',
		'cpu0',
		'30.0%',
		'3200 MHz',
		'Apple M-series',
		'8 physical / 8 logical cores'
	]);
	await expect(page.getByRole('heading', { name: 'Audio streams' })).toBeVisible();
	await expect(
		page.getByRole('row', { name: /Kitchen Deck.*aac.*15\.6.*400 B.*N\/A/ })
	).toBeVisible();
	await expect(page.getByText('jitter p50 1.2 ms · p99 14.5 ms')).toBeVisible();
	const webrtc = page.locator('section').filter({
		has: page.getByRole('heading', { name: 'WebRTC delivery' })
	});
	for (const [label, value] of [
		['Sessions', '3'],
		['Browser', '1'],
		['Tracks', '2'],
		['Adaptive', '1'],
		['Fixed', '1'],
		['Main', '2'],
		['Sub', '1'],
		['Auto', '1'],
		['High', '1'],
		['Low', '1'],
		['BWE min', '3.0 Mbps'],
		['BWE avg', '6.0 Mbps'],
		['BWE max', '9.0 Mbps'],
		['Source bitrate', '42.0 Mbps'],
		['Queued', '3'],
		['Deepest', '3'],
		['Capacity', '1000'],
		['Published', '1.2M'],
		['Published bytes', '456 GB'],
		['Enqueued', '23.5K'],
		['Written', '22.2K'],
		['Peak depth', '17'],
		['Full drops', '7'],
		['Discarded', '1.1K'],
		['Recovery drops', '13']
	] as const) {
		await expectMetric(webrtc, label, value);
	}
	await expect(
		webrtc.getByRole('row', {
			name: /^42 camera-0 192\.168\.137\.199 sub 3 \/ 1000 17 2\.2K 7 5 9$/
		})
	).toBeVisible();
	await expect(
		webrtc.getByRole('row', { name: /^192\.168\.137\.199 main 1 8\.0 Mbps Ready now$/ })
	).toBeVisible();
	const storage = page.locator('section').filter({
		has: page.getByRole('heading', { name: 'Recording and storage' })
	});
	await expectTexts(storage, [
		'Paths',
		'Shared',
		'30s',
		'1m 0s',
		'10s',
		'1.05 MB',
		'500 GB',
		'8.39 MB',
		'Active streams',
		'Viewers',
		'Leased streams',
		'1K',
		'984 / 16',
		'50K',
		'400 GB',
		'400 / 2',
		'350',
		'/recordings',
		'front-door/main',
		'20s'
	]);
	await expect(
		storage.getByRole('row', { name: /\/ Data ssd apfs 600 GB \/ 1\.00 TB 400 GB Recordings/ })
	).toBeVisible();
	await expect(page.getByRole('heading', { name: 'Network interfaces' })).toHaveCount(0);
	await expect(page.getByRole('row', { name: /CPU 52\.0 °C 70\.0 °C 100\.0 °C/ })).toBeVisible();
	const runtime = page.locator('section').filter({
		has: page.getByRole('heading', { name: 'Runtime identity' })
	});
	await expectTexts(runtime, [
		'keeppeek.local',
		'aarch64',
		'macOS 15.0',
		'Kernel 24.0.0',
		'/opt/keeppeek/bin/keeppeek',
		'/opt/keeppeek',
		'1d 0h'
	]);

	await page.setViewportSize({ width: 390, height: 844 });
	await expect(page.getByRole('navigation', { name: 'Primary navigation' })).toBeVisible();
	expect(await page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBe(0);
});

test('recovers connected stale media only after fresh server evidence', async ({ page }) => {
	test.setTimeout(20_000);
	await mockControlPeer(page, {
		healthSequence: [transitionHealth('stale'), transitionHealth('healthy')]
	});

	await page.goto('/system-health');

	const dimensions = page.getByRole('region', { name: 'Camera health dimensions' });
	const fresh = dimensions.locator('[data-health-dimension="Fresh"]');
	const streams = page.locator('section').filter({
		has: page.getByRole('heading', { name: 'Camera streams' })
	});
	await expect(fresh).toContainText('0 / 1');
	await expect(streams).toContainText('stream_report_stale');
	await expect(streams).toContainText('Frames Missing');

	await expect(fresh).toContainText('1 / 1', { timeout: 12_000 });
	await expect(streams).toContainText('healthy');
	await expect(streams).toContainText('Frames Current');
	await expect(streams).toContainText('Decodable Current');
	await expect(streams).toContainText('Recording Current');
});

test('keeps the highest-cost health issue and diagnosis action first on mobile', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await mockControlPeer(page, { health: healthSnapshot });

	await page.goto('/system-health');

	const priorityIssue = page.getByRole('region', { name: 'Highest priority health issue' });
	const diagnose = page.getByRole('link', { name: 'Diagnose North Courtyard', exact: true });
	await expect(priorityIssue).toBeInViewport();
	await expect(priorityIssue).toContainText('Camera transport is disconnected');
	await expect(diagnose).toBeInViewport();
	await expect(diagnose).toHaveAttribute('href', '/system-health/camera/192.168.137.121');
	const findings = page.getByRole('heading', { name: 'Open issues' });
	await expect(findings).toBeInViewport();
	const mobileOverview = page.locator('[data-mobile-health-overview]');
	await expect(mobileOverview).toContainText('North Courtyard offline');
	await expect(mobileOverview).toContainText('CONFIG');
	await expect(mobileOverview).toContainText('LINK');
	await expect(mobileOverview).toContainText('FRESH');
	await expect(mobileOverview).toContainText('DECODE');
	await expect(mobileOverview).toContainText('RECORD');
	expect(
		await mobileOverview.evaluate((element) => Math.round(element.getBoundingClientRect().width))
	).toBe(390);
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});
