import { createHash } from 'node:crypto';
import { copyFile, mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';

type PaperJsxResponse = {
	jsx?: unknown;
	contentHash?: { tokens?: unknown };
};

const [kind, inputPath, outputPath] = process.argv.slice(2);
if (!kind || !inputPath || !outputPath || (kind !== 'jsx' && kind !== 'reference')) {
	throw new Error(
		'Usage: bun scripts/import-paper-node-export.ts <jsx|reference> <input> <output>'
	);
}

const absoluteInput = resolve(inputPath);
const absoluteOutput = resolve(outputPath);
await mkdir(dirname(absoluteOutput), { recursive: true });

if (kind === 'jsx') {
	const response = JSON.parse(await readFile(absoluteInput, 'utf8')) as PaperJsxResponse;
	if (typeof response.jsx !== 'string' || response.jsx.trim().length === 0) {
		throw new Error('Paper JSX response is empty');
	}
	if (response.contentHash?.tokens !== 'cf3b1cd7') {
		throw new Error('Paper JSX response uses an unexpected token revision');
	}
	await writeFile(absoluteOutput, `${response.jsx.trim()}\n`);
} else {
	await copyFile(absoluteInput, absoluteOutput);
}

const contents = await readFile(absoluteOutput);
console.log(
	JSON.stringify({
		path: outputPath,
		bytes: contents.byteLength,
		sha256: createHash('sha256').update(contents).digest('hex')
	})
);
