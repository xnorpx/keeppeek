import { describe, expect, it } from 'vitest';
import type { StoryScenarioMetadata } from '$lib/storybook/demo';
import { createDemoVideoManifest, type DemoPublishEntry } from './video-publish';

function entry(scenarioId: string, videoContents: string, assetId?: string): DemoPublishEntry {
	const metadata: StoryScenarioMetadata = {
		storyId: scenarioId,
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: 'test',
			frameId: 'test',
			scenarioId
		},
		demo: {
			...(assetId === undefined ? {} : { assetId }),
			title: `Demo ${scenarioId}`,
			purpose: 'Verify automatic publishing.',
			durationMs: 1_000,
			viewport: { width: 320, height: 180 },
			captions: [{ atMs: 0, text: 'Demo' }],
			actions: [{ kind: 'click', atMs: 100, selector: '[data-demo-action]' }],
			completionSignal: { selector: '[data-demo-complete]', state: 'visible' }
		}
	};
	return {
		metadata,
		video: { fileName: `${scenarioId}.mp4`, contents: new TextEncoder().encode(videoContents) },
		captions: { fileName: `${scenarioId}.vtt`, contents: new TextEncoder().encode('WEBVTT') },
		metadataAsset: {
			fileName: `${scenarioId}.json`,
			contents: new TextEncoder().encode(JSON.stringify(metadata))
		}
	};
}

describe('demo video publishing manifest', () => {
	it('creates sorted stable HTTPS URLs and content hashes', () => {
		const manifest = createDemoVideoManifest({
			baseUrl: 'https://media.keeppeek.example/demos/',
			commitSha: 'abc123',
			generatedAt: '2026-08-19T00:00:00.000Z',
			entries: [entry('peek.mobile.live', 'mobile'), entry('peek.desktop.live', 'desktop')]
		});

		expect(manifest.videos.map((video) => video.assetId)).toEqual([
			'peek.desktop.live',
			'peek.mobile.live'
		]);
		expect(manifest.videos.map((video) => video.scenarioId)).toEqual([
			'peek.desktop.live',
			'peek.mobile.live'
		]);
		expect(manifest.videos[0].video.url).toBe(
			'https://media.keeppeek.example/demos/assets/peek.desktop.live.mp4'
		);
		expect(manifest.videos[0].video.sha256).toMatch(/^[a-f0-9]{64}$/);
		expect(manifest.commitSha).toBe('abc123');
	});

	it('rejects insecure URLs, duplicate assets, and nested asset paths', () => {
		expect(() =>
			createDemoVideoManifest({
				baseUrl: 'http://media.example/demos',
				commitSha: 'abc123',
				generatedAt: '2026-08-19T00:00:00.000Z',
				entries: []
			})
		).toThrow('must use HTTPS');

		const duplicate = entry('peek.desktop.live', 'first');
		expect(() =>
			createDemoVideoManifest({
				baseUrl: 'https://media.example/demos',
				commitSha: 'abc123',
				generatedAt: '2026-08-19T00:00:00.000Z',
				entries: [duplicate, duplicate]
			})
		).toThrow('Duplicate demo asset');

		const catalog = entry(
			'cameras.desktop.add-wizard',
			'catalog',
			'cameras.desktop.catalog-guided-setup'
		);
		const lifecycle = entry(
			'cameras.desktop.add-wizard',
			'lifecycle',
			'cameras.desktop.camera-lifecycle'
		);
		expect(
			createDemoVideoManifest({
				baseUrl: 'https://media.example/demos',
				commitSha: 'abc123',
				generatedAt: '2026-08-19T00:00:00.000Z',
				entries: [catalog, lifecycle]
			}).videos
		).toHaveLength(2);

		const nested = entry('peek.desktop.live', 'video');
		nested.video.fileName = '../video.mp4';
		expect(() =>
			createDemoVideoManifest({
				baseUrl: 'https://media.example/demos',
				commitSha: 'abc123',
				generatedAt: '2026-08-19T00:00:00.000Z',
				entries: [nested]
			})
		).toThrow('must not contain a path');
	});
});
