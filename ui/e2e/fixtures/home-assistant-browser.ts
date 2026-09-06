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

export async function readCardProbe(page: Page) {
	const probe = await page.evaluate(() => {
		const { created, closed, packets, overflow } = (
			window as unknown as { keeppeekCardProbe: CardProbe }
		).keeppeekCardProbe;
		return { created, closed, packets, overflow };
	});
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

export async function interruptCardPeer(page: Page): Promise<void> {
	await page.evaluate(() => {
		const probe = (window as unknown as { keeppeekCardProbe: CardProbe }).keeppeekCardProbe;
		const peer = probe.peers.find((connection) => connection.connectionState !== 'closed');
		if (!peer) throw new Error('No live peer was found.');
		peer.close();
		peer.dispatchEvent(new Event('connectionstatechange'));
	});
}
