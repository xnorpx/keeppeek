import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';

const uiRoot = process.cwd();
const repositoryRoot = resolve(uiRoot, '..');

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

for (const filename of ['package-lock.json', 'pnpm-lock.yaml', 'yarn.lock', 'bun.lockb']) {
	if (existsSync(resolve(uiRoot, filename))) {
		throw new Error(`${filename} is not allowed; use bun.lock from the public npm registry`);
	}
}

const bunLock = readFileSync(resolve(uiRoot, 'bun.lock'), 'utf8');
const nonPublicNpmUrls = [...bunLock.matchAll(/https:\/\/[^"\s]+/g)]
	.map(([url]) => url)
	.filter((url) => new URL(url).hostname !== 'registry.npmjs.org');

if (nonPublicNpmUrls.length > 0) {
	throw new Error(`bun.lock contains non-public npm URLs: ${nonPublicNpmUrls.join(', ')}`);
}

const cargoLock = readFileSync(resolve(repositoryRoot, 'Cargo.lock'), 'utf8');
const nonPublicCargoSources = [...cargoLock.matchAll(/source = "registry\+([^"]+)"/g)]
	.map(([, source]) => source)
	.filter((source) => source !== 'https://github.com/rust-lang/crates.io-index');

if (nonPublicCargoSources.length > 0) {
	throw new Error(
		`Cargo.lock contains non-public registry sources: ${nonPublicCargoSources.join(', ')}`
	);
}

console.log('Public npm and crates.io registry configuration verified.');
