/**
 * The thread's tree (ia.md "Thread view"): the server answers the subtree under `top` sorted by
 * seq; the client merges continuations by id and builds the order. Depth first, siblings by
 * date; a linear chain stays flat and a message with several replies indents them one level. A
 * reply whose parent is not the line just above it carries "re <sender> <time>: <first line>".
 */

import type { Message, ThreadAnswer } from './types';

export interface ThreadState {
	top: string | null;
	start: string | null;
	hiddenParent: boolean;
	/** Every message loaded so far, once each, by seq. */
	messages: Message[];
	/** Where the subtree continues, or null when it is all here. */
	next: number | null;
}

export const emptyThread = (): ThreadState => ({ top: null, start: null, hiddenParent: false, messages: [], next: null });

/** Merges an answer (the first, or a continuation) by id: path messages may come again. */
export function mergeThread(state: ThreadState, answer: ThreadAnswer): ThreadState {
	const byId = new Map(state.messages.map((m) => [m.message_id, m]));
	for (const m of answer.messages) byId.set(m.message_id, m);
	return {
		top: answer.top,
		start: answer.start,
		hiddenParent: answer.hidden_parent,
		messages: [...byId.values()].sort((a, b) => a.seq - b.seq),
		next: answer.next
	};
}

export interface TreeLine {
	message: Message;
	depth: number;
	/** The parent, when it is not the line just above. */
	re: { from: string; date: string; line: string } | null;
}

/** The thread in reading order, from `top`. */
export function treeOrder(messages: Message[], top: string): TreeLine[] {
	const byId = new Map(messages.map((m) => [m.message_id, m]));
	const replies = new Map<string, Message[]>();
	for (const m of messages) {
		if (m.message_id === top || !m.reply_to || !byId.has(m.reply_to)) continue;
		const list = replies.get(m.reply_to) ?? [];
		list.push(m);
		replies.set(m.reply_to, list);
	}
	for (const list of replies.values()) list.sort((a, b) => a.date.localeCompare(b.date) || a.seq - b.seq);
	const out: TreeLine[] = [];
	const walk = (m: Message, depth: number) => {
		const above = out[out.length - 1]?.message;
		const parent = m.reply_to ? byId.get(m.reply_to) : undefined;
		const re =
			parent && above && above !== parent
				? { from: parent.from, date: parent.date, line: parent.text.split('\n')[0] ?? '' }
				: null;
		out.push({ message: m, depth, re });
		const kids = replies.get(m.message_id) ?? [];
		for (const k of kids) walk(k, kids.length > 1 ? depth + 1 : depth);
	};
	const root = byId.get(top);
	if (root) walk(root, 0);
	return out;
}

export type ThreadOp = (body: { id: string; after?: number }) => Promise<ThreadAnswer>;

/**
 * One open thread's reads, one at a time, over an injected op. `load` is the first answer and
 * every refresh: it re-reads the span loaded so far from the start (up to where the retained
 * continuation ended, or all of it when it was complete) and replaces the state with that, so a
 * change on a later page is seen and the continuation frontier is kept. `more` continues from
 * `next`. Reads are queued: an earlier read never lands after a later one.
 */
export class ThreadReader {
	readonly id: string;
	state: ThreadState = emptyThread();
	private readonly op: ThreadOp;
	private readonly changed: (state: ThreadState) => void;
	private queue: Promise<unknown> = Promise.resolve();

	constructor(id: string, op: ThreadOp, changed: (state: ThreadState) => void = () => {}) {
		this.id = id;
		this.op = op;
		this.changed = changed;
	}

	load(): Promise<void> {
		return this.enqueue(async () => {
			// undefined: nothing loaded yet; null: the whole subtree was here
			const frontier = this.state.top === null ? undefined : this.state.next;
			let s = mergeThread(emptyThread(), await this.op({ id: this.id }));
			if (frontier !== undefined) {
				while (s.next !== null && (frontier === null || s.next < frontier)) {
					s = mergeThread(s, await this.op({ id: this.id, after: s.next }));
				}
			}
			this.set(s);
		});
	}

	more(): Promise<void> {
		return this.enqueue(async () => {
			if (this.state.next === null) return;
			this.set(mergeThread(this.state, await this.op({ id: this.id, after: this.state.next })));
		});
	}

	private set(state: ThreadState): void {
		this.state = state;
		this.changed(state);
	}

	private enqueue(step: () => Promise<void>): Promise<void> {
		const run = this.queue.then(step);
		this.queue = run.catch(() => {});
		return run;
	}
}
