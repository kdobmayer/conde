/** One open thread: the first `owner.thread` answer, then continuations merged by id. */

import { rpc } from './api';
import { emptyThread, mergeThread, type ThreadState } from './thread-tree';
import type { ThreadAnswer } from './types';

export class ThreadView {
	state = $state<ThreadState>(emptyThread());
	loading = $state(false);
	error = $state('');

	constructor(readonly id: string) {}

	/** The first answer: the path to `id` and the subtree's first messages. */
	async load(): Promise<void> {
		await this.fetch({ id: this.id });
	}

	/** The next part of the subtree, while there is one. */
	async more(): Promise<void> {
		if (this.state.next === null) return;
		await this.fetch({ id: this.id, after: this.state.next });
	}

	private async fetch(body: { id: string; after?: number }): Promise<void> {
		this.loading = true;
		try {
			this.state = mergeThread(this.state, await rpc<ThreadAnswer>('owner.thread', body));
			this.error = '';
		} catch (e) {
			this.error = (e as Error).message;
		} finally {
			this.loading = false;
		}
	}
}
