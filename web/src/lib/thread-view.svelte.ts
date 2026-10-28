/** One open thread for the page: ThreadReader's state, with loading and the last error. */

import { rpc } from './api';
import { ThreadReader, emptyThread, type ThreadState } from './thread-tree';
import type { ThreadAnswer } from './types';

export class ThreadView {
	state = $state<ThreadState>(emptyThread());
	loading = $state(false);
	error = $state('');
	readonly id: string;
	private readonly reader: ThreadReader;
	private pending = 0;

	constructor(id: string) {
		this.id = id;
		this.reader = new ThreadReader(id, (body) => rpc<ThreadAnswer>('owner.thread', body), (s) => (this.state = s));
	}

	/** The first answer, and every refresh of what is loaded. */
	load(): Promise<void> {
		return this.run(() => this.reader.load());
	}

	/** The next part of the subtree, while there is one. */
	more(): Promise<void> {
		return this.run(() => this.reader.more());
	}

	private async run(read: () => Promise<void>): Promise<void> {
		this.loading = ++this.pending > 0;
		try {
			await read();
			this.error = '';
		} catch (e) {
			this.error = (e as Error).message;
		} finally {
			this.loading = --this.pending > 0;
		}
	}
}
