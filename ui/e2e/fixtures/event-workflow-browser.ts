import { create } from '@bufbuild/protobuf';
import { timestampFromDate } from '@bufbuild/protobuf/wkt';
import type {
	EventBookmarkChange,
	EventWorkflowResult,
	EventWorkflowTarget
} from '../../src/lib/event-workflow';
import {
	EventOrigin,
	EventSchema,
	PublishEventSchema,
	type Ok,
	type Request,
	type ServerCapabilities
} from '../../src/lib/proto/webrtc_pb';

async function fixtureClient() {
	const modulePath = '/src/lib/control-client.ts';
	const loaded = (await import(modulePath)) as {
		ControlClient: new () => {
			getCameras(): Promise<{ id: string }[]>;
			getServerCapabilities(): Promise<ServerCapabilities>;
			request(command: Request['command']): Promise<Ok['result']>;
			getEventWorkflow(targets: readonly EventWorkflowTarget[]): Promise<EventWorkflowResult>;
			bookmarkEvent(change: EventBookmarkChange): Promise<EventWorkflowResult>;
		};
	};
	return new loaded.ControlClient();
}

export async function seedEventWorkflow(count: number) {
	if (count < 1 || count > 32) throw new Error('Workflow fixture count is out of bounds.');
	const client = await fixtureClient();
	const cameras = await client.getCameras();
	const capabilities = await client.getServerCapabilities();
	const source = capabilities.sourceSessions.find(
		(item) => item.sourceSessionId !== capabilities.selfSourceSessionId
	);
	const camera = cameras[0];
	if (!camera || !source) throw new Error('The real camera fixture is not ready.');
	const zone = `workflow-${crypto.randomUUID()}`;
	const date = '2026-08-18';
	for (let index = 0; index < count; index += 1) {
		await client.request({
			case: 'publishEvent',
			value: create(PublishEventSchema, {
				event: create(EventSchema, {
					eventId: `${zone}-${index}`,
					revision: 1n,
					sourceId: camera.id,
					sourceSessionId: source.sourceSessionId,
					origin: EventOrigin.KEEPPEEK,
					eventType: 'person',
					zone,
					startTime: timestampFromDate(new Date(Date.parse(`${date}T12:00:00Z`) + index * 1000))
				})
			})
		});
	}
	return { date, zone, sourceId: camera.id };
}

export async function updateBookmarkNote(
	target: { sourceId: string; eventId: string },
	note: string
) {
	const client = await fixtureClient();
	const result = await client.getEventWorkflow([target]);
	const current = result.states[0];
	if (!current?.bookmark) throw new Error('Expected a shared bookmark.');
	await client.bookmarkEvent({
		...target,
		expectedRevision: current.bookmark.revision,
		active: true,
		note
	});
}
