import type { StateStoreWatch } from './control-client-state-store';

export type ProfileSubscription = Pick<StateStoreWatch, 'close'>;

type Listen<T> = (value: T) => void;
type Group<T> = {
	listeners: Set<Listen<T>>;
	latest?: T;
	opened: Promise<ProfileSubscription>;
};

// The shell and the settings panel share the same recorder connection.
export class SharedProfileWatch<T> {
	private group?: Group<T>;

	invalidateSnapshot(): void {
		if (this.group) delete this.group.latest;
	}

	async subscribe(
		listener: Listen<T>,
		open: (emit: Listen<T>) => Promise<ProfileSubscription>
	): Promise<ProfileSubscription> {
		const receive = (value: T) => listener(value);
		let group = this.group;
		if (!group) {
			const listeners = new Set([receive]);
			const current: Group<T> = {
				listeners,
				opened: Promise.resolve().then(() =>
					open((value) => {
						current.latest = value;
						for (const notify of listeners) notify(value);
					})
				)
			};
			group = this.group = current;
		} else {
			group.listeners.add(receive);
			if (group.latest !== undefined) {
				try {
					receive(group.latest);
				} catch (cause) {
					group.listeners.delete(receive);
					throw cause;
				}
			}
		}
		let underlying: ProfileSubscription;
		try {
			underlying = await group.opened;
		} catch (cause) {
			group.listeners.delete(receive);
			if (this.group === group) this.group = undefined;
			throw cause;
		}
		const subscribed = group;
		return {
			close: async () => {
				if (!subscribed.listeners.delete(receive) || subscribed.listeners.size) return;
				if (this.group === subscribed) this.group = undefined;
				await underlying.close();
			}
		};
	}
}
