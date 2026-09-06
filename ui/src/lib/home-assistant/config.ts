export const maxSources = 16;

export type CardSource = {
	source_id: string;
	title?: string;
	quality: 'auto' | 'low' | 'high';
};

export type CardConfig = {
	type: 'custom:keeppeek-card';
	endpoint: string;
	token: string;
	sources: CardSource[];
	title?: string;
	layout: 'grid' | 'single';
	columns: number;
	aspect_ratio: '16:9' | '4:3' | '1:1';
	show_name: boolean;
	view: 'live';
};

export function configObject(value: unknown): Record<string, unknown> {
	if (!value || typeof value !== 'object' || Array.isArray(value)) {
		throw new Error('Card configuration must be an object.');
	}
	return value as Record<string, unknown>;
}

export function parseEndpoint(value: unknown): string {
	const message = 'Set endpoint to an HTTPS KeepPeek URL without credentials, query, or fragment.';
	if (typeof value !== 'string' || value.length > 2048) throw new Error(message);
	let endpoint: URL;
	try {
		endpoint = new URL(value);
	} catch {
		throw new Error(message);
	}
	const loopback = ['localhost', '127.0.0.1', '[::1]'].includes(endpoint.hostname);
	if (endpoint.protocol !== 'https:' && !(loopback && endpoint.protocol === 'http:')) {
		throw new Error(message);
	}
	if (endpoint.username || endpoint.password || endpoint.search || endpoint.hash) {
		throw new Error(message);
	}
	return endpoint.href.replace(/\/+$/, '');
}

function text(value: unknown, name: string): string {
	if (typeof value !== 'string' || !value.trim() || value.length > 160 || /[\r\n\0]/.test(value)) {
		throw new Error(`Set a nonempty ${name} of at most 160 characters.`);
	}
	return value;
}

function choice<const Value extends string>(
	value: unknown,
	choices: readonly Value[],
	fallback: Value,
	name: string
): Value {
	if (value === undefined) return fallback;
	if (!choices.includes(value as Value)) throw new Error(`Select a supported ${name}.`);
	return value as Value;
}

function parseSources(value: unknown): CardSource[] {
	if (!Array.isArray(value) || value.length < 1 || value.length > maxSources) {
		throw new Error(`Select between 1 and ${maxSources} camera sources.`);
	}
	const sources = value.map((item) => {
		const source = configObject(item);
		return {
			source_id: text(source.source_id, 'source ID'),
			...(source.title === undefined ? {} : { title: text(source.title, 'source title') }),
			quality: choice(source.quality, ['auto', 'low', 'high'], 'auto', 'video quality')
		};
	});
	if (new Set(sources.map((source) => source.source_id)).size !== sources.length) {
		throw new Error('Select each source ID only once per card.');
	}
	return sources;
}

export function parseCardConfig(value: unknown): CardConfig {
	const config = configObject(value);
	if (config.type !== 'custom:keeppeek-card') throw new Error('Set type to custom:keeppeek-card.');
	const endpoint = parseEndpoint(config.endpoint);
	if (typeof config.token !== 'string' || !/^[\x21-\x7e]{1,512}$/.test(config.token)) {
		throw new Error('Set a KeepPeek access key in token. YAML secret references must be resolved.');
	}
	const columns = config.columns ?? 2;
	if (typeof columns !== 'number' || !Number.isInteger(columns) || columns < 1 || columns > 4) {
		throw new Error('Set columns to an integer between 1 and 4.');
	}
	if (config.show_name !== undefined && typeof config.show_name !== 'boolean') {
		throw new Error('Set show_name to true or false.');
	}
	return {
		type: 'custom:keeppeek-card',
		endpoint,
		token: config.token,
		sources: parseSources(config.sources),
		...(config.title === undefined ? {} : { title: text(config.title, 'card title') }),
		layout: choice(config.layout, ['grid', 'single'], 'grid', 'layout'),
		columns,
		aspect_ratio: choice(config.aspect_ratio, ['16:9', '4:3', '1:1'], '16:9', 'aspect ratio'),
		show_name: config.show_name ?? true,
		view: choice(config.view, ['live'], 'live', 'view')
	};
}
