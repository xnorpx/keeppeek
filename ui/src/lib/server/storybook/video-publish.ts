import { createHash } from 'node:crypto';
import { demoAssetId, type StoryScenarioMetadata } from '$lib/storybook/demo';

export type DemoPublishAsset = {
	fileName: string;
	contents: Uint8Array;
};

export type DemoPublishEntry = {
	metadata: StoryScenarioMetadata;
	video: DemoPublishAsset;
	captions: DemoPublishAsset;
	metadataAsset: DemoPublishAsset;
};

export type HostedDemoVideo = {
	assetId: string;
	scenarioId: string;
	storyId: string;
	title: string;
	purpose: string;
	video: HostedAsset;
	captions: HostedAsset;
	metadata: HostedAsset;
};

export type HostedAsset = {
	url: string;
	bytes: number;
	sha256: string;
};

export type DemoVideoManifest = {
	schemaVersion: 1;
	generatedAt: string;
	commitSha: string;
	videos: HostedDemoVideo[];
};

function normalizeBaseUrl(baseUrl: string): string {
	const url = new URL(baseUrl);
	if (url.protocol !== 'https:') throw new Error('Demo video base URL must use HTTPS');
	return url.toString().replace(/\/$/, '');
}

function hostedAsset(baseUrl: string, asset: DemoPublishAsset): HostedAsset {
	if (asset.fileName.includes('/') || asset.fileName.includes('\\')) {
		throw new Error(`Demo asset filename must not contain a path: ${asset.fileName}`);
	}
	return {
		url: `${baseUrl}/assets/${encodeURIComponent(asset.fileName)}`,
		bytes: asset.contents.byteLength,
		sha256: createHash('sha256').update(asset.contents).digest('hex')
	};
}

export function createDemoVideoManifest(options: {
	baseUrl: string;
	commitSha: string;
	generatedAt: string;
	entries: readonly DemoPublishEntry[];
}): DemoVideoManifest {
	const baseUrl = normalizeBaseUrl(options.baseUrl);
	const assetIds = new Set<string>();
	const videos = options.entries
		.map((entry): HostedDemoVideo => {
			const demo = entry.metadata.demo;
			if (demo === undefined)
				throw new Error(`Scenario ${entry.metadata.storyId} has no demo metadata`);
			const assetId = demoAssetId(entry.metadata);
			if (assetIds.has(assetId)) throw new Error(`Duplicate demo asset: ${assetId}`);
			assetIds.add(assetId);
			return {
				assetId,
				scenarioId: entry.metadata.paper.scenarioId,
				storyId: entry.metadata.storyId,
				title: demo.title,
				purpose: demo.purpose,
				video: hostedAsset(baseUrl, entry.video),
				captions: hostedAsset(baseUrl, entry.captions),
				metadata: hostedAsset(baseUrl, entry.metadataAsset)
			};
		})
		.toSorted((left, right) => left.assetId.localeCompare(right.assetId));

	return {
		schemaVersion: 1,
		generatedAt: options.generatedAt,
		commitSha: options.commitSha,
		videos
	};
}
