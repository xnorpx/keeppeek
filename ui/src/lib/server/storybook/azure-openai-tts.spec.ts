import { describe, expect, it, vi } from 'vitest';
import {
	createAzureOpenAiTtsRequest,
	loadAzureOpenAiTtsConfig,
	synthesizeAzureOpenAiNarration
} from './azure-openai-tts';

const narration = {
	text: 'Open Keep and review now.',
	voice: 'coral',
	instructions: 'Speak clearly in a calm product-demo tone.',
	speed: 1.05
};

describe('Azure OpenAI demo narration', () => {
	it('loads API key configuration and expands a resource endpoint', () => {
		expect(
			loadAzureOpenAiTtsConfig({
				AZURE_OPENAI_API_KEY: 'secret',
				AZURE_OPENAI_ENDPOINT: 'https://keeppeek.openai.azure.com/',
				AZURE_OPENAI_TTS_DEPLOYMENT: 'keeppeek-demo-tts'
			})
		).toEqual({
			endpoint: new URL('https://keeppeek.openai.azure.com/openai/v1/audio/speech'),
			deployment: 'keeppeek-demo-tts',
			authentication: { kind: 'api-key', value: 'secret' }
		});
	});

	it('loads token configuration without duplicating the v1 path', () => {
		expect(
			loadAzureOpenAiTtsConfig({
				AZURE_OPENAI_AUTH_TOKEN: 'token',
				AZURE_OPENAI_ENDPOINT: 'https://keeppeek.services.ai.azure.com/openai/v1/',
				AZURE_OPENAI_TTS_DEPLOYMENT: 'keeppeek-demo-tts'
			})
		).toEqual({
			endpoint: new URL('https://keeppeek.services.ai.azure.com/openai/v1/audio/speech'),
			deployment: 'keeppeek-demo-tts',
			authentication: { kind: 'authorization-token', value: 'token' }
		});
	});

	it('rejects ambiguous or insecure configuration', () => {
		expect(() =>
			loadAzureOpenAiTtsConfig({
				AZURE_OPENAI_API_KEY: 'secret',
				AZURE_OPENAI_AUTH_TOKEN: 'token',
				AZURE_OPENAI_ENDPOINT: 'https://keeppeek.openai.azure.com/',
				AZURE_OPENAI_TTS_DEPLOYMENT: 'keeppeek-demo-tts'
			})
		).toThrow('Set exactly one');
		expect(() =>
			loadAzureOpenAiTtsConfig({
				AZURE_OPENAI_API_KEY: 'secret',
				AZURE_OPENAI_ENDPOINT: 'http://localhost/openai/v1/',
				AZURE_OPENAI_TTS_DEPLOYMENT: 'keeppeek-demo-tts'
			})
		).toThrow('must use HTTPS');
	});

	it('creates a WAV request with deployment, voice, and direction', () => {
		expect(createAzureOpenAiTtsRequest(narration, 'keeppeek-demo-tts')).toEqual({
			model: 'keeppeek-demo-tts',
			input: 'Open Keep and review now.',
			voice: 'coral',
			response_format: 'wav',
			instructions: 'Speak clearly in a calm product-demo tone.',
			speed: 1.05
		});
	});

	it('synthesizes WAV audio without putting credentials in the body', async () => {
		const fetchMock = vi.fn<typeof fetch>(
			async () => new Response(new Uint8Array([82, 73, 70, 70]))
		);
		const audio = await synthesizeAzureOpenAiNarration(
			narration,
			{
				endpoint: new URL('https://keeppeek.openai.azure.com/openai/v1/audio/speech'),
				deployment: 'keeppeek-demo-tts',
				authentication: { kind: 'api-key', value: 'secret' }
			},
			fetchMock
		);

		expect(new Uint8Array(audio)).toEqual(new Uint8Array([82, 73, 70, 70]));
		const [, init] = fetchMock.mock.calls[0]!;
		expect(init?.method).toBe('POST');
		expect(new Headers(init?.headers).get('api-key')).toBe('secret');
		expect(init?.body).not.toContain('secret');
	});

	it('reports only the Azure status when synthesis fails', async () => {
		await expect(
			synthesizeAzureOpenAiNarration(
				narration,
				{
					endpoint: new URL('https://keeppeek.openai.azure.com/openai/v1/audio/speech'),
					deployment: 'keeppeek-demo-tts',
					authentication: { kind: 'api-key', value: 'secret' }
				},
				vi.fn(async () => new Response('sensitive service details', { status: 401 }))
			)
		).rejects.toThrow('Azure OpenAI TTS failed with status 401');
	});
});
