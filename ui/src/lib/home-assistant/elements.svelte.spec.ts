import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	KeepPeekConnectionManager,
	type SessionAdapter,
	type SessionSnapshot
} from './connection-manager';
import './elements.svelte';
import { page } from 'vitest/browser';

type CardElement = HTMLElement & {
	setConfig: (config: unknown) => void;
	hass: unknown;
	getCardSize: () => number;
};
const cards: CardElement[] = [];
const adapters: SessionAdapter[] = [];
const managerKey = Symbol.for('keeppeek.card.connections.v1');
const config = {
	type: 'custom:keeppeek-card',
	endpoint: 'https://keeppeek.example.net',
	token: 'private-card-fixture-key',
	sources: [
		{ source_id: 'front-door', title: 'Front door' },
		{ source_id: 'driveway', title: 'Driveway' }
	]
};

function setup() {
	const factory = vi.fn((_config, notify: (snapshot: SessionSnapshot) => void) => {
		const adapter = {
			configure: vi.fn(() =>
				notify({ status: 'ready', message: null, cameras: [], streams: new Map() })
			),
			close: vi.fn(async () => undefined),
			retry: vi.fn()
		};
		adapters.push(adapter);
		return adapter;
	});
	Reflect.set(window, managerKey, new KeepPeekConnectionManager(factory));
	return factory;
}

function card() {
	const element = document.createElement('keeppeek-card') as CardElement;
	element.setConfig(config);
	element.style.width = '600px';
	document.body.append(element);
	cards.push(element);
	return element;
}

afterEach(async () => {
	for (const element of cards.splice(0)) element.remove();
	await vi.waitFor(() => {
		for (const adapter of adapters) expect(adapter.close).toHaveBeenCalledTimes(1);
	});
	adapters.splice(0);
	Reflect.deleteProperty(window, managerKey);
	vi.restoreAllMocks();
});

describe('KeepPeek Lovelace custom elements', () => {
	it('fills a single-camera card without an unused grid column', async () => {
		setup();
		const element = card();
		element.setConfig({ ...config, sources: config.sources.slice(0, 1) });
		await vi.waitFor(() =>
			expect(
				element
					.shadowRoot!.querySelector<HTMLElement>('.video-grid')
					?.style.getPropertyValue('--columns')
			).toBe('1')
		);
	});

	it('retries an acquisition failure even when there is no lease yet', async () => {
		const factory = setup();
		factory.mockImplementationOnce(() => {
			throw new Error('Fixture acquisition failure.');
		});
		card();
		await page.getByRole('button', { name: 'Reconnect', exact: true }).click();
		await vi.waitFor(() => expect(factory).toHaveBeenCalledTimes(2));
	});

	it('clears the previous camera view when replacement configuration is invalid', async () => {
		const factory = setup();
		const element = card();
		await vi.waitFor(() => expect(factory).toHaveBeenCalledTimes(1));
		expect(() => element.setConfig({ ...config, token: '' })).toThrow(/access key/);
		await vi.waitFor(() => expect(element.shadowRoot!.querySelectorAll('video')).toHaveLength(0));
		await vi.waitFor(() => expect(adapters[0]!.close).toHaveBeenCalledTimes(1));
	});

	it('shares sessions, keeps credentials out of DOM, and ignores unrelated hass updates', async () => {
		const factory = setup();
		const first = card();
		const second = card();
		await vi.waitFor(() => expect(factory).toHaveBeenCalledTimes(1));
		first.hass = { states: {} };
		first.hass = { states: { 'light.kitchen': { state: 'on' } } };
		expect(first.shadowRoot!.innerHTML).not.toContain(config.token);
		expect(first.getCardSize()).toBeGreaterThan(0);
		first.remove();
		await vi.waitFor(() => expect(adapters[0]!.configure).toHaveBeenCalled());
		expect(adapters[0]!.close).not.toHaveBeenCalled();
		second.remove();
		await vi.waitFor(() => expect(adapters[0]!.close).toHaveBeenCalledTimes(1));
	});

	it('updates sources and display settings without replacing the peer', async () => {
		const factory = setup();
		const element = card();
		await vi.waitFor(() => expect(factory).toHaveBeenCalledTimes(1));
		element.setConfig({ ...config, columns: 1, sources: [config.sources[0]] });
		await vi.waitFor(() =>
			expect(adapters[0]!.configure).toHaveBeenLastCalledWith([
				{ ...config.sources[0], quality: 'auto' }
			])
		);
		expect(factory).toHaveBeenCalledTimes(1);
		expect(adapters[0]!.close).not.toHaveBeenCalled();
	});

	it('releases a hidden tab and reconnects when it becomes visible again', async () => {
		const factory = setup();
		card();
		await vi.waitFor(() => expect(factory).toHaveBeenCalledTimes(1));
		const visibility = vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('hidden');
		document.dispatchEvent(new Event('visibilitychange'));
		await vi.waitFor(() => expect(adapters[0]!.close).toHaveBeenCalledTimes(1));
		visibility.mockReturnValue('visible');
		document.dispatchEvent(new Event('visibilitychange'));
		await vi.waitFor(() => expect(factory).toHaveBeenCalledTimes(2));
	});

	it('inherits theme changes and fits a 320-pixel dashboard column', async () => {
		setup();
		const element = card();
		element.style.width = '320px';
		element.style.setProperty('--ha-card-background', 'rgb(240, 245, 240)');
		await vi.waitFor(() => expect(element.shadowRoot!.querySelector('video')).not.toBeNull());
		const surface = element.shadowRoot!.querySelector<HTMLElement>('.keeppeek-card')!;
		expect(getComputedStyle(surface).backgroundColor).toBe('rgb(240, 245, 240)');
		element.style.setProperty('--ha-card-background', 'rgb(35, 37, 40)');
		expect(getComputedStyle(surface).backgroundColor).toBe('rgb(35, 37, 40)');
		expect(surface.scrollWidth).toBeLessThanOrEqual(320);
	});
});
