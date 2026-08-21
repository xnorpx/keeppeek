import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';

const uiRoot = process.cwd();
const repositoryRoot = resolve(uiRoot, '..');
const requiredBunVersion = '1.4.0';

type PackageManifest = {
	packageManager?: string;
	engines?: Record<string, string>;
};

function packageManifest(path: string): PackageManifest {
	return JSON.parse(readFileSync(path, 'utf8')) as PackageManifest;
}

function versionParts(version: string): [number, number, number] {
	const match = /^(\d+)\.(\d+)\.(\d+)/.exec(version);
	if (!match) throw new Error(`Invalid Bun version: ${version}`);
	return [Number(match[1]), Number(match[2]), Number(match[3])];
}

function versionAtLeast(actual: string, required: string): boolean {
	const actualParts = versionParts(actual);
	const requiredParts = versionParts(required);
	for (let index = 0; index < actualParts.length; index += 1) {
		if (actualParts[index] !== requiredParts[index]) {
			return actualParts[index] > requiredParts[index];
		}
	}
	return true;
}

const requiredSettings = [
	[resolve(repositoryRoot, '.npmrc'), 'registry=https://registry.npmjs.org/'],
	[resolve(uiRoot, 'bunfig.toml'), 'registry = "https://registry.npmjs.org/"'],
	[resolve(repositoryRoot, '.cargo/config.toml'), 'default = "crates-io"'],
	[resolve(repositoryRoot, '.cargo/config.toml'), 'protocol = "sparse"']
] as const;

for (const [path, expected] of requiredSettings) {
	const contents = readFileSync(path, 'utf8');
	if (!contents.includes(expected)) {
		throw new Error(`${path} must contain ${expected}`);
	}
}

const runtimeBunVersion = (process.versions as Record<string, string | undefined>).bun;
if (!runtimeBunVersion || !versionAtLeast(runtimeBunVersion, requiredBunVersion)) {
	throw new Error(`Bun ${requiredBunVersion} or newer is required`);
}

const bunVersionFile = readFileSync(resolve(uiRoot, '.bun-version'), 'utf8').trim();
if (bunVersionFile !== requiredBunVersion) {
	throw new Error(`.bun-version must pin ${requiredBunVersion}`);
}

const uiManifest = packageManifest(resolve(uiRoot, 'package.json'));
const visualHarnessManifest = packageManifest(resolve(uiRoot, 'visual-harness/package.json'));
if (uiManifest.packageManager !== `bun@${requiredBunVersion}`) {
	throw new Error(`UI packageManager must pin bun@${requiredBunVersion}`);
}
if (uiManifest.engines?.bun !== `>=${requiredBunVersion}`) {
	throw new Error(`UI Bun engine must require >=${requiredBunVersion}`);
}
if (visualHarnessManifest.packageManager !== `bun@${requiredBunVersion}`) {
	throw new Error(`Visual harness packageManager must pin bun@${requiredBunVersion}`);
}

const lockfilePath = resolve(uiRoot, 'bun.lock');
if (existsSync(lockfilePath)) {
	const lockfile = readFileSync(lockfilePath, 'utf8');
	const packageUrls = lockfile.match(/https:\/\/[^"\s]+/g) ?? [];
	const nonPublicUrl = packageUrls.find(
		(packageUrl) => !packageUrl.startsWith('https://registry.npmjs.org/')
	);
	if (nonPublicUrl)
		throw new Error(`Bun lockfile contains a non-public registry URL: ${nonPublicUrl}`);
}

console.log(`Bun ${runtimeBunVersion}, public npm, and crates.io registry configuration verified.`);
