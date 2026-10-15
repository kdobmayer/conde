/**
 * A message's one delivery word (ia.md "Feed": the worst recipient state). Problems are
 * retrying, held, denied, refused and gone; otherwise the word says how far it got: read,
 * delivered, read N of M on a channel post, posted.
 */

import type { Message } from '../types';

export type Tone = 'problem' | 'pending' | 'done';

export interface StateWord {
	word: string;
	tone: Tone;
}

/** Recipient states, worst first, with their word and tone. */
const RANK: [string, StateWord][] = [
	['gone', { word: 'gone', tone: 'problem' }],
	['refused', { word: 'refused', tone: 'problem' }],
	['denied', { word: 'denied', tone: 'problem' }],
	['held', { word: 'held', tone: 'problem' }],
	['retrying', { word: 'retrying', tone: 'problem' }],
	['queued', { word: 'queued', tone: 'pending' }],
	['sending', { word: 'sending', tone: 'pending' }],
	['sent', { word: 'sent', tone: 'pending' }],
	['notified', { word: 'delivered', tone: 'done' }],
	['unread', { word: 'delivered', tone: 'done' }],
	['delivered', { word: 'delivered', tone: 'done' }],
	['read', { word: 'read', tone: 'done' }]
];
const rank = new Map(RANK.map(([state], i) => [state, i]));

/** The word for `m`, or null when there is nothing to say (a DM with no delivery known). */
export function stateWord(m: Pick<Message, 'place' | 'delivery'>): StateWord | null {
	const states = (m.delivery?.recipients ?? []).map((r) => r.state);
	const known = states.filter((s) => rank.has(s)).sort((a, b) => rank.get(a)! - rank.get(b)!);
	const worst = known[0] === undefined ? null : RANK[rank.get(known[0])!]![1];
	if (worst?.tone === 'problem') return worst;
	const readBy = m.delivery?.read_by;
	if (readBy) return { word: `read ${readBy.read} of ${readBy.total}`, tone: 'done' };
	if (worst) return worst;
	return m.place === 'dm' ? null : { word: 'posted', tone: 'done' };
}
