import { collectReportDiagnosticsBundle } from './diagnostics-bundle';
import type { BrowserLogStore } from './browser-logs';
import type { ControlClient } from './control-client';
import { startReportCapture } from './report-capture';
import {
	reportLogWindow,
	type ReportLogCheckpoint,
	type ReportLogWindow
} from './report-log-window';
import type { BrowserLogEntry, LogSnapshot } from './types';

const reportLimitMs = 5 * 60_000;
type Capture = Awaited<ReturnType<typeof startReportCapture>>;
type Position = { cursor: ReportLogCheckpoint; server: LogSnapshot | null };
type Session = {
	media: AbortController;
	start: Promise<Position>;
	timer: ReturnType<typeof setTimeout>;
	capture?: Capture;
	gap: boolean;
};

type Dependencies = {
	snapshot(cursor: Pick<ReportLogCheckpoint, 'utcMs' | 'browserSequence'>): Promise<Position>;
	latestBrowserSequence(): number;
	browserEntries(): BrowserLogEntry[];
	package(server: LogSnapshot | null, report: ReportLogWindow): Promise<Blob>;
	capture: typeof startReportCapture;
	now(): number;
};

export function createReportSession(
	control: ControlClient,
	browser: BrowserLogStore
): ReportSession {
	return new ReportSession({
		now: Date.now,
		capture: startReportCapture,
		browserEntries: () => browser.snapshot(),
		latestBrowserSequence: () => browser.latestSequence,
		package: (snapshot, window) => collectReportDiagnosticsBundle(control, snapshot, window),
		async snapshot(position) {
			const [logs, settings] = await Promise.allSettled([
				control.getLogSnapshot(),
				control.getLoggingSettings()
			]);
			const server = logs.status === 'fulfilled' ? logs.value : null;
			return {
				server,
				cursor: {
					...position,
					server: server
						? {
								sequence: server.newest_sequence ?? 0,
								filter: settings.status === 'fulfilled' ? settings.value.active_filter : undefined
							}
						: null
				}
			};
		}
	});
}

type Phase = 'idle' | 'recording' | 'finishing' | 'review';
type CaptureState = 'logs-only' | 'requesting' | 'capturing' | 'unavailable';
type Review = {
	window: ReportLogWindow['manifest'];
	diagnostics: Blob | null;
	recording: Blob | null;
	stopReason: string;
	diagnosticsUnavailable: boolean;
};

export class ReportSession {
	phase: Phase = $state('idle');
	mediaState: CaptureState = $state('logs-only');
	startedAt = $state(0);
	bytes = $state(0);
	review: Review | null = $state.raw(null);
	#session: Session | null = null;

	constructor(private readonly deps: Dependencies) {}

	start(withDisplay: boolean): void {
		if (this.phase !== 'idle') return;
		this.startedAt = this.deps.now();
		this.phase = 'recording';
		this.mediaState = withDisplay ? 'requesting' : 'logs-only';
		const media = new AbortController();
		const session: Session = {
			media,
			start: this.snapshot(this.startedAt),
			timer: setTimeout(() => {
				void this.stop('duration-limit');
			}, reportLimitMs),
			gap: false
		};
		this.#session = session;
		if (!withDisplay) return;

		void this.deps
			.capture({
				signal: media.signal,
				onSize: (bytes) => {
					if (this.#session === session) this.bytes = bytes;
				}
			})
			.then(
				(capture) => {
					if (this.#session !== session || this.phase !== 'recording') return;
					session.capture = capture;
					this.mediaState = 'capturing';
					void capture.result.then((result) => {
						if (this.#session !== session || this.phase !== 'recording') return;
						if (result.reason === 'error') this.mediaState = 'unavailable';
						else void this.stop(result.reason);
					});
				},
				() => {
					if (this.#session === session && this.phase === 'recording')
						this.mediaState = 'unavailable';
				}
			);
	}

	private snapshot(utcMs: number): Promise<Position> {
		const position = { utcMs, browserSequence: this.deps.latestBrowserSequence() };
		return this.deps.snapshot(position).catch(() => ({
			cursor: { ...position, server: null },
			server: null
		}));
	}

	recordServerLogGap(): void {
		if (this.#session) this.#session.gap = true;
	}

	async stop(reason = 'stopped'): Promise<void> {
		const session = this.#session;
		if (!session || this.phase !== 'recording') return;
		this.phase = 'finishing';
		clearTimeout(session.timer);
		const media = session.capture?.stop();
		if (!media) session.media.abort();
		const stoppedAt = this.deps.now();
		const browserEntries = this.deps.browserEntries();
		const [start, stop, captured] = await Promise.all([
			session.start,
			this.snapshot(stoppedAt),
			media
		]);
		if (this.#session !== session) return;
		const window = reportLogWindow(
			start.cursor,
			stop.cursor,
			browserEntries,
			stop.server,
			session.gap
		);
		const diagnostics = await this.deps.package(stop.server, window).catch(() => null);
		if (this.#session !== session) return;
		this.review = {
			window: window.manifest,
			diagnostics,
			recording: captured?.recording ?? null,
			stopReason: captured && captured.reason !== 'stopped' ? captured.reason : reason,
			diagnosticsUnavailable: diagnostics === null
		};
		this.#session = null;
		this.phase = 'review';
	}

	discardRecording(): void {
		if (this.review) this.review = { ...this.review, recording: null };
	}

	cancel(): void {
		if (this.#session) {
			clearTimeout(this.#session.timer);
			this.#session.media.abort();
		}
		this.#session = null;
		this.review = null;
		this.bytes = 0;
		this.startedAt = 0;
		this.mediaState = 'logs-only';
		this.phase = 'idle';
	}
}
