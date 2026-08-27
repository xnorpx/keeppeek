import { getContext, setContext } from 'svelte';
import { LivePeer, type LiveSessionTransport } from './stream-peer.svelte';

const LIVE_PEER_CONTEXT = Symbol('keeppeek.stream-peer');

export function setLivePeer(sessionTransport?: LiveSessionTransport): LivePeer {
	return setContext(LIVE_PEER_CONTEXT, new LivePeer(sessionTransport));
}

export function useLivePeer(): LivePeer {
	const peer = getContext<LivePeer | undefined>(LIVE_PEER_CONTEXT);
	if (!peer) throw new Error('Live peer context is unavailable');
	return peer;
}
