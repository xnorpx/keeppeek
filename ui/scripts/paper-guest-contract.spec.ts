import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'node:fs';
import { verifyGuestReviewOpenApi, verifyGuestReviewRouter } from './paper-guest-contract';

const openApi = readFileSync(new URL('../../api/openapi.yaml', import.meta.url), 'utf8').replaceAll(
	'\r\n',
	'\n'
);
const router = readFileSync(
	new URL('../../src/server/guest_review/http.rs', import.meta.url),
	'utf8'
);

describe('approved guest HTTP boundary', () => {
	it('accepts the actual dedicated OpenAPI and Rust guest route family', () => {
		expect(() => verifyGuestReviewOpenApi(openApi)).not.toThrow();
		expect(() => verifyGuestReviewRouter(router)).not.toThrow();
	});
	it('rejects an extra guest route or a broadened media method', () => {
		expect(() =>
			verifyGuestReviewOpenApi(openApi.replace('  /guest/media:', '  /guest/cameras:'))
		).toThrow();
		expect(() =>
			verifyGuestReviewOpenApi(
				openApi.replace('  /guest/media:\n    get:', '  /guest/media:\n    post:')
			)
		).toThrow();
	});
	it('rejects inherited normal authority and an alternative ordinary authentication scheme', () => {
		expect(() =>
			verifyGuestReviewOpenApi(
				openApi.replace('      security: []', '      security: [{ BrowserSession: [] }]')
			)
		).toThrow();
		expect(() =>
			verifyGuestReviewOpenApi(
				openApi.replace(
					'      security:\n        - GuestReviewSession: []',
					'      security:\n        - BrowserSession: []'
				)
			)
		).toThrow();
		expect(() =>
			verifyGuestReviewOpenApi(
				openApi.replace(
					'      security:\n        - GuestReviewSession: []',
					'      security:\n        - GuestReviewSession: []\n        - BrowserSession: []'
				)
			)
		).toThrow();
	});
	it('rejects sharing the ordinary cookie or changing the dedicated cookie transport', () => {
		expect(() =>
			verifyGuestReviewOpenApi(
				openApi.replace('name: __Host-keeppeek-guest', 'name: __Host-keeppeek-session')
			)
		).toThrow();
		expect(() =>
			verifyGuestReviewOpenApi(
				openApi.replace(
					'GuestReviewSession:\n      type: apiKey\n      in: cookie',
					'GuestReviewSession:\n      type: apiKey\n      in: header'
				)
			)
		).toThrow();
	});
	it('rejects a broadened Rust router or an ordinary-cookie substitution', () => {
		expect(() =>
			verifyGuestReviewRouter(
				router.replace('("GET", "/guest/media")', '("GET" | "POST", "/guest/media")')
			)
		).toThrow();
		expect(() =>
			verifyGuestReviewRouter(router.replace('/guest/media', '/guest/cameras'))
		).toThrow();
		expect(() =>
			verifyGuestReviewRouter(router.replace('__Host-keeppeek-guest', '__Host-keeppeek-session'))
		).toThrow();
	});
});
