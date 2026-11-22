export const guestReviewHttpPaths = ['/share', '/guest/session', '/guest/media', '/guest/download'];
const guestOperations = [
	'DELETE /guest/session',
	'GET /guest/download',
	'GET /guest/media',
	'GET /guest/session',
	'GET /share',
	'POST /guest/session'
];

export function verifyGuestReviewOpenApi(source: string): void {
	const routes = [
		...source.matchAll(/^  (\/[^\s:]+):\r?\n([\s\S]*?)(?=^  \/|^components:|(?![\s\S]))/gm)
	].filter((match) => /^\/(?:guest|share)(?:\/|$)/.test(match[1]));
	if (routes.map((match) => match[1]).join('|') !== guestReviewHttpPaths.join('|'))
		throw new Error('Guest OpenAPI route boundary changed');
	const operations: string[] = [];
	for (const route of routes) {
		for (const operation of route[2].matchAll(
			/^    ([a-z]+):\r?\n([\s\S]*?)(?=^    \S|(?![\s\S]))/gm
		)) {
			const method = operation[1].toUpperCase();
			operations.push(`${method} ${route[1]}`);
			const security = operation[2]
				.match(/^      security:([^\r\n]*(?:\r?\n {8,}[^\r\n]*)*)/m)?.[1]
				.replace(/\s/g, '');
			const expected = route[1] === '/share' || method === 'POST' ? '[]' : '-GuestReviewSession:[]';
			if (security !== expected) throw new Error('Guest OpenAPI authentication boundary changed');
		}
	}
	if (operations.sort().join('|') !== guestOperations.join('|'))
		throw new Error('Guest OpenAPI method boundary changed');
	const cookie = source.match(
		/^    GuestReviewSession:\r?\n([\s\S]*?)(?=^    \S|(?![\s\S]))/m
	)?.[1];
	for (const field of ['type: apiKey', 'in: cookie', 'name: __Host-keeppeek-guest']) {
		if (!cookie?.split(/\r?\n/).includes(`      ${field}`))
			throw new Error('Guest OpenAPI cookie boundary changed');
	}
}

export function verifyGuestReviewRouter(source: string): void {
	const operations = new Set<string>();
	for (const match of source.matchAll(
		/\("([A-Z]+)"(?:\s*\|\s*"([A-Z]+)")?,\s*"(\/guest\/[^"\r\n]+)"\)/g
	)) {
		operations.add(`${match[1]} ${match[3]}`);
		if (match[2]) operations.add(`${match[2]} ${match[3]}`);
	}
	if (
		[...operations].sort().join('|') !==
		guestOperations.filter((operation) => operation !== 'GET /share').join('|')
	)
		throw new Error('Rust guest router exceeds its approved dedicated boundary');
	if (!/const COOKIE: &str = "__Host-keeppeek-guest";/.test(source))
		throw new Error('Rust guest cookie boundary changed');
}
