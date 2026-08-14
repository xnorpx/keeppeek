import { expect, test } from '@playwright/test';

test('renders the KeepPeek dashboard without configured cameras', async ({ page }) => {
	await page.route('**/health', async (route) => {
		await route.fulfill({ json: { status: 'ok', cameras: [] } });
	});
	await page.route('**/api/cameras', async (route) => {
		await route.fulfill({ json: [] });
	});

	await page.goto('/');

	await expect(page).toHaveTitle('Peek - KeepPeek');
	await expect(page.getByRole('heading', { name: 'Peek', exact: true })).toBeVisible();
	await expect(page.getByText('System online', { exact: true })).toBeVisible();
	await expect(page.getByText('0 cameras', { exact: true })).toBeVisible();
	await expect(page.getByText('No cameras configured.')).toBeVisible();
});

test('links from Peek to the full camera information page', async ({ page }) => {
	await page.addInitScript(() => {
		class MockPeerConnection {
			localDescription: RTCSessionDescriptionInit | null = null;
			iceGatheringState: RTCIceGatheringState = 'complete';
			connectionState: RTCPeerConnectionState = 'connected';
			iceConnectionState: RTCIceConnectionState = 'connected';
			ontrack: RTCPeerConnection['ontrack'] = null;
			onconnectionstatechange: RTCPeerConnection['onconnectionstatechange'] = null;
			oniceconnectionstatechange: RTCPeerConnection['oniceconnectionstatechange'] = null;
			private statsTimestamp = performance.now();
			private bytesReceived = 1_000_000;
			private framesReceived = 300;
			private framesDecoded = 300;
			private transceivers: RTCRtpTransceiver[] = [];
			private receiver = {
				getStats: async (): Promise<RTCStatsReport> => {
					this.statsTimestamp += 1_000;
					this.bytesReceived += 250_000;
					this.framesReceived += 15;
					this.framesDecoded += 15;
					return new Map<string, object>([
						[
							'inbound',
							{
								id: 'inbound',
								type: 'inbound-rtp',
								kind: 'video',
								ssrc: 1,
								timestamp: this.statsTimestamp,
								codecId: 'codec',
								transportId: 'transport',
								bytesReceived: this.bytesReceived,
								packetsReceived: 10_000,
								packetsLost: 2,
								frameWidth: 640,
								frameHeight: 360,
								framesReceived: this.framesReceived,
								framesPerSecond: 15,
								framesDecoded: this.framesDecoded,
								keyFramesDecoded: 12,
								framesDropped: 3,
								freezeCount: 1,
								totalFreezesDuration: 0.2,
								jitter: 0.004,
								jitterBufferDelay: 1.5,
								jitterBufferEmittedCount: 300,
								totalDecodeTime: 0.6,
								nackCount: 4,
								pliCount: 1,
								decoderImplementation: 'Mock decoder',
								powerEfficientDecoder: true
							}
						],
						['codec', { id: 'codec', type: 'codec', mimeType: 'video/H264' }],
						[
							'transport',
							{
								id: 'transport',
								type: 'transport',
								timestamp: this.statsTimestamp,
								selectedCandidatePairId: 'candidate-pair'
							}
						],
						[
							'candidate-pair',
							{
								id: 'candidate-pair',
								type: 'candidate-pair',
								timestamp: this.statsTimestamp,
								currentRoundTripTime: 0.012
							}
						]
					]) as unknown as RTCStatsReport;
				}
			} as RTCRtpReceiver;

			addTransceiver(): RTCRtpTransceiver {
				const transceiver = {
					mid: null,
					setCodecPreferences() {}
				} as unknown as RTCRtpTransceiver;
				this.transceivers.push(transceiver);
				return transceiver;
			}

			async createOffer(): Promise<RTCSessionDescriptionInit> {
				this.transceivers.forEach((transceiver, index) => {
					(transceiver as unknown as { mid: string }).mid = `${index}`;
				});
				return { type: 'offer', sdp: 'v=0\r\n' };
			}

			async setLocalDescription(description: RTCSessionDescriptionInit): Promise<void> {
				this.localDescription = description;
			}

			async setRemoteDescription(): Promise<void> {
				this.ontrack?.({
					receiver: this.receiver,
					streams: [new MediaStream()],
					transceiver: this.transceivers[0]
				} as unknown as RTCTrackEvent);
			}

			close() {}
		}

		Object.defineProperty(window, 'RTCPeerConnection', { value: MockPeerConnection });
		Object.defineProperty(navigator, 'sendBeacon', { value: () => true });
	});
	await page.route('**/health', async (route) => {
		await route.fulfill({
			json: { status: 'ok', cameras: [{ id: 'north-garden', state: 'online' }] }
		});
	});
	await page.route('**/api/cameras', async (route) => {
		await route.fulfill({
			json: [
				{
					id: 'north-garden',
					ip: '192.0.2.10',
					name: 'North Garden',
					manufacturer: 'Reolink',
					model: 'RLC-820A',
					firmware_version: 'v1.2.3',
					is_reolink: true,
					profiles: [
						{
							name: 'Main',
							stream: 'main',
							encoding: 'h265',
							resolution: '3840x2160',
							framerate: 25
						},
						{
							name: 'Sub',
							stream: 'sub',
							encoding: 'h264',
							resolution: '640x360',
							framerate: 15
						}
					]
				}
			]
		});
	});
	await page.route('**/api/live/browser/offer', async (route) => {
		await route.fulfill({
			json: {
				session_id: 1,
				answer: { type: 'answer', sdp: 'v=0\r\n' },
				estimated_bitrate_bps: null,
				tracks: [
					{
						track_id: 'camera-0',
						requested_quality: 'low',
						active_stream: 'sub',
						estimated_bitrate_bps: null
					}
				]
			}
		});
	});
	await page.route('**/api/live/browser/1', async (route) => {
		await route.fulfill({
			json: {
				estimated_bitrate_bps: 1_500_000,
				tracks: [
					{
						track_id: 'camera-0',
						requested_quality: 'low',
						active_stream: 'sub',
						estimated_bitrate_bps: 1_500_000
					}
				]
			}
		});
	});
	await page.route('**/api/live/browser/1/tracks/camera-0/quality', async (route) => {
		await route.fulfill({
			json: {
				track_id: 'camera-0',
				requested_quality: 'auto',
				active_stream: 'sub',
				estimated_bitrate_bps: 1_500_000
			}
		});
	});
	await page.route('**/api/live/browser/1/close', async (route) => {
		await route.fulfill({ status: 204 });
	});
	await page.route('**/api/recordings/north-garden', async (route) => {
		await route.fulfill({
			json: { camera_id: 'north-garden', date: null, dates: [], segments: [] }
		});
	});

	await page.goto('/');

	await expect(page.getByText('1 camera', { exact: true })).toBeVisible();
	await expect(page.getByText('192.0.2.10', { exact: true })).toHaveCount(0);
	await expect(page.getByText('RLC-820A', { exact: true })).toHaveCount(0);

	const diagnosticsTrigger = page.getByRole('button', { name: 'WebRTC stream diagnostics' });
	await diagnosticsTrigger.hover();
	const diagnostics = page.locator('[data-web-rtc-diagnostics="north-garden"]');
	await expect(diagnostics).toBeVisible();
	await expect(diagnostics.getByText('Stream FPS', { exact: true })).toBeVisible();
	await expect(diagnostics.getByText('Decoded FPS', { exact: true })).toBeVisible();
	await expect(diagnostics).toContainText('15.0');
	await expect(diagnostics).toContainText('640 × 360');
	await expect(diagnostics).toContainText('2 (0.02%)');
	await expect(diagnostics).toContainText('12');
	await expect(diagnostics).toContainText('1.5 Mbps');
	const receiveBitrate = diagnostics.locator('[data-web-rtc-metric="receive-bitrate"]');
	await expect(receiveBitrate).toHaveText('2.0 Mbps');
	await page.locator('[data-camera-id="north-garden"] video').dispatchEvent('resize');
	await page.evaluate(() => new Promise(requestAnimationFrame));
	expect(await receiveBitrate.textContent()).toBe('2.0 Mbps');
	await expect(diagnostics).toContainText('h264');
	await expect(diagnostics).toContainText('Mock decoder · HW');

	await diagnosticsTrigger.click();
	await page.mouse.move(0, 0);
	await expect(diagnostics).toBeVisible();
	await diagnosticsTrigger.press('Escape');
	await expect(diagnostics).toHaveCount(0);

	const cameraLink = page.getByRole('link', { name: 'Open camera information' });
	await expect(cameraLink).toHaveAttribute('href', '/camera?camera=north-garden');
	await page.getByRole('button', { name: 'Focus North Garden live view' }).click();
	await expect(page.getByRole('link', { name: 'Open camera information' })).toHaveCount(1);

	await page.goto('/?camera=north-garden');
	await expect(page.locator('section[aria-label="North Garden focus"]')).toBeVisible();
	await expect(page.getByRole('link', { name: 'Open camera information' })).toHaveCount(1);
	const historyLink = page.getByRole('link', { name: 'History' });
	await expect(historyLink).toHaveAttribute('href', '/keep?camera=north-garden&stream=main');
	await historyLink.click();
	await expect(page).toHaveURL(/\/keep\?camera=north-garden&stream=main/);
	await expect(page).toHaveTitle('Keep - KeepPeek');
});

