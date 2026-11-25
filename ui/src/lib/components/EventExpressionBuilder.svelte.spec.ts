import { describe, expect, it, vi } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import { render } from 'vitest-browser-svelte';
import EventExpressionBuilder from './EventExpressionBuilder.svelte';
import type { EventExpressionNode } from '$lib/event-expression';

describe('EventExpressionBuilder', () => {
	it('renders plain-language summary, preserves invalid drafts, and exposes provenance & diagnostics safely', async () => {
		const draft: EventExpressionNode = {
			op: 'and',
			children: [
				{ op: 'predicate', predicate: { kind: 'event_types', values: ['person'] } },
				{ op: 'predicate', predicate: { kind: 'confidence', operator: 'at_least', threshold: 0.8 } }
			]
		};
		const onapply = vi.fn();
		let latest: EventExpressionNode = draft;
		await render(EventExpressionBuilder, {
			props: {
				draft,
				cameras: [{ id: 'front-door', name: 'Front Door' }],
				canConvertToCommon: true,
				semanticAvailable: false,
				semanticSummary: {
					modelId: 'clip',
					modelVersion: 'v2',
					producerId: 'cv-service',
					dimensions: 512,
					scoreInterpretation: 'cosine_similarity_higher_is_closer_threshold_before_topk',
					minimumScore: 0.75,
					searchedStartMs: Date.parse('2026-08-18T00:00:00Z'),
					searchedEndMs: Date.parse('2026-08-18T12:00:00Z'),
					candidateCount: 42,
					candidatesTruncated: false
				},
				diagnostics: {
					planCategory: 'indexed_camera_time_hybrid_semantic',
					estimatedCandidates: 100,
					scannedCandidates: 42,
					wallTimeMs: 7
				},
				onchange: (next) => {
					latest = next;
				},
				onapply
			}
		});

		await expect
			.element(page.getByText(/type in \[person\] AND confidence >= 0\.80/))
			.toBeVisible();
		await expect.element(page.getByTestId('semantic-fallback-notice')).toBeVisible();
		await expect.element(page.getByText(/clip@v2/)).toBeVisible();
		await expect.element(page.getByText(/indexed_camera_time_hybrid_semantic/)).toBeVisible();

		await userEvent.click(page.getByRole('button', { name: 'Add condition' }));
		expect(latest.op).toBe('and');
		if (latest.op === 'and') {
			expect(latest.children).toHaveLength(3);
		}
	});

	it('disables submit and shows alert when draft is invalid without resetting it', async () => {
		const invalidDraft: EventExpressionNode = { op: 'and', children: [] };
		await render(EventExpressionBuilder, {
			props: {
				draft: invalidDraft,
				cameras: [],
				canConvertToCommon: false,
				semanticAvailable: true,
				onchange: () => {},
				onapply: () => {}
			}
		});

		await expect.element(page.getByText(/must contain at least one condition/)).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Run advanced search' })).toBeDisabled();
	});
});
