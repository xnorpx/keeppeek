import {
	isServerCapabilityId,
	type ServerCapabilityId,
	unsupportedCapabilityLabel
} from './capabilities';

export type CapabilityCommandPhase =
	'blocked' | 'editing' | 'failed' | 'idle' | 'submitting' | 'succeeded';

export type CapabilityCommandState = {
	commandId: string;
	capability: ServerCapabilityId;
	phase: CapabilityCommandPhase;
	error: string | null;
	capabilityLost: boolean;
};

function commandState(
	commandId: string,
	capability: ServerCapabilityId,
	phase: CapabilityCommandPhase
): CapabilityCommandState {
	return { commandId, capability, phase, error: null, capabilityLost: false };
}

export class CapabilityState {
	#advertised = $state.raw<ReadonlySet<ServerCapabilityId>>(new Set());
	#commands = $state.raw<Readonly<Record<string, CapabilityCommandState>>>({});

	constructor(advertisedCapabilities: Iterable<string> = []) {
		this.updateAdvertised(advertisedCapabilities);
	}

	get advertised(): ReadonlySet<ServerCapabilityId> {
		return this.#advertised;
	}

	get commands(): Readonly<Record<string, CapabilityCommandState>> {
		return this.#commands;
	}

	supports(capability: ServerCapabilityId): boolean {
		return this.#advertised.has(capability);
	}

	label(capability: ServerCapabilityId): string {
		return unsupportedCapabilityLabel(capability);
	}

	command(commandId: string): CapabilityCommandState | null {
		return this.#commands[commandId] ?? null;
	}

	updateAdvertised(advertisedCapabilities: Iterable<string>): void {
		const nextAdvertised = new Set<ServerCapabilityId>();
		for (const capability of advertisedCapabilities) {
			if (isServerCapabilityId(capability)) nextAdvertised.add(capability);
		}
		this.#advertised = nextAdvertised;

		let changed = false;
		const nextCommands = { ...this.#commands };
		for (const [commandId, state] of Object.entries(nextCommands)) {
			if (
				!nextAdvertised.has(state.capability) &&
				(state.phase === 'editing' || state.phase === 'submitting' || state.phase === 'failed')
			) {
				nextCommands[commandId] = {
					...state,
					phase: 'blocked',
					capabilityLost: true
				};
				changed = true;
			}
		}
		if (changed) this.#commands = nextCommands;
	}

	begin(commandId: string, capability: ServerCapabilityId): boolean {
		if (!this.supports(capability)) {
			this.#setCommand(commandState(commandId, capability, 'blocked'));
			return false;
		}
		this.#setCommand(commandState(commandId, capability, 'editing'));
		return true;
	}

	submit(commandId: string): boolean {
		const state = this.#requireCommand(commandId);
		if (!this.supports(state.capability)) {
			this.#setCommand({ ...state, phase: 'blocked', capabilityLost: true });
			return false;
		}
		this.#setCommand({ ...state, phase: 'submitting', error: null, capabilityLost: false });
		return true;
	}

	fail(commandId: string, error: string): void {
		const state = this.#requireCommand(commandId);
		const message = error.trim();
		if (message.length === 0) throw new Error('Command failure must name the server error');
		this.#setCommand({ ...state, phase: 'failed', error: message });
	}

	succeed(commandId: string): void {
		const state = this.#requireCommand(commandId);
		this.#setCommand({ ...state, phase: 'succeeded', error: null, capabilityLost: false });
	}

	reset(commandId: string): void {
		const state = this.#requireCommand(commandId);
		this.#setCommand(commandState(commandId, state.capability, 'idle'));
	}

	#setCommand(state: CapabilityCommandState): void {
		this.#commands = { ...this.#commands, [state.commandId]: state };
	}

	#requireCommand(commandId: string): CapabilityCommandState {
		const state = this.command(commandId);
		if (state === null) throw new Error(`Unknown capability command: ${commandId}`);
		return state;
	}
}
