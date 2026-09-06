import { createHash } from 'node:crypto';
import { readFileSync, realpathSync } from 'node:fs';
import { resolve } from 'node:path';
import { gzipSync } from 'node:zlib';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vite';

const packageInfo = JSON.parse(
	readFileSync(new URL('./package.json', import.meta.url), 'utf8')
) as { version: string };
const version = (process.env.KEEPPEEK_CARD_VERSION ?? packageInfo.version).replace(/^v/, '');
if (!/^\d+\.\d+\.\d+(?:-[\da-zA-Z.-]+)?$/.test(version))
	throw new Error('KEEPPEEK_CARD_VERSION must be a semantic version.');

export default defineConfig({
	publicDir: false,
	cacheDir: '../target/home-assistant-card/vite-cache',
	server: {
		fs: {
			allow: [
				import.meta.dirname,
				realpathSync(resolve(import.meta.dirname, 'node_modules/@fontsource/archivo'))
			]
		}
	},
	plugins: [
		svelte({ configFile: false }),
		{
			name: 'keeppeek-card-artifact',
			generateBundle(_options, bundle) {
				const module = bundle['keeppeek.js'];
				if (!module || module.type !== 'chunk')
					throw new Error('The card module was not generated.');
				const gzipBytes = gzipSync(module.code).byteLength;
				if (gzipBytes > 500 * 1024) throw new Error('The card exceeds its 500 KiB gzip budget.');
				this.emitFile({
					type: 'asset',
					fileName: 'keeppeek-card.json',
					source:
						JSON.stringify(
							{
								version,
								file: 'keeppeek.js',
								bytes: Buffer.byteLength(module.code),
								gzip_bytes: gzipBytes,
								sha256: createHash('sha256').update(module.code).digest('hex')
							},
							null,
							2
						) + '\n'
				});
			}
		}
	],
	define: { 'import.meta.env.VITE_KEEPPEEK_CARD_VERSION': JSON.stringify(version) },
	build: {
		outDir: resolve(import.meta.dirname, '../target/home-assistant-card/dist'),
		emptyOutDir: true,
		target: 'es2022',
		minify: true,
		lib: {
			entry: resolve(import.meta.dirname, 'src/lib/home-assistant/elements.svelte.ts'),
			formats: ['es'],
			fileName: () => 'keeppeek.js'
		}
	}
});
