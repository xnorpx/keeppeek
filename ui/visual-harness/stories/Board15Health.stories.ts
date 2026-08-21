import type { Meta, StoryObj } from '@storybook/svelte';
import Board15HealthStory from './Board15HealthStory.svelte';

const meta = {
	title: 'Health/Server and Client',
	component: Board15HealthStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 15 server, camera-stream, browser-receiver, and machine-readable health evidence rendered from shared production ranking and verdict owners.'
			}
		},
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '2GO-0',
			frameId: '2GY-0',
			scenarioId: 'health.desktop.overview',
			reference: 'references/15-health-server-client.png',
			referenceSha256: 'de44fdef07d7c14bce45237e1fea81f6d6c6461b636f602b2e655f89c69cf68b',
			exceptions: [
				"Health is read via HealthCommand over the WebRTC control channel, not Paper's obsolete GET /health label.",
				'Current stream totals do not expose 24-hour frame loss, writer lag, recorded-today completeness, or exact recording-gap start.',
				'Issue mute and external-service liveness are unavailable; no unsupported action or service inference is shown.'
			]
		}
	}
} satisfies Meta<typeof Board15HealthStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Degraded: Story = {
	name: 'Degraded server and client'
};
