import type { Meta, StoryObj } from '@storybook/svelte';
import Board26MobileDiagnosisStory from './Board26MobileDiagnosisStory.svelte';

const meta = {
	title: 'Health/Mobile Diagnosis',
	component: Board26MobileDiagnosisStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 26 mobile camera diagnosis states rendered from the production compact diagnosis owner and API-shaped WebRTC health evidence.'
			}
		}
	}
} satisfies Meta<typeof Board26MobileDiagnosisStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const OfflineIssue: Story = {
	name: 'Offline issue',
	args: { state: 'issue' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '40I-0',
			frameId: '4EU-0',
			scenarioId: 'health.mobile.camera-issue',
			reference: 'references/26-mobile-health-issue.png',
			referenceSha256: 'c2bc11e1097bbe539b143c423bed2c0e0863e6033ad83e8a45f6db9b3f192889'
		}
	}
};

export const StreamEvidence: Story = {
	name: 'Degraded stream evidence',
	args: { state: 'stream' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '40I-0',
			frameId: '4GF-0',
			scenarioId: 'health.mobile.stream-evidence',
			reference: 'references/26-mobile-stream-evidence.png',
			referenceSha256: 'a1d6c9a37eb78c415524ca3bfc9819746069fdd7465d90810d9e7236d60c03dc'
		}
	}
};
