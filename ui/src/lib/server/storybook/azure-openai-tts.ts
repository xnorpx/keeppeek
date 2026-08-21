import type { DemoNarration } from '$lib/storybook/demo';

export type AzureOpenAiTtsConfig = {
	endpoint: URL;
	deployment: string;
	authentication:
		{ kind: 'api-key'; value: string } | { kind: 'authorization-token'; value: string };
};

type AzureOpenAiEnvironment = Readonly<Record<string, string | undefined>>;

function requireEnvironmentValue(environment: AzureOpenAiEnvironment, name: string): string {
	const value = environment[name]?.trim();
	if (value === undefined || value.length === 0) {
		throw new Error(`${name} is required for Azure OpenAI TTS`);
	}
	return value;
}

function resolveTtsEndpoint(configuredEndpoint: string): URL {
	const endpoint = new URL(configuredEndpoint);
	if (endpoint.protocol !== 'https:') {
		throw new Error('AZURE_OPENAI_ENDPOINT must use HTTPS');
	}

	const basePath = endpoint.pathname.replace(/\/+$/, '');
	if (basePath.endsWith('/openai/v1/audio/speech')) return endpoint;
	endpoint.pathname = basePath.endsWith('/openai/v1')
		? `${basePath}/audio/speech`
		: `${basePath}/openai/v1/audio/speech`;
	return endpoint;
}

export function loadAzureOpenAiTtsConfig(
	environment: AzureOpenAiEnvironment
): AzureOpenAiTtsConfig {
	const apiKey = environment.AZURE_OPENAI_API_KEY?.trim();
	const authorizationToken = environment.AZURE_OPENAI_AUTH_TOKEN?.trim();
	if (
		(apiKey === undefined || apiKey.length === 0) ===
		(authorizationToken === undefined || authorizationToken.length === 0)
	) {
		throw new Error('Set exactly one of AZURE_OPENAI_API_KEY or AZURE_OPENAI_AUTH_TOKEN');
	}

	return {
		endpoint: resolveTtsEndpoint(requireEnvironmentValue(environment, 'AZURE_OPENAI_ENDPOINT')),
		deployment: requireEnvironmentValue(environment, 'AZURE_OPENAI_TTS_DEPLOYMENT'),
		authentication:
			authorizationToken !== undefined && authorizationToken.length > 0
				? { kind: 'authorization-token', value: authorizationToken }
				: { kind: 'api-key', value: apiKey! }
	};
}

export function createAzureOpenAiTtsRequest(
	narration: DemoNarration,
	deployment: string
): Record<string, string | number> {
	return {
		model: deployment,
		input: narration.text,
		voice: narration.voice,
		response_format: 'wav',
		...(narration.instructions === undefined ? {} : { instructions: narration.instructions }),
		...(narration.speed === undefined ? {} : { speed: narration.speed })
	};
}

export async function synthesizeAzureOpenAiNarration(
	narration: DemoNarration,
	config: AzureOpenAiTtsConfig,
	fetchImplementation: typeof fetch = fetch
): Promise<ArrayBuffer> {
	const headers = new Headers({ 'Content-Type': 'application/json' });
	if (config.authentication.kind === 'api-key') {
		headers.set('api-key', config.authentication.value);
	} else {
		headers.set('Authorization', `Bearer ${config.authentication.value}`);
	}

	const response = await fetchImplementation(config.endpoint, {
		method: 'POST',
		headers,
		body: JSON.stringify(createAzureOpenAiTtsRequest(narration, config.deployment))
	});
	if (!response.ok) {
		throw new Error(`Azure OpenAI TTS failed with status ${response.status}`);
	}

	const audio = await response.arrayBuffer();
	if (audio.byteLength === 0) throw new Error('Azure OpenAI TTS returned empty audio');
	return audio;
}
