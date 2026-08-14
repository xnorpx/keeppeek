import { getContext, setContext } from 'svelte';
import { LivePeer } from './live-peer.svelte';

const LIVE_PEER_CONTEXT = Symbol('keeppeek.live-peer');

export function setLivePeer(): LivePeer {
	return setContext(LIVE_PEER_CONTEXT, new LivePeer());
}

export function useLivePeer(): LivePeer {
	const peer = getContext<LivePeer | undefined>(LIVE_PEER_CONTEXT);
	if (!peer) throw new Error('Live peer context is unavailable');
	return peer;
}
