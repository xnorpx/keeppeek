import type { Meta, StoryObj } from '@storybook/svelte';
import Board30CameraDiagnosisStory from './Board30CameraDiagnosisStory.svelte';

const meta = {
	title: 'Health/Camera Diagnosis',
	component: Board30CameraDiagnosisStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 30 desktop camera diagnosis rendered from the shared production evidence model, with unsupported history, retry, credential probe, and configuration actions kept explicit.'
			}
		},
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '4ZU-0',
			frameId: '507-0',
			scenarioId: 'health.desktop.camera-diagnosis',
			reference: 'references/30-camera-diagnosis.png',
			referenceSha256: '0ab1125972a5a523023d2f7ba16847073c2c815200ae507da5076da020b18b5e',
			exceptions: [
				'Server health does not expose packet-loss history, recording-gap start, or retry countdown.',
				'No credential probe command exists, so the story keeps that action unavailable.',
				'Transport mutation remains gated by keeppeek.runtime-config.v1 and is sent over WebRTC when advertised.'
			]
		}
	}
} satisfies Meta<typeof Board30CameraDiagnosisStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const OutageEvidence: Story = {};
