import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';

type TokenSnapshot = {
	tokens: Array<{ name: string; value: string }>;
};

const themePrefixes = [
	'--breakpoint-',
	'--color-',
	'--container-',
	'--font-',
	'--leading-',
	'--radius-',
	'--spacing-',
	'--text-',
	'--tracking-'
] as const;

function normalizeValue(value: string): string {
	return /^#[0-9a-f]+$/i.test(value) ? value.toLowerCase() : value;
}

function declarations(tokens: TokenSnapshot['tokens']): string {
	return tokens.map((token) => `\t${token.name}: ${normalizeValue(token.value)};`).join('\n');
}

const sourcePath = resolve('design/paper/keeppeek-nvr-v34/tokens.json');
const outputPath = resolve('src/styles/paper-theme.css');
const snapshot = JSON.parse(await readFile(sourcePath, 'utf8')) as TokenSnapshot;
const themeTokens = snapshot.tokens.filter((token) =>
	themePrefixes.some((prefix) => token.name.startsWith(prefix))
);
const runtimeTokens = snapshot.tokens.filter(
	(token) => !themePrefixes.some((prefix) => token.name.startsWith(prefix))
);

const css = `@theme {\n${declarations(themeTokens)}\n}\n\n:root {\n${declarations(runtimeTokens)}\n}\n`;
await mkdir(dirname(outputPath), { recursive: true });
await writeFile(outputPath, css);
console.log(`Generated ${themeTokens.length} Tailwind and ${runtimeTokens.length} runtime tokens`);
