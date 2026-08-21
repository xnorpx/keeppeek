import type { DemoNarration, StoryScenarioMetadata } from '../../storybook/demo';
import type { NarratedDemoPlan } from './demo-video';

export type DemoNarrationManifestCue = {
	index: number;
	sourceAtMs: number;
	pauseAfterMs: number;
	text: string;
	fileName: string;
	durationMs: number;
	bytes: number;
	sha256: string;
};

export type DemoNarrationManifest = {
	schemaVersion: 1;
	storyId: string;
	scenarioId: string;
	commitSha: string;
	generatedAt: string;
	deployment: string;
	voice: string;
	instructions?: string;
	speed?: number;
	cues: DemoNarrationManifestCue[];
};

function formatWebVttTime(timeMs: number): string {
	const hours = Math.floor(timeMs / 3_600_000);
	const minutes = Math.floor((timeMs % 3_600_000) / 60_000);
	const seconds = Math.floor((timeMs % 60_000) / 1_000);
	const milliseconds = timeMs % 1_000;
	return `${hours.toString().padStart(2, '0')}:${minutes
		.toString()
		.padStart(2, '0')}:${seconds.toString().padStart(2, '0')}.${milliseconds
		.toString()
		.padStart(3, '0')}`;
}

export function assertDemoNarrationManifest(
	metadata: StoryScenarioMetadata,
	manifest: DemoNarrationManifest
): void {
	const narration = metadata.demo?.narration;
	if (!narration) throw new Error(`Scenario ${metadata.storyId} has no narration`);
	if (manifest.schemaVersion !== 1) throw new Error('Unsupported narration manifest schema');
	if (manifest.storyId !== metadata.storyId) throw new Error('Narration story ID does not match');
	if (manifest.scenarioId !== metadata.paper.scenarioId) {
		throw new Error('Narration scenario ID does not match');
	}
	if (manifest.deployment.trim().length === 0) throw new Error('Narration deployment is empty');
	if (
		manifest.voice !== narration.voice ||
		manifest.instructions !== narration.instructions ||
		manifest.speed !== narration.speed
	) {
		throw new Error('Narration voice settings do not match story metadata');
	}
	if (manifest.cues.length !== narration.cues.length) {
		throw new Error('Narration cue count does not match story metadata');
	}

	for (const [index, cue] of manifest.cues.entries()) {
		const source = narration.cues[index]!;
		if (
			cue.index !== index ||
			cue.sourceAtMs !== source.atMs ||
			cue.pauseAfterMs !== (source.pauseAfterMs ?? 0) ||
			cue.text !== source.text
		) {
			throw new Error(`Narration cue ${index} does not match story metadata`);
		}
		if (
			cue.fileName.includes('/') ||
			cue.fileName.includes('\\') ||
			!cue.fileName.endsWith('.wav')
		) {
			throw new Error(`Narration cue ${index} has an invalid WAV filename`);
		}
		if (!Number.isInteger(cue.durationMs) || cue.durationMs <= 0 || cue.bytes <= 0) {
			throw new Error(`Narration cue ${index} has invalid media measurements`);
		}
		if (!/^[a-f0-9]{64}$/.test(cue.sha256)) {
			throw new Error(`Narration cue ${index} has an invalid SHA-256 hash`);
		}
	}
}

export function createNarratedDemoWebVtt(narration: DemoNarration, plan: NarratedDemoPlan): string {
	if (narration.cues.length !== plan.segments.length) {
		throw new Error('Narration cues and paced video segments must have the same length');
	}
	const cues = narration.cues.map((cue, index) => {
		const segment = plan.segments[index]!;
		const endMs = segment.outputStartMs + segment.audioDurationMs;
		return `${index + 1}\n${formatWebVttTime(segment.outputStartMs)} --> ${formatWebVttTime(endMs)}\n${cue.text}`;
	});
	return `WEBVTT\n\n${cues.join('\n\n')}\n`;
}
