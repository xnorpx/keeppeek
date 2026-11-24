import { fromBinary } from '@bufbuild/protobuf';
import type { Page } from '@playwright/test';
import { ControlEnvelopeSchema } from '../../src/lib/proto/webrtc_pb';

export type CardProbe = {
	created: number;
	closed: number;
	packets: number[][];
	overflow: boolean;
	peers: RTCPeerConnection[];
};

export async function installCardProbe(page: Page): Promise<void> {
	await page.addInitScript(() => {
		const probe: CardProbe = { created: 0, closed: 0, packets: [], overflow: false, peers: [] };
		(window as unknown as { keeppeekCardProbe: CardProbe }).keeppeekCardProbe = probe;
		const NativePeer = RTCPeerConnection;
		window.RTCPeerConnection = new Proxy(NativePeer, {
			construct(target, argumentsList, newTarget) {
				const peer = Reflect.construct(target, argumentsList, newTarget) as RTCPeerConnection;
				probe.created += 1;
				probe.peers.push(peer);
				return peer;
			}
		});
		const close = NativePeer.prototype.close;
		const closedPeers = new WeakSet<RTCPeerConnection>();
		NativePeer.prototype.close = function () {
			if (!closedPeers.has(this)) {
				closedPeers.add(this);
				probe.closed += 1;
			}
			Reflect.apply(close, this, []);
		};
		const send = RTCDataChannel.prototype.send;
		RTCDataChannel.prototype.send = function (data: string | Blob | ArrayBuffer | ArrayBufferView) {
			if (this.label === 'control-channel') {
				const bytes =
					data instanceof ArrayBuffer
						? new Uint8Array(data)
						: ArrayBuffer.isView(data)
							? new Uint8Array(data.buffer, data.byteOffset, data.byteLength)
							: null;
				if (bytes && bytes.byteLength <= 65_536 && probe.packets.length < 512)
					probe.packets.push([...bytes]);
				else probe.overflow = true;
			}
			Reflect.apply(send, this, [data]);
		};
	});
}

function recordedPackets(page: Page) {
	return page.evaluate(() => {
		const { created, closed, packets, overflow } = (
			window as unknown as { keeppeekCardProbe: CardProbe }
		).keeppeekCardProbe;
		return { created, closed, packets, overflow };
	});
}

export async function readCardProbe(page: Page) {
	const probe = await recordedPackets(page);
	const commands = probe.packets.map((packet) => {
		const { message } = fromBinary(ControlEnvelopeSchema, new Uint8Array(packet));
		return message.case === 'request' ? message.value.command.case : undefined;
	});
	return {
		created: probe.created,
		closed: probe.closed,
		overflow: probe.overflow,
		subscriptions: commands.filter((command) => command === 'subscribeMedia').length
	};
}

export async function readRecordedCursorBudget(page: Page) {
	const { packets, created, closed, overflow } = await recordedPackets(page);
	if (overflow) throw new Error('The bounded control-packet witness filled.');
	const active = new Map<string, string>();
	let peakCursors = 0;
	let maximumRequestedBufferMs = 0;
	for (const packet of packets) {
		const { message } = fromBinary(ControlEnvelopeSchema, new Uint8Array(packet));
		if (message.case !== 'request' || message.value.command.case !== 'storedMediaCommand') continue;
		const { action } = message.value.command.value;
		if (action.case === 'open') {
			active.set(action.value.storedMediaId, action.value.sourceId);
			peakCursors = Math.max(peakCursors, active.size);
			const duration = action.value.maxBufferDuration;
			if (!duration) throw new Error('The comparison must request an explicit buffer period.');
			maximumRequestedBufferMs = Math.max(
				maximumRequestedBufferMs,
				Number(duration.seconds) * 1000 + duration.nanos / 1_000_000
			);
		} else if (action.case === 'close') active.delete(action.value.storedMediaId);
	}
	return {
		peers: created - closed,
		active: [...active.values()],
		peakCursors,
		maximumRequestedBufferMs
	};
}

export async function interruptCardPeer(page: Page): Promise<void> {
	await page.evaluate(() => {
		const probe = (window as unknown as { keeppeekCardProbe: CardProbe }).keeppeekCardProbe;
		const peer = probe.peers.find((connection) => connection.connectionState !== 'closed');
		if (!peer) throw new Error('No live peer was found.');
		peer.close();
		peer.dispatchEvent(new Event('connectionstatechange'));
	});
}
