import type { StateStoreWatch } from './control-client-state-store';

export type ProfileSubscription = Pick<StateStoreWatch, 'close'>;
type Listen<T> = (value: T) => void;
type Open<T> = (emit: Listen<T>) => Promise<ProfileSubscription>;
type Group<T> = {
	listeners: Map<Listen<T>, ((error: unknown) => void) | undefined>;
	open: Open<T>;
	latest?: T;
	epoch: number;
	opened: Promise<void>;
	subscription?: ProfileSubscription;
	paused: boolean;
	failed: boolean;
};

export class SharedProfileWatch<T> {
	private group?: Group<T>;

	private publish(group: Group<T>, value: T): void {
		group.latest = value;
		for (const listener of group.listeners.keys()) listener(value);
	}

	invalidateSnapshot(pending?: T): void {
		const group = this.group;
		if (!group) return;
		if (pending === undefined) delete group.latest;
		else if (group.latest !== pending) this.publish(group, pending);
	}

	private retire(group: Group<T>): void {
		group.epoch++;
		group.paused = true;
		group.opened = group.opened
			.catch(() => {})
			.then(async () => {
				const subscription = group.subscription;
				group.subscription = undefined;
				await subscription?.close();
			});
	}

	pause(pending: T): void {
		const group = this.group;
		if (!group) return;
		if (!group.paused) this.retire(group);
		this.invalidateSnapshot(pending);
	}

	private start(group: Group<T>): Promise<void> {
		group.paused = false;
		group.failed = false;
		const epoch = ++group.epoch;
		const current = () => this.group === group && group.epoch === epoch;
		group.opened = group.opened
			.catch(() => {})
			.then(async () => {
				if (!current()) return;
				const opened = await group.open((value) => {
					if (current()) this.publish(group, value);
				});
				if (current()) group.subscription = opened;
				else await opened.close();
			});
		return group.opened;
	}

	async resume(): Promise<void> {
		const group = this.group;
		if (!group?.paused) return;
		const opened = this.start(group);
		try {
			await opened;
		} catch (error) {
			if (this.group !== group || group.opened !== opened) return;
			group.failed = true;
			for (const onError of group.listeners.values()) onError?.(error);
		}
	}

	async subscribe(
		listener: Listen<T>,
		open: Open<T>,
		onError?: (error: unknown) => void
	): Promise<ProfileSubscription> {
		const receive = (value: T) => listener(value);
		let group = this.group;
		if (!group) {
			group = {
				listeners: new Map(),
				open,
				epoch: 0,
				opened: Promise.resolve(),
				paused: false,
				failed: false
			};
			this.group = group;
			void this.start(group);
		} else if (group.failed) {
			void this.start(group);
		}
		const subscribed = group;
		const close = async () => {
			if (!subscribed.listeners.delete(receive) || subscribed.listeners.size) return;
			if (this.group === subscribed) this.group = undefined;
			this.retire(subscribed);
			await subscribed.opened;
		};
		group.listeners.set(receive, onError);
		try {
			if (group.latest !== undefined) receive(group.latest);
			await group.opened;
		} catch (cause) {
			await close();
			throw cause;
		}
		return { close };
	}
}
