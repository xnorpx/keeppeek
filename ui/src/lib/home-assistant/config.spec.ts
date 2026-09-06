import { describe, expect, it } from 'vitest';
import { parseCardConfig } from './config';

const validConfig = {
	type: 'custom:keeppeek-card',
	endpoint: 'https://keeppeek.example.net/',
	token: 'test-only-credential',
	sources: [{ source_id: 'front-door', title: 'Front door' }]
};

describe('Home Assistant card configuration', () => {
	it('normalizes the endpoint and supplies bounded live-grid defaults', () => {
		expect(parseCardConfig(validConfig)).toEqual({
			...validConfig,
			endpoint: 'https://keeppeek.example.net',
			sources: [{ source_id: 'front-door', title: 'Front door', quality: 'auto' }],
			layout: 'grid',
			columns: 2,
			aspect_ratio: '16:9',
			show_name: true,
			view: 'live'
		});
	});

	it.each([
		'not-a-url',
		'http://keeppeek.example.net',
		'https://user:test-only-credential@keeppeek.example.net',
		'https://keeppeek.example.net?token=test-only-credential',
		'https://keeppeek.example.net/#test-only-credential',
		'javascript:test-only-credential'
	])('rejects an unsafe endpoint without echoing its contents', (endpoint) => {
		expect(() => parseCardConfig({ ...validConfig, endpoint })).toThrow(/endpoint/i);
		try {
			parseCardConfig({ ...validConfig, endpoint });
		} catch (error) {
			expect(String(error)).not.toContain(validConfig.token);
		}
	});

	it.each(['http://localhost:3000', 'http://127.0.0.1:3000', 'http://[::1]:3000'])(
		'allows loopback HTTP for local development',
		(endpoint) => {
			expect(parseCardConfig({ ...validConfig, endpoint }).endpoint).toBe(endpoint);
		}
	);

	it.each([
		{ token: '' },
		{ token: 'credential\r\nheader' },
		{ sources: [] },
		{ sources: [{ source_id: '' }] },
		{ sources: [{ source_id: 'duplicate' }, { source_id: 'duplicate' }] },
		{ sources: Array.from({ length: 17 }, (_, index) => ({ source_id: `source-${index}` })) },
		{ columns: 0 },
		{ columns: 5 },
		{ columns: 1.5 },
		{ layout: 'unknown' },
		{ view: 'timeline' },
		{ aspect_ratio: 'unbounded' },
		{ show_name: 'yes' }
	])('rejects invalid options before connecting: %j', (options) => {
		expect(() => parseCardConfig({ ...validConfig, ...options })).toThrow();
	});
});
