/**
 * The Feed's live window (ia.md "The Feed window", "Search"): `owner.feed` under the window rule
 * (lib/feed-window.ts), passes on a reconnect, on `messages` and `delivery` frames and on poll
 * ticks (all through `onBoard`), a fresh window on a filter change, "N new" against the top the
 * owner last saw, the per-device last visit, and search as a separate mode.
 */

import { rpc } from './api';
import { onBoard } from './board.svelte';
import { FeedWindow, type Clock, type WindowView } from './feed-window';
import type { FeedAnswer, FeedMessage, SearchAnswer, SearchHit } from './types';

export interface Filters {
	/** One own agent. */
	agent?: string;
	dir?: 'in' | 'out' | 'own';
	/** `dm`, or a channel name without its `#`. */
	place?: string;
	problems?: boolean;
}

/** Filters in the path form (the hash router keeps no query after the hash):
 * `agent=desk;dir=out;place=dm;problems=1`. Unknown or malformed parts are left out. */
export function parseFilters(path: string): Filters {
	const f: Filters = {};
	for (const part of path.split(';')) {
		const [key, raw = ''] = part.split('=');
		let value: string;
		try {
			value = decodeURIComponent(raw);
		} catch {
			continue;
		}
		if (key === 'agent' && value) f.agent = value;
		else if (key === 'dir' && (value === 'in' || value === 'out' || value === 'own')) f.dir = value;
		else if (key === 'place' && value) f.place = value;
		else if (key === 'problems' && value === '1') f.problems = true;
	}
	return f;
}

export function formatFilters(f: Filters): string {
	const parts: string[] = [];
	if (f.agent) parts.push(`agent=${encodeURIComponent(f.agent)}`);
	if (f.dir) parts.push(`dir=${f.dir}`);
	if (f.place) parts.push(`place=${encodeURIComponent(f.place)}`);
	if (f.problems) parts.push('problems=1');
	return parts.join(';');
}

/** The `owner.feed` predicates of a filter set. */
export function feedBody(f: Filters): Record<string, unknown> {
	return {
		agent: f.agent,
		direction: f.dir,
		place: f.place === undefined ? undefined : f.place === 'dm' ? 'dm' : `#${f.place}`,
		problems: f.problems || undefined
	};
}

const SEEN_KEY = 'conde.feed.seen';

/** The highest seq this device has shown, for the "new since your last visit" divider. */
export function readSeen(): number {
	try {
		const n = Number(localStorage.getItem(SEEN_KEY));
		return Number.isSafeInteger(n) && n > 0 ? n : 0;
	} catch {
		return 0;
	}
}

function writeSeen(seq: number): void {
	try {
		localStorage.setItem(SEEN_KEY, String(seq));
	} catch {
		// not remembered on this device
	}
}

const realClock: Clock = { now: () => Date.now(), later: (fn, ms) => void setTimeout(fn, ms) };

export interface SearchState {
	pattern: string;
	hits: SearchHit[];
	next: number | string | null;
	scanned: number | null;
	error: string;
}

export class Feed {
	view = $state<WindowView<FeedMessage>>({
		rows: [],
		floor: 0,
		exhausted: false,
		watermark: 0,
		restarted: false,
		error: null,
		asOf: null,
		loaded: false
	});
	/** Rows above the top the owner last saw ("N new"). */
	fresh = $state(0);
	/** The device's last visit, read once: the divider sits below rows newer than it. */
	readonly lastVisit = readSeen();
	search = $state<SearchState | null>(null);

	private filters: Filters;
	private seenTop = 0;
	private window: FeedWindow<FeedMessage>;
	private off: () => void;

	constructor(filters: Filters, clock: Clock = realClock) {
		this.filters = filters;
		this.window = new FeedWindow<FeedMessage>(
			(bounds) => rpc<FeedAnswer>('owner.feed', { ...feedBody(this.filters), ...bounds }),
			clock,
			(v) => {
				if (this.seenTop === 0) this.seenTop = v.watermark;
				this.view = v;
				this.fresh = v.rows.filter((r) => r.seq > this.seenTop).length;
			}
		);
		this.off = onBoard((topics) => {
			if (topics.includes('messages') || topics.includes('delivery')) this.window.trigger();
		});
		void this.window.fresh();
	}

	/** A filter change starts a fresh window. */
	setFilters(filters: Filters): void {
		this.filters = filters;
		this.seenTop = 0;
		void this.window.fresh();
	}

	older(): Promise<void> {
		return this.window.older();
	}

	/** The owner is at the top: everything shown counts as seen, here and on this device. Neither
	 * moves backward (a narrower filter or a row that stopped matching shows a lower watermark). */
	atTop(): void {
		this.seenTop = Math.max(this.seenTop, this.view.watermark);
		this.fresh = 0;
		if (this.view.watermark > readSeen()) writeSeen(this.view.watermark);
	}

	/** Search mode: `owner.search` replaces the list until it is cleared; filters do not apply. */
	async find(pattern: string): Promise<void> {
		this.search = { pattern, hits: [], next: null, scanned: null, error: '' };
		await this.searchMore();
	}

	async searchMore(): Promise<void> {
		const s = this.search;
		if (!s) return;
		try {
			const r = await rpc<SearchAnswer>('owner.search', { pattern: s.pattern, after: s.next ?? undefined });
			if (this.search !== s) return;
			this.search = { ...s, hits: [...s.hits, ...r.hits], next: r.next_cursor, scanned: r.partial ? (r.scanned ?? null) : null, error: '' };
		} catch (e) {
			if (this.search === s) this.search = { ...s, error: (e as Error).message };
		}
	}

	clearSearch(): void {
		this.search = null;
	}

	destroy(): void {
		this.off();
	}
}
