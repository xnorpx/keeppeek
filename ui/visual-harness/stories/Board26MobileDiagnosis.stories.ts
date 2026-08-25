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
			referenceSha256: 'fad4120467ee3b2ee2b33edc33b7523571008c390f5eb600ac6351ea3cb561e2'
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
			referenceSha256: 'ee90a7d58f9992adca50a217a18b9d85ce70cbe39662c735bf6dade19b6df7a7'
		}
	}
};
