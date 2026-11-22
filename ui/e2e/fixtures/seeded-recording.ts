import { expect, type Page } from '@playwright/test';
import type { RecordingCoverageResponse } from '../../src/lib/types';

export async function openSeekableRecording(page: Page) {
	const response = await page.request.get('/recording-coverage?search=e2e-h264&stream=sub');
	await expect(response).toBeOK();
	const coverage: RecordingCoverageResponse = await response.json();
	const camera = coverage.cameras.find((entry) => entry.camera_name === 'e2e-h264');
	const stream = camera?.streams.find((entry) => entry.stream_id === 'sub');
	// The live edge can end after a single frame. Use the seeded substream for playback.
	const recording = stream?.ranges.find((range) => range.end_ms - range.start_ms >= 10_000);
	if (!camera || !recording) throw new Error('The fixture needs ten seconds of recorded video.');
	const at = recording.start_ms + 1_000;
	const date = new Date(at).toISOString().slice(0, 10);
	await page.goto(
		`/keep?camera=${encodeURIComponent(camera.camera_id)}&stream=sub&at=${at}&date=${date}`
	);
}
