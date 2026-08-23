import type { CameraHealth, HealthIssue, ServerHealthResponse, StreamHealth } from '$lib/types';

type HealthPresentationSnapshot = Pick<ServerHealthResponse, 'cameras' | 'issues'>;

export type RankedHealthFinding = {
	issue: HealthIssue;
	camera: CameraHealth | null;
	priority: number;
};

export type CameraDiagnosisEvidence = {
	camera: CameraHealth;
	relatedIssues: HealthIssue[];
	latestStreamReportAtMs: number | null;
	reconnects: number | null;
	drops: number | null;
	errors: number | null;
	recordingGapStartMs: null;
	retryEvidence: null;
	credentialProbeAvailable: false;
	canSuggestTcp: boolean;
	reportingNormally: number;
	otherUnhealthyCameras: number;
};

const severityPriority: Record<HealthIssue['severity'], number> = {
	critical: 0,
	warning: 100,
	info: 200
};

export function rankHealthFindings(snapshot: HealthPresentationSnapshot): RankedHealthFinding[] {
	return snapshot.issues
		.map((issue, index) => {
			const matchedCamera = cameraForIssue(snapshot.cameras, issue);
			const camera = matchedCamera ? effectiveCameraHealth(matchedCamera, snapshot.issues) : null;
			return {
				issue,
				camera,
				priority: severityPriority[issue.severity] + impactPriority(camera, issue.scope),
				index
			};
		})
		.toSorted((left, right) => left.priority - right.priority || left.index - right.index)
		.map(({ issue, camera, priority }) => ({ issue, camera, priority }));
}

export function cameraDiagnosisEvidence(
	snapshot: HealthPresentationSnapshot,
	cameraId: string
): CameraDiagnosisEvidence | null {
	const cameras = snapshot.cameras.map((camera) => effectiveCameraHealth(camera, snapshot.issues));
	const camera = cameras.find((candidate) => candidate.id === cameraId) ?? null;
	if (camera === null) return null;

	return {
		camera,
		relatedIssues: snapshot.issues.filter(
			(issue) => cameraForIssue(snapshot.cameras, issue)?.id === camera.id
		),
		latestStreamReportAtMs:
			camera.streams.length === 0
				? null
				: Math.max(...camera.streams.map((stream) => stream.updated_at_ms)),
		reconnects: sumOptional(camera.streams, 'reconnects'),
		drops: sumOptional(camera.streams, 'drops'),
		errors: sumOptional(camera.streams, 'errors'),
		recordingGapStartMs: null,
		retryEvidence: null,
		credentialProbeAvailable: false,
		canSuggestTcp: camera.transport === 'udp',
		reportingNormally: cameras.filter((candidate) => candidate.state === 'online').length,
		otherUnhealthyCameras: cameras.filter(
			(candidate) =>
				candidate.id !== camera.id && candidate.state !== 'online' && candidate.state !== 'starting'
		).length
	};
}

export function reconcileServerHealth(snapshot: ServerHealthResponse): ServerHealthResponse {
	return {
		...snapshot,
		cameras: snapshot.cameras.map((camera) => effectiveCameraHealth(camera, snapshot.issues))
	};
}

export function effectiveCameraHealth(
	camera: CameraHealth,
	issues: readonly HealthIssue[]
): CameraHealth {
	if (camera.state !== 'online') return camera;
	const issue = issues.find(
		(candidate) =>
			candidate.severity !== 'info' && cameraForIssue([camera], candidate)?.id === camera.id
	);
	return issue
		? { ...camera, state: 'degraded', last_error: camera.last_error ?? issue.message }
		: camera;
}

function cameraForIssue(cameras: readonly CameraHealth[], issue: HealthIssue): CameraHealth | null {
	return (
		cameras.find(
			(camera) =>
				issue.scope === camera.id || issue.scope === camera.ip || issue.scope === camera.name
		) ?? null
	);
}

function impactPriority(camera: CameraHealth | null, scope: string): number {
	if (camera?.state === 'offline') return -50;
	if (camera?.state === 'stale') return -40;
	if (camera?.state === 'degraded') return -30;
	if (scope === 'storage') return -20;
	if (scope === 'runtime') return -10;
	return 0;
}

function sumOptional(
	streams: readonly StreamHealth[],
	field: 'drops' | 'errors' | 'reconnects'
): number | null {
	const values = streams.map((stream) => stream[field]).filter((value) => value !== undefined);
	return values.length === 0 ? null : values.reduce((sum, value) => sum + value, 0);
}
