import { resolve, sep } from 'node:path';

const harnessRoot = import.meta.dir;
const storybookRoot = resolve(harnessRoot, 'storybook-static');
if (!(await Bun.file(resolve(storybookRoot, 'index.html')).exists())) {
	throw new Error('Build Storybook before running Loki.');
}

const server = Bun.serve({
	hostname: '0.0.0.0',
	port: 0,
	async fetch(request) {
		let pathname: string;
		try {
			pathname = decodeURIComponent(new URL(request.url).pathname);
		} catch {
			return new Response('Invalid path', { status: 400 });
		}
		const relativePath = pathname.replace(/^\/+/, '') || 'index.html';
		const filePath = resolve(storybookRoot, relativePath);
		if (filePath !== storybookRoot && !filePath.startsWith(`${storybookRoot}${sep}`)) {
			return new Response('Invalid path', { status: 400 });
		}
		const file = Bun.file(filePath);
		return (await file.exists()) ? new Response(file) : new Response('Not found', { status: 404 });
	}
});

const environment = { ...process.env };
delete environment.CI;
delete environment.CONTINUOUS_INTEGRATION;
delete environment.BUILD_NUMBER;
delete environment.RUN_ID;
const lokiBinary = resolve(
	harnessRoot,
	'node_modules',
	'.bin',
	process.platform === 'win32' ? 'loki.cmd' : 'loki'
);

async function runLoki(configuration: 'desktop' | 'mobile'): Promise<number> {
	const process = Bun.spawn(
		[
			lokiBinary,
			'test',
			'--reactUri',
			`http://host.docker.internal:${server.port}`,
			'--configurationFilter',
			`^chrome\\.${configuration}$`,
			'--requireReference=false',
			'--verboseRenderer'
		],
		{
			cwd: harnessRoot,
			env: environment,
			stdin: 'inherit',
			stdout: 'inherit',
			stderr: 'inherit'
		}
	);
	return process.exited;
}

try {
	const desktopExitCode = await runLoki('desktop');
	const mobileExitCode = await runLoki('mobile');
	if (desktopExitCode !== 0 || mobileExitCode !== 0) process.exitCode = 1;
} finally {
	await server.stop(true);
}
