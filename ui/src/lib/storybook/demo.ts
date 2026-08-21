export type PaperStoryMetadata = {
	fileId: string;
	tokenHash: string;
	boardId: string;
	frameId: string;
	scenarioId: string;
};

export type DemoViewport = {
	width: number;
	height: number;
};

export type DemoCaption = {
	atMs: number;
	endMs?: number;
	text: string;
};

export type DemoNarrationCue = {
	atMs: number;
	text: string;
	pauseAfterMs?: number;
};

export type DemoNarration = {
	voice: string;
	instructions?: string;
	speed?: number;
	cues: readonly DemoNarrationCue[];
};

export type DemoNarrationSpeech = Omit<DemoNarration, 'cues'> & { text: string };

export type DemoAction =
	| {
			kind: 'click';
			atMs: number;
			selector: string;
	  }
	| {
			kind: 'press';
			atMs: number;
			key: string;
			selector?: string;
	  }
	| {
			kind: 'pointer-drag';
			atMs: number;
			selector: string;
			deltaX: number;
			deltaY: number;
			durationMs: number;
			holdAfterMs?: number;
			steps?: number;
	  };

export type DemoCompletionSignal = {
	selector: string;
	state: 'attached' | 'detached' | 'hidden' | 'visible';
};

export type DemoMetadata = {
	title: string;
	purpose: string;
	narration?: DemoNarration;
	durationMs: number;
	viewport: DemoViewport;
	captions: readonly DemoCaption[];
	actions: readonly DemoAction[];
	completionSignal: DemoCompletionSignal;
};

export type StoryScenarioMetadata = {
	storyId: string;
	paper: PaperStoryMetadata;
	demo?: DemoMetadata;
};

export type DemoScenarioDefinition = {
	metadata: StoryScenarioMetadata;
	previewScenarioId: string;
	storySource: string;
	fixtureSources: readonly string[];
};

export type MetadataValidationIssue = {
	path: string;
	message: string;
};

const storyIdPattern = /^[a-z0-9]+(?:(?:--|[.-])[a-z0-9]+)*$/;
const tokenHashPattern = /^[a-f0-9]{8,64}$/;

function requireText(
	issues: MetadataValidationIssue[],
	path: string,
	value: string | undefined
): void {
	if (value === undefined || value.trim().length === 0) {
		issues.push({ path, message: 'must not be empty' });
	}
}

function requirePositiveInteger(
	issues: MetadataValidationIssue[],
	path: string,
	value: number
): void {
	if (!Number.isInteger(value) || value <= 0) {
		issues.push({ path, message: 'must be a positive integer' });
	}
}

