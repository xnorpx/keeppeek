import { readFileSync } from 'node:fs';
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

console.log('Public npm and crates.io registry configuration verified.');
