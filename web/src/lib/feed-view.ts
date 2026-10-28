/**
 * What the Feed page derives from rows and URLs (ia.md "Feed", "Thread view"): the URL forms, the
 * day sections, the last-visit divider, the thread's headline and time span. Plain functions,
 * so node tests import them.
 */

/** The Feed's history state: the filter path, the open thread, and whether opening it pushed the
 * entry (then closing goes back to the entry below instead of replacing this one). */
export interface FeedState {
	filters?: string;
	thread?: string;
	pushed?: boolean;
}

export const DIRECTION_WORD = { in: 'Received', out: 'Sent', own: 'Between yours' } as const;

export const NOTHING_YET = 'Nothing yet. Messages your agents send or receive on the shared board appear here as they happen.';

/** `#/`, or `#/f/<filters>` for a filtered Feed. */
export function feedHash(filterPath: string): string {
	return filterPath ? `#/f/${filterPath}` : '#/';
}

/** `#/t/<id>`, or `#/f/<filters>/t/<id>` with the Feed filtered behind the thread. */
export function threadHash(filterPath: string, id: string): string {
	return `${filterPath ? `#/f/${filterPath}` : '#'}/t/${encodeURIComponent(id)}`;
}

/** The heading a row sits under: "In the last hour", "Earlier today", "Yesterday", or its date. */
export function feedSection(iso: string, now: number): string {
	const t = Date.parse(iso);
	if (now - t < 3_600_000) return 'In the last hour';
	const d = new Date(t);
	const today = new Date(now);
	if (d.toDateString() === today.toDateString()) return 'Earlier today';
	const yesterday = new Date(now);
	yesterday.setDate(today.getDate() - 1);
	if (d.toDateString() === yesterday.toDateString()) return 'Yesterday';
	return d.toLocaleDateString([], {
		weekday: 'long',
		day: 'numeric',
		month: 'long',
		year: d.getFullYear() === today.getFullYear() ? undefined : 'numeric'
	});
}

/** The index of the first row the device had already shown (the divider goes above it), or -1
 * when there is no last visit or nothing above it is new. */
export function dividerBefore(rows: { seq: number }[], lastVisit: number): number {
	if (lastVisit <= 0) return -1;
	const i = rows.findIndex((r) => r.seq <= lastVisit);
	return i > 0 ? i : -1;
}

/** A thread's headline: the first sentence of its top message's first line, else that line cut. */
export function headline(text: string, max = 80): string {
	const line = (text.split('\n').find((x) => x.trim()) ?? '').trim();
	const end = line.search(/[.?!](\s|$)/);
	if (end > 0 && end < max) return line.slice(0, end + 1);
	return line.length > max ? `${line.slice(0, max - 1).trimEnd()}…` : line;
}

/** "11:10–13:32" within one day, else both ends with their date. */
export function timeSpan(first: string, last: string, clock: (iso: string) => string): string {
	const a = new Date(first);
	const b = new Date(last);
	if (a.toDateString() === b.toDateString()) return first === last ? clock(first) : `${clock(first)}–${clock(last)}`;
	const date = (d: Date) => d.toLocaleDateString([], { month: 'short', day: 'numeric' });
	return `${date(a)} ${clock(first)} – ${date(b)} ${clock(last)}`;
}

/** The active filters in words, for the filtered-to-nothing state. */
export function describeFilters(f: { agent?: string; dir?: 'in' | 'out' | 'own'; place?: string; problems?: boolean }): string {
	const parts: string[] = [];
	if (f.agent) parts.push(`agent ${f.agent}`);
	if (f.dir) parts.push(DIRECTION_WORD[f.dir].toLowerCase());
	if (f.place) parts.push(f.place === 'dm' ? 'direct messages' : `#${f.place}`);
	if (f.problems) parts.push('needs attention');
	return parts.join(' · ');
}