export function validateStoryScenarioMetadata(
	metadata: StoryScenarioMetadata
): MetadataValidationIssue[] {
	const issues: MetadataValidationIssue[] = [];

	requireText(issues, 'storyId', metadata.storyId);
	if (!storyIdPattern.test(metadata.storyId)) {
		issues.push({ path: 'storyId', message: 'must be a stable lowercase identifier' });
	}

	requireText(issues, 'paper.fileId', metadata.paper.fileId);
	requireText(issues, 'paper.boardId', metadata.paper.boardId);
	requireText(issues, 'paper.frameId', metadata.paper.frameId);
	requireText(issues, 'paper.scenarioId', metadata.paper.scenarioId);
	if (!storyIdPattern.test(metadata.paper.scenarioId)) {
		issues.push({
			path: 'paper.scenarioId',
			message: 'must be a stable lowercase identifier'
		});
	}
	if (!tokenHashPattern.test(metadata.paper.tokenHash)) {
		issues.push({ path: 'paper.tokenHash', message: 'must be a lowercase hexadecimal hash' });
	}

	const demo = metadata.demo;
	if (demo === undefined) return issues;

	requireText(issues, 'demo.title', demo.title);
	requireText(issues, 'demo.purpose', demo.purpose);
	requirePositiveInteger(issues, 'demo.durationMs', demo.durationMs);
	requirePositiveInteger(issues, 'demo.viewport.width', demo.viewport.width);
	requirePositiveInteger(issues, 'demo.viewport.height', demo.viewport.height);
	if (demo.narration !== undefined) {
		requireText(issues, 'demo.narration.voice', demo.narration.voice);
		if (demo.narration.instructions !== undefined) {
			requireText(issues, 'demo.narration.instructions', demo.narration.instructions);
		}
		if (
			demo.narration.speed !== undefined &&
			(demo.narration.speed < 0.25 || demo.narration.speed > 4)
		) {
			issues.push({ path: 'demo.narration.speed', message: 'must be between 0.25 and 4' });
		}
		if (demo.narration.cues.length === 0) {
			issues.push({ path: 'demo.narration.cues', message: 'must contain at least one cue' });
		}
		let previousCueAtMs = -1;
		for (const [index, cue] of demo.narration.cues.entries()) {
			const path = `demo.narration.cues[${index}]`;
			requireText(issues, `${path}.text`, cue.text);
			if (!Number.isInteger(cue.atMs) || cue.atMs < 0 || cue.atMs >= demo.durationMs) {
				issues.push({ path: `${path}.atMs`, message: 'must occur within the demo duration' });
			}
			if (index === 0 && cue.atMs !== 0) {
				issues.push({ path: `${path}.atMs`, message: 'must start at source time zero' });
			}
			if (cue.atMs <= previousCueAtMs) {
				issues.push({ path: `${path}.atMs`, message: 'must be later than the previous cue' });
			}
			if (
				cue.pauseAfterMs !== undefined &&
				(!Number.isInteger(cue.pauseAfterMs) || cue.pauseAfterMs < 0)
			) {
				issues.push({ path: `${path}.pauseAfterMs`, message: 'must be a non-negative integer' });
			}
			previousCueAtMs = cue.atMs;
		}
	}

	if (demo.captions.length === 0) {
		issues.push({ path: 'demo.captions', message: 'must contain at least one caption' });
	}

	if (demo.actions.length === 0) {
		issues.push({ path: 'demo.actions', message: 'must contain at least one action' });
	}
	requireText(issues, 'demo.completionSignal.selector', demo.completionSignal.selector);

	let previousActionEndMs = -1;
	for (const [index, action] of demo.actions.entries()) {
		const path = `demo.actions[${index}]`;
		if (!Number.isInteger(action.atMs) || action.atMs < 0 || action.atMs >= demo.durationMs) {
			issues.push({ path: `${path}.atMs`, message: 'must occur within the demo duration' });
		}
		if (action.atMs < previousActionEndMs) {
			issues.push({ path: `${path}.atMs`, message: 'must not overlap the previous action' });
		}

		let actionEndMs = action.atMs;
		if (action.kind === 'click') {
			requireText(issues, `${path}.selector`, action.selector);
		} else if (action.kind === 'press') {
			requireText(issues, `${path}.key`, action.key);
			if (action.selector !== undefined) requireText(issues, `${path}.selector`, action.selector);
		} else {
			requireText(issues, `${path}.selector`, action.selector);
			requirePositiveInteger(issues, `${path}.durationMs`, action.durationMs);
			if (action.holdAfterMs !== undefined) {
				requirePositiveInteger(issues, `${path}.holdAfterMs`, action.holdAfterMs);
			}
			if (action.steps !== undefined) requirePositiveInteger(issues, `${path}.steps`, action.steps);
			actionEndMs = action.atMs + action.durationMs + (action.holdAfterMs ?? 0);
			if (actionEndMs > demo.durationMs) {
				issues.push({ path, message: 'must finish within the demo duration' });
			}
		}
		previousActionEndMs = actionEndMs;
	}

	let previousAtMs = -1;
	for (const [index, caption] of demo.captions.entries()) {
		const path = `demo.captions[${index}]`;
		requireText(issues, `${path}.text`, caption.text);

		if (!Number.isInteger(caption.atMs) || caption.atMs < 0 || caption.atMs >= demo.durationMs) {
			issues.push({ path: `${path}.atMs`, message: 'must occur within the demo duration' });
		}
		if (caption.atMs <= previousAtMs) {
			issues.push({ path: `${path}.atMs`, message: 'must be later than the previous caption' });
		}
		if (
			caption.endMs !== undefined &&
			(!Number.isInteger(caption.endMs) ||
				caption.endMs <= caption.atMs ||
				caption.endMs > demo.durationMs)
		) {
			issues.push({ path: `${path}.endMs`, message: 'must end after the caption starts' });
		}

		previousAtMs = caption.atMs;
	}

	return issues;
}

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

export function createDemoWebVtt(metadata: DemoMetadata): string {
	const issues = validateStoryScenarioMetadata({
		storyId: 'demo.validation',
		paper: {
			fileId: 'validation',
			tokenHash: '00000000',
			boardId: 'validation',
			frameId: 'validation',
			scenarioId: 'demo.validation'
		},
		demo: metadata
	});
	if (issues.length > 0) {
		throw new Error(issues.map((issue) => `${issue.path} ${issue.message}`).join('; '));
	}

	const cues = metadata.captions.map((caption, index) => {
		const nextCaption = metadata.captions[index + 1];
		const endMs = caption.endMs ?? nextCaption?.atMs ?? metadata.durationMs;
		return `${index + 1}\n${formatWebVttTime(caption.atMs)} --> ${formatWebVttTime(endMs)}\n${caption.text}`;
	});

	return `WEBVTT\n\n${cues.join('\n\n')}\n`;
}
