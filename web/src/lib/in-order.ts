/**
 * A read sent again on every change, whose answers can land out of order. An answer reaches the
 * screen only when no read sent after it has landed and no local write was made after it was
 * sent, so the screen never goes back to an older state. A newer read in flight does not hold an
 * older answer back: on a busy board every read would be overtaken and none would land.
 */
export function inOrder<T>(read: () => Promise<T>, apply: (answer: T) => void, fail: (e: Error) => void) {
	let sent = 0;
	let landed = 0;
	/** Whether the read numbered `n` is newer than what is on screen; if so, it now is. */
	const lands = (n: number): boolean => {
		if (n <= landed) return false;
		landed = n;
		return true;
	};
	return {
		/** Sends the read; its answer or failure lands unless something newer landed first. */
		async refresh(): Promise<void> {
			const n = ++sent;
			let answer: T;
			try {
				answer = await read();
			} catch (e) {
				if (lands(n)) fail(e as Error);
				return;
			}
			if (lands(n)) apply(answer);
		},
		/** A local write is on screen: the answers of reads sent before it are older. */
		wrote(): void {
			landed = sent;
		}
	};
}
