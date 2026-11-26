/**
 * Reproducible desktop smoke and performance harness for KeepPeek Desktop.
 *
 * Compares the existing browser UI (Playwright Chromium) against the release
 * Tauri desktop binary (`keeppeek-desktop`) using the same local KeepPeek
 * server and `test_camera` H.264 RTSP fixture.
 */
import { existsSync } from 'node:fs';
import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { arch, cpus, platform, release, tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

type DiagnosticsReport = {
	os: string;
	arch: string;
	configured_origin: string;
	active_origin: string | null;
	launcher_loaded_ms: number | null;
	probe_finished_ms: number | null;
	server_loaded_ms: number | null;
	probe_ok: boolean;
	probe_error: string | null;
	blocked_navigation_count: number;
};

type TreeSample = {
	processCount: number;
	memoryMib: number;
	cpuSeconds: number;
};

type SteadyStats = {
	warmupSecs: number;
	durationSecs: number;
	samples: number;
	meanCpuPercent: number;
	medianMemoryMib: number;
	p95MemoryMib: number;
	maxProcessCount: number;
	webrtcSessionObserved: boolean;
};

type PlaywrightChromium = {
	launch: (options: { headless: boolean }) => Promise<{
		version: () => string;
		newPage: () => Promise<{
			goto: (url: string, options: { waitUntil: string }) => Promise<unknown>;
			waitForSelector: (selector: string, options: { timeout: number }) => Promise<unknown>;
			waitForFunction: (
				fn: () => boolean,
				arg: unknown,
				options: { timeout: number }
			) => Promise<unknown>;
			close: () => Promise<void>;
		}>;
		close: () => Promise<void>;
	}>;
	launchPersistentContext: (
		userDataDir: string,
		options: { headless: boolean }
	) => Promise<{
		pages: () => Array<{
			goto: (url: string, options: { waitUntil: string }) => Promise<unknown>;
			waitForFunction: (
				fn: () => boolean,
				arg: unknown,
				options: { timeout: number }
			) => Promise<unknown>;
		}>;
		newPage: () => Promise<{
			goto: (url: string, options: { waitUntil: string }) => Promise<unknown>;
			waitForFunction: (
				fn: () => boolean,
				arg: unknown,
				options: { timeout: number }
			) => Promise<unknown>;
		}>;
		close: () => Promise<void>;
	}>;
};

type HarnessPaths = {
	repositoryRoot: string;
	keeppeekBin: string;
	testCameraBin: string;
	desktopBin: string;
	sampleVideo: string;
	runRoot: string;
};

await main();

async function main(): Promise<void> {
	if (process.platform !== 'win32' && process.platform !== 'darwin') {
		throw new Error(
			`KeepPeek Desktop benchmark supports only Windows and macOS (got ${process.platform}).`
		);
	}
	const args = parseArgs(process.argv.slice(2));
	const paths = resolvePaths();
	await mkdir(paths.runRoot, { recursive: true });

	const serverPort = await findFreePort();
	const closedPort = await findFreePort();
	const serverUrl = `http://127.0.0.1:${serverPort}`;
	const fixture = await startFixtureServer(paths, serverPort);

	try {
		await waitForServerReady(serverUrl, 30_000);
		const smoke = await runSmokeChecks(paths, serverUrl, closedPort);
		const playwrightModule = (await import(
			path.join(paths.repositoryRoot, 'ui', 'node_modules', 'playwright', 'index.mjs')
		)) as { chromium: PlaywrightChromium };

		const browserLaunches = await runBrowserLaunches(
			playwrightModule.chromium,
			serverUrl,
			args.launches
		);
		const desktopLaunches = await runDesktopLaunches(paths, serverUrl, args.launches);
		const browserSteady = await measureBrowserSteady(
			playwrightModule.chromium,
			paths.runRoot,
			serverUrl,
			args.warmupSecs,
			args.steadySecs
		);
		const desktopSteady = await measureDesktopSteady(
			paths,
			serverUrl,
			args.warmupSecs,
			args.steadySecs
		);
		await assertServerStillAlive(serverUrl);

		const result = {
			environment: {
				os: `${platform()} ${release()}`,
				arch: arch(),
				cpuModel: cpus()[0]?.model ?? 'unknown',
				logicalCores: cpus().length,
				chromiumVersion: browserLaunches.chromiumVersion,
				webviewVersion: detectWebViewVersion()
			},
			workload: {
				launches: args.launches,
				warmupSecs: args.warmupSecs,
				steadySecs: args.steadySecs,
				fixture: 'cc-4k-640x360-h264.mp4 (1 camera via test_camera RTSP)'
			},
			smoke,
			browser: {
				coldLaunchMs: summarize(browserLaunches.launchMs),
				firstLiveFrameMs: summarize(browserLaunches.firstFrameMs),
				steady: browserSteady
			},
			desktop: {
				coldLaunchMs: summarize(desktopLaunches.launchMs),
				launcherReadyMs: summarize(desktopLaunches.launcherMs),
				steady: desktopSteady
			}
		};

		const summaryText = JSON.stringify(result, null, 2);
		if (args.output) await writeFile(args.output, `${summaryText}\n`);
		console.log(summaryText);
	} finally {
		fixture.server.kill();
		fixture.camera.kill();
		await Promise.allSettled([fixture.server.exited, fixture.camera.exited]);
		await rm(paths.runRoot, { recursive: true, force: true });
	}
}

function resolvePaths(): HarnessPaths {
	const scriptDir = path.dirname(fileURLToPath(import.meta.url));
	const repositoryRoot = path.resolve(scriptDir, '../../..');
	const executableExt = process.platform === 'win32' ? '.exe' : '';
	const targetRoot = process.env.CARGO_TARGET_DIR
		? path.resolve(process.env.CARGO_TARGET_DIR)
		: path.join(repositoryRoot, 'target');
	const releaseDir = path.join(targetRoot, 'release');

	return {
		repositoryRoot,
		keeppeekBin: requireFile(path.join(releaseDir, `keeppeek${executableExt}`)),
		testCameraBin: requireFile(path.join(releaseDir, `test_camera${executableExt}`)),
		desktopBin: requireFile(path.join(releaseDir, `keeppeek-desktop${executableExt}`)),
		sampleVideo: requireFile(
			path.join(repositoryRoot, 'crates', 'test-camera', 'testdata', 'cc-4k-640x360-h264.mp4')
		),
		runRoot: path.join(tmpdir(), `keeppeek-desktop-bench-${Date.now()}`)
	};
}

async function startFixtureServer(
	paths: HarnessPaths,
	serverPort: number
): Promise<{ camera: ReturnType<typeof Bun.spawn>; server: ReturnType<typeof Bun.spawn> }> {
	const camera = await startTestCamera(paths, 'bench-h264');
	const configPath = path.join(paths.runRoot, 'config.toml');
	await writeFile(
		configPath,
		`host = "127.0.0.1"
port = ${serverPort}

[access]
require_secure_remote = false

[storage]
event_thumbnail_max_mb = 16
short_term_secs = 5
medium_term_secs = 60
flush_interval_secs = 1
write_buffer_bytes = 8192
long_term_max_gb = 0

${camera.config}
`
	);

	const server = Bun.spawn([paths.keeppeekBin, '--tui', 'never', `--config=${configPath}`], {
		cwd: paths.repositoryRoot,
		stdout: 'ignore',
		stderr: 'pipe'
	});
	return { camera: camera.process, server };
}

async function runSmokeChecks(
	paths: HarnessPaths,
	serverUrl: string,
	closedPort: number
): Promise<{
	unavailableServerRejected: boolean;
	connectedServerOrigin: string;
	serverSurvivedDesktopExit: boolean;
}> {
	const unavailableDiag = await runDesktopSmoke(
		paths,
		`http://127.0.0.1:${closedPort}`,
		path.join(paths.runRoot, 'diag-unavailable.json'),
		100
	);
	if (unavailableDiag.probe_ok || !unavailableDiag.probe_error?.includes('Retry')) {
		throw new Error(
			`Expected unavailable server failure with Retry guidance, got: ${JSON.stringify(unavailableDiag)}`
		);
	}

	const connectedDiag = await runDesktopSmoke(
		paths,
		serverUrl,
		path.join(paths.runRoot, 'diag-connected.json'),
		150
	);
	if (
		!connectedDiag.probe_ok ||
		connectedDiag.active_origin !== serverUrl ||
		connectedDiag.server_loaded_ms === null ||
		connectedDiag.blocked_navigation_count !== 0
	) {
		throw new Error(`Connected smoke check failed: ${JSON.stringify(connectedDiag)}`);
	}
	await assertServerStillAlive(serverUrl);
	return {
		unavailableServerRejected: true,
		connectedServerOrigin: connectedDiag.active_origin,
		serverSurvivedDesktopExit: true
	};
}

async function runBrowserLaunches(
	chromium: PlaywrightChromium,
	serverUrl: string,
	count: number
): Promise<{ launchMs: number[]; firstFrameMs: number[]; chromiumVersion: string }> {
	const launchMs: number[] = [];
	const firstFrameMs: number[] = [];
	let chromiumVersion = 'unknown';

	for (let run = 0; run < count; run += 1) {
		const started = performance.now();
		const browser = await chromium.launch({ headless: true });
		chromiumVersion = browser.version();
		try {
			const page = await browser.newPage();
			await page.goto(serverUrl, { waitUntil: 'load' });
			await page.waitForSelector('[data-peek-wall], [data-shell-rail]', { timeout: 20_000 });
			launchMs.push(Math.round(performance.now() - started));

			await page.waitForFunction(
				() => {
					const video = document.querySelector('video');
					if (!video) return false;
					const quality = video.getVideoPlaybackQuality?.();
					return (
						video.videoWidth > 0 &&
						(quality ? quality.totalVideoFrames > 0 : video.readyState >= 2)
					);
				},
				undefined,
				{ timeout: 20_000 }
			);
			firstFrameMs.push(Math.round(performance.now() - started));
		} finally {
			await browser.close();
		}
	}
	return { launchMs, firstFrameMs, chromiumVersion };
}

async function runDesktopLaunches(
	paths: HarnessPaths,
	serverUrl: string,
	count: number
): Promise<{ launchMs: number[]; launcherMs: number[] }> {
	const launchMs: number[] = [];
	const launcherMs: number[] = [];
	for (let run = 0; run < count; run += 1) {
		const diag = await runDesktopSmoke(
			paths,
			serverUrl,
			path.join(paths.runRoot, `diag-launch-${run}.json`),
			50
		);
		if (!diag.probe_ok || diag.server_loaded_ms === null) {
			throw new Error(`Desktop launch ${run} failed: ${JSON.stringify(diag)}`);
		}
		launchMs.push(diag.server_loaded_ms);
		if (diag.launcher_loaded_ms !== null) launcherMs.push(diag.launcher_loaded_ms);
	}
	return { launchMs, launcherMs };
}

function parseArgs(argv: string[]): {
	launches: number;
	steadySecs: number;
	warmupSecs: number;
	output: string | null;
} {
	let launchesValue = 10;
	let steadyValue = 300;
	let warmupValue = 10;
	let outputValue: string | null = null;
	for (let index = 0; index < argv.length; index += 1) {
		const flag = argv[index];
		const next = argv[index + 1];
		if (flag === '--launches' && next) {
			launchesValue = Number(next);
			index += 1;
		} else if (flag === '--steady-secs' && next) {
			steadyValue = Number(next);
			index += 1;
		} else if (flag === '--warmup-secs' && next) {
			warmupValue = Number(next);
			index += 1;
		} else if (flag === '--output' && next) {
			outputValue = next;
			index += 1;
		}
	}
	if (!Number.isInteger(launchesValue) || launchesValue < 1 || launchesValue > 100) {
		throw new Error('--launches must be an integer between 1 and 100');
	}
	if (!Number.isInteger(steadyValue) || steadyValue < 5 || steadyValue > 3600) {
		throw new Error('--steady-secs must be an integer between 5 and 3600');
	}
	if (!Number.isInteger(warmupValue) || warmupValue < 1 || warmupValue > 120) {
		throw new Error('--warmup-secs must be an integer between 1 and 120');
	}
	return {
		launches: launchesValue,
		steadySecs: steadyValue,
		warmupSecs: warmupValue,
		output: outputValue
	};
}

function requireFile(filePath: string): string {
	if (!existsSync(filePath)) {
		throw new Error(`Required file not found: ${filePath}`);
	}
	return filePath;
}

function findFreePort(): Promise<number> {
	return new Promise((resolvePort, rejectPort) => {
		const probe = createServer();
		probe.once('error', rejectPort);
		probe.listen(0, '127.0.0.1', () => {
			const address = probe.address();
			const port = typeof address === 'object' && address ? address.port : 0;
			probe.close((closeError) => {
				if (closeError) rejectPort(closeError);
				else resolvePort(port);
			});
		});
	});
}

async function startTestCamera(
	paths: HarnessPaths,
	name: string
): Promise<{ process: ReturnType<typeof Bun.spawn>; config: string }> {
	const proc = Bun.spawn(
		[
			paths.testCameraBin,
			'rtsp',
			'--main',
			paths.sampleVideo,
			'--sub',
			paths.sampleVideo,
			'--name',
			name,
			'--start-at-seconds',
			'0'
		],
		{ cwd: paths.repositoryRoot, stdout: 'pipe', stderr: 'ignore' }
	);
	const reader = proc.stdout.getReader();
	const decoder = new TextDecoder();
	const marker = 'transport = "tcp"\n';
	let output = '';
	while (true) {
		const { done, value } = await reader.read();
		if (value) output += decoder.decode(value, { stream: true });
		const end = output.indexOf(marker);
		if (end !== -1) {
			reader.releaseLock();
			return { process: proc, config: output.slice(0, end + marker.length) };
		}
		if (done) break;
	}
	throw new Error('test_camera exited before printing configuration');
}

async function waitForServerReady(url: string, timeoutMs: number): Promise<void> {
	const deadline = Date.now() + timeoutMs;
	while (Date.now() < deadline) {
		try {
			const res = await fetch(`${url}/auth/session`);
			if (res.ok) return;
		} catch {
			// Server is still starting.
		}
		await Bun.sleep(200);
	}
	throw new Error(`KeepPeek server did not become ready at ${url}`);
}

async function assertServerStillAlive(url: string): Promise<void> {
	const res = await fetch(`${url}/auth/session`);
	if (!res.ok) {
		throw new Error(`Expected KeepPeek server to remain healthy, got HTTP ${res.status}`);
	}
}

async function runDesktopSmoke(
	paths: HarnessPaths,
	url: string,
	diagnosticsPath: string,
	smokeExitMs: number
): Promise<DiagnosticsReport> {
	const proc = Bun.spawn(
		[
			paths.desktopBin,
			'--config',
			path.join(paths.runRoot, 'desktop-client-config.toml'),
			'--server-url',
			url,
			'--diagnostics-file',
			diagnosticsPath,
			'--smoke-exit-ms',
			String(smokeExitMs)
		],
		{ cwd: paths.repositoryRoot, stdout: 'ignore', stderr: 'pipe' }
	);
	const exitCode = await proc.exited;
	if (exitCode !== 0) {
		const stderr = await new Response(proc.stderr).text();
		throw new Error(`keeppeek-desktop exited with ${exitCode}: ${stderr}`);
	}
	return JSON.parse(await readFile(diagnosticsPath, 'utf8')) as DiagnosticsReport;
}

async function measureBrowserSteady(
	chromium: PlaywrightChromium,
	runRoot: string,
	url: string,
	warmupSeconds: number,
	steadySeconds: number
): Promise<SteadyStats> {
	const profileDir = path.join(runRoot, 'chromium-steady-profile');
	await mkdir(profileDir, { recursive: true });
	const context = await chromium.launchPersistentContext(profileDir, { headless: true });
	try {
		const page = context.pages()[0] ?? (await context.newPage());
		await page.goto(url, { waitUntil: 'load' });
		await page.waitForFunction(
			() => {
				const video = document.querySelector('video');
				return Boolean(video && video.videoWidth > 0);
			},
			undefined,
			{ timeout: 20_000 }
		);
		await Bun.sleep(warmupSeconds * 1000);
		return await sampleSteadyTree({ commandMarker: profileDir }, url, warmupSeconds, steadySeconds);
	} finally {
		await context.close();
	}
}

async function measureDesktopSteady(
	paths: HarnessPaths,
	url: string,
	warmupSeconds: number,
	steadySeconds: number
): Promise<SteadyStats> {
	const holdMs = (warmupSeconds + steadySeconds + 15) * 1000;
	const diagPath = path.join(paths.runRoot, 'diag-steady.json');
	const proc = Bun.spawn(
		[
			paths.desktopBin,
			'--config',
			path.join(paths.runRoot, 'desktop-client-config.toml'),
			'--server-url',
			url,
			'--diagnostics-file',
			diagPath,
			'--smoke-exit-ms',
			String(holdMs)
		],
		{ cwd: paths.repositoryRoot, stdout: 'ignore', stderr: 'ignore' }
	);
	try {
		await Bun.sleep(warmupSeconds * 1000);
		return await sampleSteadyTree({ rootPid: proc.pid }, url, warmupSeconds, steadySeconds);
	} finally {
		proc.kill();
		await proc.exited;
	}
}

async function sampleSteadyTree(
	selector: { rootPid?: number; commandMarker?: string },
	url: string,
	warmupSeconds: number,
	steadySeconds: number
): Promise<SteadyStats> {
	const intervalMs = Math.min(5_000, Math.max(1_000, Math.floor((steadySeconds * 1000) / 5)));
	const memorySamples: number[] = [];
	let maxProcesses = 0;
	let webrtcObserved = false;

	const first = sampleProcessTree(selector);
	const startWall = performance.now();
	const deadline = Date.now() + steadySeconds * 1000;
	let last = first;

	while (Date.now() < deadline) {
		await Bun.sleep(intervalMs);
		const sample = sampleProcessTree(selector);
		last = sample;
		memorySamples.push(sample.memoryMib);
		if (sample.processCount > maxProcesses) maxProcesses = sample.processCount;
		if (!webrtcObserved) webrtcObserved = await checkActiveWebRtcSession(url);
	}

	const elapsedSecs = Math.max(1, (performance.now() - startWall) / 1000);
	const cpuDelta = Math.max(0, last.cpuSeconds - first.cpuSeconds);
	const meanCpuPercent = Number(((cpuDelta / elapsedSecs) * 100).toFixed(2));
	const memStats = summarize(memorySamples);

	return {
		warmupSecs: warmupSeconds,
		durationSecs: steadySeconds,
		samples: memorySamples.length,
		meanCpuPercent,
		medianMemoryMib: memStats.median,
		p95MemoryMib: memStats.p95,
		maxProcessCount: maxProcesses,
		webrtcSessionObserved: webrtcObserved
	};
}

async function checkActiveWebRtcSession(url: string): Promise<boolean> {
	try {
		const res = await fetch(`${url}/metrics`);
		if (!res.ok) return false;
		const text = await res.text();
		return /keeppeek_webrtc_active_sessions\s+[1-9]/.test(text);
	} catch {
		return false;
	}
}

function sampleProcessTree(selector: { rootPid?: number; commandMarker?: string }): TreeSample {
	if (process.platform === 'win32') return sampleWindowsProcessTree(selector);
	if (process.platform === 'darwin') return sampleDarwinProcessTree(selector);
	throw new Error(`Process-tree sampling is not supported on ${process.platform}.`);
}

function sampleWindowsProcessTree(selector: {
	rootPid?: number;
	commandMarker?: string;
}): TreeSample {
	const rootExpr =
		selector.rootPid !== undefined
			? `@(${selector.rootPid})`
			: `@($all | Where-Object { $_.CommandLine -and $_.CommandLine.Contains('${(selector.commandMarker ?? '').replace(/'/g, "''")}') } | ForEach-Object { [int]$_.ProcessId })`;
	const script = `
$all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, WorkingSetSize, KernelModeTime, UserModeTime, CommandLine
$roots = ${rootExpr}
$children = @{}
foreach ($p in $all) {
  $parent = [int]$p.ParentProcessId
  if (-not $children.ContainsKey($parent)) { $children[$parent] = New-Object System.Collections.Generic.List[int] }
  $children[$parent].Add([int]$p.ProcessId)
}
$byId = @{}
foreach ($p in $all) { $byId[[int]$p.ProcessId] = $p }
$visited = New-Object System.Collections.Generic.HashSet[int]
$queue = New-Object System.Collections.Generic.Queue[int]
foreach ($r in $roots) { $queue.Enqueue([int]$r) }
while ($queue.Count -gt 0) {
  $cur = $queue.Dequeue()
  if ($visited.Add($cur) -and $children.ContainsKey($cur)) {
    foreach ($child in $children[$cur]) { $queue.Enqueue($child) }
  }
}
$mem = 0L; $cpu = 0L; $count = 0
foreach ($id in $visited) {
  if ($byId.ContainsKey($id)) {
    $p = $byId[$id]
    $count++
    $mem += [int64]$p.WorkingSetSize
    $cpu += [int64]$p.KernelModeTime + [int64]$p.UserModeTime
  }
}
@{ processCount = $count; memoryMib = [math]::Round($mem / 1MB, 2); cpuSeconds = [math]::Round($cpu / 10000000.0, 4) } | ConvertTo-Json -Compress
`;
	const res = Bun.spawnSync(['powershell.exe', '-NoLogo', '-NoProfile', '-Command', script], {
		stdout: 'pipe',
		stderr: 'pipe'
	});
	if (res.exitCode !== 0) {
		throw new Error(`Windows process-tree sampling failed: ${new TextDecoder().decode(res.stderr)}`);
	}
	return JSON.parse(new TextDecoder().decode(res.stdout).trim()) as TreeSample;
}

function sampleDarwinProcessTree(selector: {
	rootPid?: number;
	commandMarker?: string;
}): TreeSample {
	const res = Bun.spawnSync(['ps', '-axo', 'pid=,ppid=,rss=,time=,command='], {
		stdout: 'pipe',
		stderr: 'pipe'
	});
	if (res.exitCode !== 0) {
		throw new Error(`macOS process-tree sampling failed: ${new TextDecoder().decode(res.stderr)}`);
	}
	const rows = new Map<number, { ppid: number; rssKib: number; cpuSecs: number; cmd: string }>();
	const children = new Map<number, number[]>();
	for (const line of new TextDecoder().decode(res.stdout).split('\n')) {
		const match = /^\s*(\d+)\s+(\d+)\s+(\d+)\s+([0-9:.-]+)\s+(.*)$/.exec(line);
		if (!match) continue;
		const pid = Number(match[1]);
		const ppid = Number(match[2]);
		rows.set(pid, {
			ppid,
			rssKib: Number(match[3]),
			cpuSecs: parsePsTime(match[4]),
			cmd: match[5]
		});
		const list = children.get(ppid) ?? [];
		list.push(pid);
		children.set(ppid, list);
	}
	const roots: number[] = [];
	if (selector.rootPid !== undefined) {
		roots.push(selector.rootPid);
	} else if (selector.commandMarker) {
		for (const [pid, info] of rows) {
			if (info.cmd.includes(selector.commandMarker)) roots.push(pid);
		}
	}
	const visited = new Set<number>();
	const queue = [...roots];
	while (queue.length > 0) {
		const current = queue.shift()!;
		if (visited.has(current)) continue;
		visited.add(current);
		for (const child of children.get(current) ?? []) queue.push(child);
	}
	let rssKib = 0;
	let cpuSecs = 0;
	let processCount = 0;
	for (const pid of visited) {
		const info = rows.get(pid);
		if (!info) continue;
		processCount += 1;
		rssKib += info.rssKib;
		cpuSecs += info.cpuSecs;
	}
	return {
		processCount,
		memoryMib: Number((rssKib / 1024).toFixed(2)),
		cpuSeconds: Number(cpuSecs.toFixed(4))
	};
}

function parsePsTime(raw: string): number {
	const [dayPart, clockPart] = raw.includes('-') ? raw.split('-') : ['0', raw];
	const parts = clockPart.split(':').map(Number);
	const days = Number(dayPart) || 0;
	if (parts.length === 3) return days * 86_400 + parts[0] * 3600 + parts[1] * 60 + parts[2];
	if (parts.length === 2) return days * 86_400 + parts[0] * 60 + parts[1];
	return days * 86_400 + (parts[0] || 0);
}

function detectWebViewVersion(): string {
	if (process.platform !== 'win32') return 'WKWebView (system)';
	const script = `
$paths = @(
  'HKLM:\\SOFTWARE\\WOW6432Node\\Microsoft\\EdgeUpdate\\Clients\\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
  'HKCU:\\Software\\Microsoft\\EdgeUpdate\\Clients\\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
)
foreach ($p in $paths) {
  $v = (Get-ItemProperty -Path $p -ErrorAction SilentlyContinue).pv
  if ($v) { Write-Output "WebView2 $v"; break }
}
`;
	const res = Bun.spawnSync(['powershell.exe', '-NoLogo', '-NoProfile', '-Command', script], {
		stdout: 'pipe',
		stderr: 'ignore'
	});
	return new TextDecoder().decode(res.stdout).trim() || 'WebView2 (unknown)';
}

function summarize(values: number[]): { min: number; median: number; p95: number; max: number } {
	if (values.length === 0) return { min: 0, median: 0, p95: 0, max: 0 };
	const sorted = [...values].sort((left, right) => left - right);
	const mid = Math.floor(sorted.length / 2);
	const median =
		sorted.length % 2 === 0 ? Number(((sorted[mid - 1] + sorted[mid]) / 2).toFixed(2)) : sorted[mid];
	const p95Index = Math.min(sorted.length - 1, Math.ceil(sorted.length * 0.95) - 1);
	return {
		min: sorted[0],
		median,
		p95: sorted[p95Index],
		max: sorted[sorted.length - 1]
	};
}
