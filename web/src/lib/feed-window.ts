/**
 * The Feed window (ia.md "The Feed window"), as a plain module over an injected op and clock.
 *
 * The window is every matching row with `seq > floor`. `floor` and `exhausted` are kept apart from
 * the rows on screen: only the two unbounded-downward calls (a fresh window, an older page) set
 * `exhausted`. A refresh pass re-reads the whole window from `floor` up, page by page, and then
 * replaces the rows (never merges them by id), so a row that changed or stopped matching shows so;
 * only then does the shown watermark advance. A pass reads at most 10 pages; one still answering
 * `more` at the tenth restarts the window from the pass's first page. At most one pass starts per
 * 2 s: a trigger during a pass, or within 2 s of the last start, gets one more pass when allowed.
 */

export interface Row {
	seq: number;
}

export interface Page<T> {
	messages: T[];
	more: boolean;
}

/** `owner.feed` bounds: both exclusive, both optional. */
export interface Bounds {
	after?: number;
	before?: number;
}

export type FeedOp<T> = (bounds: Bounds) => Promise<Page<T>>;

export interface Clock {
	now(): number;
	later(fn: () => void, ms: number): void;
}

export const PASS_PAGES = 10;
export const PASS_EVERY_MS = 2000;

export interface WindowView<T> {
	rows: T[];
	/** Exclusive lower seq bound of the window. */
	floor: number;
	/** Nothing matches below `floor`. */
	exhausted: boolean;
	/** The newest seq shown: feeds "N new" and the per-device last visit; moves only when a pass ends. */
	watermark: number;
	/** The last pass hit its 10-page bound and restarted the window from its first page. */
	restarted: boolean;
	/** The last call failed; the window is the one before it. */
	error: string | null;
	/** Clock time of the last answer the rows come from ("as of"). */
	asOf: number | null;
	/** A fresh window has answered. */
	loaded: boolean;
}

/** An empty window that will not grow downward: "Nothing yet" / "No delivery problems". */
export function isNone<T>(v: WindowView<T>): boolean {
	return v.loaded && v.rows.length === 0 && v.exhausted && !v.error;
}

const newest = <T extends Row>(rows: T[]) => rows[0]?.seq;
const oldest = <T extends Row>(rows: T[]) => rows[rows.length - 1]?.seq;

export class FeedWindow<T extends Row> {
	view: WindowView<T> = {
		rows: [],
		floor: 0,
		exhausted: false,
		watermark: 0,
		restarted: false,
		error: null,
		asOf: null,
		loaded: false
	};
	/** One window operation at a time, in order. */
	private queue: Promise<void> = Promise.resolve();
	/** Bumped by a fresh window: answers of work started before it are dropped. */
	private generation = 0;
	private running = false;
	private scheduled = false;
	private dirty = false;
	private lastStart = -Infinity;

	private readonly op: FeedOp<T>;
	private readonly clock: Clock;
	private readonly changed: (view: WindowView<T>) => void;

	constructor(op: FeedOp<T>, clock: Clock, changed: (view: WindowView<T>) => void = () => {}) {
		this.op = op;
		this.clock = clock;
		this.changed = changed;
	}

	private set(view: WindowView<T>): void {
		this.view = view;
		this.changed(view);
	}

	private fail(e: unknown): void {
		this.set({ ...this.view, error: (e as Error).message ?? String(e) });
	}

	private enqueue(work: () => Promise<void>): Promise<void> {
		this.queue = this.queue.then(work, work);
		return this.queue;
	}

	/** On load, a filter change or a restart: one unbounded call; its rows are the window. */
	fresh(): Promise<void> {
		const generation = ++this.generation;
		return this.enqueue(() => this.load(generation));
	}

	private async load(generation: number): Promise<void> {
		try {
			const page = await this.op({});
			if (generation !== this.generation) return;
			this.set({
				rows: page.messages,
				floor: page.more ? (oldest(page.messages) ?? 1) - 1 : 0,
				exhausted: !page.more,
				watermark: newest(page.messages) ?? 0,
				restarted: false,
				error: null,
				asOf: this.clock.now(),
				loaded: true
			});
		} catch (e) {
			if (generation === this.generation) this.fail(e);
		}
	}

	/** Scrolling down, while not exhausted: the page below `floor`, appended. */
	older(): Promise<void> {
		const generation = this.generation;
		return this.enqueue(async () => {
			if (this.view.exhausted || !this.view.loaded) return;
			try {
				const page = await this.op({ before: this.view.floor + 1 });
				if (generation !== this.generation) return;
				this.set({
					...this.view,
					rows: [...this.view.rows, ...page.messages],
					floor: page.more ? (oldest(page.messages) ?? 1) - 1 : 0,
					exhausted: !page.more,
					error: null
				});
			} catch (e) {
				if (generation === this.generation) this.fail(e);
			}
		});
	}

	/** A board change (`messages` or `delivery`), a reconnect or a poll tick. */
	trigger(): void {
		if (this.running || this.scheduled) {
			this.dirty = true;
			return;
		}
		const wait = this.lastStart + PASS_EVERY_MS - this.clock.now();
		if (wait > 0) {
			this.scheduled = true;
			this.clock.later(() => {
				this.scheduled = false;
				this.start();
			}, wait);
			return;
		}
		this.start();
	}

	private start(): void {
		this.running = true;
		this.dirty = false;
		const generation = this.generation;
		// stamped when the pass begins, not when it is queued behind a fresh window or an older page
		void this.enqueue(() => {
			this.lastStart = this.clock.now();
			return this.pass(generation);
		}).finally(() => {
			this.running = false;
			if (this.dirty) {
				this.dirty = false;
				this.trigger();
			}
		});
	}

	/** Re-reads the window from `floor` up, page by page, then replaces it. */
	private async pass(generation: number): Promise<void> {
		if (!this.view.loaded) return;
		const floor = this.view.floor;
		const rows: T[] = [];
		let first: Page<T> | null = null;
		try {
			for (let n = 0; n < PASS_PAGES; n++) {
				const bounds: Bounds = n === 0 ? { after: floor } : { after: floor, before: oldest(rows) };
				const page = await this.op(bounds);
				if (generation !== this.generation) return;
				first ??= page;
				rows.push(...page.messages);
				if (!page.more || page.messages.length === 0) {
					if (rows.length === 0 && !this.view.exhausted) {
						// every loaded row stopped matching while older ones exist: start over
						await this.load(generation);
						return;
					}
					this.set({
						...this.view,
						rows,
						watermark: newest(rows) ?? this.view.watermark,
						restarted: false,
						error: null,
						asOf: this.clock.now()
					});
					return;
				}
			}
			// The tenth page still has more above `floor`: the first page is exactly the newest
			// unrestricted page, so the window restarts from it with no further call.
			const top = first!.messages;
			this.set({
				...this.view,
				rows: top,
				floor: (oldest(top) ?? 1) - 1,
				exhausted: false,
				watermark: newest(top) ?? this.view.watermark,
				restarted: true,
				error: null,
				asOf: this.clock.now()
			});
		} catch (e) {
			if (generation === this.generation) this.fail(e);
		}
	}
}