test('shares one peer across camera tracks and changes focus quality out of band', async ({
	page
}) => {
	await page.addInitScript(() => {
		class MockPeerConnection {
			localDescription: RTCSessionDescriptionInit | null = null;
			iceGatheringState: RTCIceGatheringState = 'complete';
			connectionState: RTCPeerConnectionState = 'connected';
			iceConnectionState: RTCIceConnectionState = 'connected';
			ontrack: RTCPeerConnection['ontrack'] = null;
			onconnectionstatechange: RTCPeerConnection['onconnectionstatechange'] = null;
			oniceconnectionstatechange: RTCPeerConnection['oniceconnectionstatechange'] = null;
			private transceivers: RTCRtpTransceiver[] = [];
			private receiver = { getStats: async () => new Map() } as unknown as RTCRtpReceiver;

			constructor() {
				const scope = window as unknown as { peerCount?: number };
				scope.peerCount = (scope.peerCount ?? 0) + 1;
			}

			addTransceiver(): RTCRtpTransceiver {
				const transceiver = {
					mid: null,
					setCodecPreferences() {}
				} as unknown as RTCRtpTransceiver;
				this.transceivers.push(transceiver);
				return transceiver;
			}

			async createOffer(): Promise<RTCSessionDescriptionInit> {
				this.transceivers.forEach((transceiver, index) => {
					(transceiver as unknown as { mid: string }).mid = `${index}`;
				});
				return { type: 'offer', sdp: 'v=0\r\n' };
			}

			async setLocalDescription(description: RTCSessionDescriptionInit): Promise<void> {
				this.localDescription = description;
			}

			async setRemoteDescription(): Promise<void> {
				for (const transceiver of this.transceivers) {
					this.ontrack?.({
						receiver: this.receiver,
						streams: [new MediaStream()],
						transceiver
					} as unknown as RTCTrackEvent);
				}
			}

			close() {}
		}

		Object.defineProperty(window, 'RTCPeerConnection', { value: MockPeerConnection });
	});
	await page.route('**/health', async (route) => {
		await route.fulfill({
			json: {
				status: 'ok',
				cameras: [
					{ id: 'kitchen', state: 'online' },
					{ id: 'garden', state: 'online' }
				]
			}
		});
	});
	await page.route('**/api/cameras', async (route) => {
		await route.fulfill({
			json: [
				{
					id: 'kitchen',
					ip: '192.0.2.10',
					name: 'Kitchen',
					manufacturer: 'Reolink',
					model: 'RLC-820A',
					firmware_version: null,
					is_reolink: true,
					profiles: [
						{
							name: 'Main',
							stream: 'main',
							encoding: 'h265',
							resolution: '3840x2160',
							framerate: 25
						},
						{ name: 'Sub', stream: 'sub', encoding: 'h264', resolution: '640x360', framerate: 15 }
					]
				},
				{
					id: 'garden',
					ip: '192.0.2.11',
					name: 'Garden',
					manufacturer: 'Reolink',
					model: 'RLC-811A',
					firmware_version: null,
					is_reolink: true,
					profiles: [
						{
							name: 'Main',
							stream: 'main',
							encoding: 'h265',
							resolution: '3840x2160',
							framerate: 25
						},
						{ name: 'Sub', stream: 'sub', encoding: 'h264', resolution: '640x360', framerate: 15 }
					]
				}
			]
		});
	});
	let offeredTracks: Array<{ track_id: string; camera_id: string; mid: string; quality: string }> =
		[];
	let qualityRequests = 0;
	await page.route('**/api/live/browser/offer', async (route) => {
		offeredTracks = route.request().postDataJSON().tracks;
		await route.fulfill({
			json: {
				session_id: 1,
				answer: { type: 'answer', sdp: 'v=0\r\n' },
				estimated_bitrate_bps: 3_000_000,
				tracks: offeredTracks.map((track) => ({
					track_id: track.track_id,
					requested_quality: 'low',
					active_stream: 'sub',
					estimated_bitrate_bps: 3_000_000
				}))
			}
		});
	});
	await page.route('**/api/live/browser/1', async (route) => {
		await route.fulfill({
			json: {
				estimated_bitrate_bps: 3_000_000,
				tracks: offeredTracks.map((track) => ({
					track_id: track.track_id,
					requested_quality: 'low',
					active_stream: 'sub',
					estimated_bitrate_bps: 3_000_000
				}))
			}
		});
	});
	await page.route('**/api/live/browser/1/tracks/camera-1/quality', async (route) => {
		qualityRequests += 1;
		await route.fulfill({
			json: {
				track_id: 'camera-1',
				requested_quality: 'auto',
				active_stream: 'sub',
				estimated_bitrate_bps: 3_000_000
			}
		});
	});

	await page.goto('/');
	await expect
		.poll(() => page.evaluate(() => (window as unknown as { peerCount?: number }).peerCount))
		.toBe(1);
	await expect.poll(() => offeredTracks.length).toBe(2);
	expect(offeredTracks).toEqual([
		{ track_id: 'camera-0', camera_id: 'garden', mid: '0', quality: 'low' },
		{ track_id: 'camera-1', camera_id: 'kitchen', mid: '1', quality: 'low' }
	]);

	await page.getByRole('button', { name: 'Focus Kitchen live view' }).click({ force: true });
	await expect.poll(() => qualityRequests).toBe(1);
	await expect
		.poll(() => page.evaluate(() => (window as unknown as { peerCount?: number }).peerCount))
		.toBe(1);
});
