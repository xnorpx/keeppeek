import { execFile } from 'node:child_process';
import { promisify } from 'node:util';

export type CalibrationRecording = {
	source_id: string;
	stream_id: string;
	recording_stream_id: string;
	start_ms: number;
	end_ms: number;
	tone_lead_ms: number;
	periods: { recording_id: string; start_ms: number; end_ms: number; fragments: number }[];
};

export async function seedCalibrationRecording(
	binary: string,
	directory: string,
	configPath: string,
	sourceId: string,
	mediaPath: string
): Promise<CalibrationRecording> {
	const { stdout } = await promisify(execFile)(
		binary,
		[
			'seed-recording',
			'--calibration',
			'--source',
			mediaPath,
			'--config',
			configPath,
			'--stream-id',
			`${sourceId}/sub`,
			'--age-seconds',
			'120'
		],
		{ cwd: directory, windowsHide: true, timeout: 30_000, maxBuffer: 16_384 }
	);
	const result: CalibrationRecording = JSON.parse(stdout);
	if (
		result.source_id !== sourceId ||
		result.stream_id !== 'sub' ||
		result.recording_stream_id !== `${sourceId}/sub` ||
		result.tone_lead_ms !== 200 ||
		!Number.isSafeInteger(result.start_ms) ||
		!Number.isSafeInteger(result.end_ms) ||
		result.end_ms <= result.start_ms ||
		result.end_ms - result.start_ms > 60_000 ||
		!Array.isArray(result.periods) ||
		result.periods.length !== 2
	)
		throw new Error('The calibration seed returned an invalid bounded manifest.');
	for (const period of result.periods) {
		if (
			!/^[0-9a-f-]{36}$/.test(period.recording_id) ||
			period.fragments !== 2 ||
			!Number.isSafeInteger(period.start_ms) ||
			!Number.isSafeInteger(period.end_ms) ||
			period.start_ms < result.start_ms ||
			period.end_ms > result.end_ms ||
			period.end_ms <= period.start_ms
		)
			throw new Error('The calibration seed returned an invalid recording period.');
	}
	return result;
}
