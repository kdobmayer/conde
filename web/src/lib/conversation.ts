/**
 * The conversation's rules as pure functions (ia.md "DM", "Channels", "Agents"): which messages
 * carry reply context, the one delivery word with its short detail, which own agent reads a
 * channel, and an agent's conversations in order.
 */

import { stateWord, type Tone } from './ui/state.ts';
import { clock, countdown } from './time.ts';
import type { Fleet, FleetAgent, Message, OwnerAgent, Rail } from './types.ts';

/** The parent `messages[i]` replies to, when that parent is not the message just above it. */
export function replyContext(messages: Pick<Message, 'message_id' | 'reply_to'>[], i: number): string | null {
	const parent = messages[i]?.reply_to ?? null;
	if (parent === null || messages[i - 1]?.message_id === parent) return null;
	return parent;
}

export interface DeliveryWord {
	word: string;
	tone: Tone;
	/** What the word needs to be exact: the read time, the retry clock, the failure reason. */
	detail: string | null;
}

const FAILED = new Set(['denied', 'refused', 'gone']);

/**
 * The message's one delivery word (U8's mapping, `stateWord`) with its detail: "read 10:49" for
 * a single reader, "retrying in 0:23", a failure with its reason. Null when there is nothing to
 * say (a DM with no delivery known).
 */
export function deliveryWord(m: Pick<Message, 'place' | 'delivery'>, now: number): DeliveryWord | null {
	const w = stateWord(m);
	if (!w) return null;
	const recipients = m.delivery?.recipients ?? [];
	let detail: string | null = null;
	if (w.word === 'read' && recipients.length === 1 && recipients[0]!.read_at) {
		detail = clock(recipients[0]!.read_at);
	} else if (w.word === 'retrying') {
		const next = recipients
			.filter((r) => r.state === 'retrying' && r.retry_at)
			.map((r) => r.retry_at!)
			.sort()[0];
		if (next) detail = countdown(next, now);
	} else if (FAILED.has(w.word)) {
		const e = recipients.find((r) => r.state === w.word)?.last_error;
		detail = e ? (e.reason ?? e.status) : null;
	}
	return { word: w.word, tone: w.tone, detail };
}

/** The first line of `text`, cut to `n` characters. */
export function firstLine(text: string, n = 70): string {
	const line = text.split('\n').find((l) => l.trim() !== '')?.trim() ?? '';
	return line.length > n ? `${line.slice(0, n - 1).trimEnd()}…` : line;
}

/** The first own agent that is a member of `channel`; the history is the same through any. */
export function channelAgent(rail: Rail | null, channel: string): string | null {
	return rail?.agents.find((a) => a.channels.some((c) => c.channel === channel))?.name ?? null;
}

export interface Entry {
	/** An agent's canonical name for a DM, `#name` for a channel. */
	target: string;
	last_seq: number;
	last_at: string | null;
	unread: number;
}

/** An agent's conversations: DMs newest first, then channels newest first. */
export function conversationsOf(a: OwnerAgent): { dms: Entry[]; channels: Entry[] } {
	const newest = (x: Entry, y: Entry) => y.last_seq - x.last_seq;
	return {
		dms: a.dms.map((d) => ({ target: d.with, last_seq: d.last_seq, last_at: d.last_at, unread: d.unread })).sort(newest),
		channels: a.channels
			.map((c) => ({ target: c.channel, last_seq: c.last_seq, last_at: c.last_at, unread: c.unread }))
			.sort(newest)
	};
}

/** The agent's most recent conversation, DM or channel; null when it has none. */
export function latestOf(a: OwnerAgent): string | null {
	const { dms, channels } = conversationsOf(a);
	const all = [...dms, ...channels].sort((x, y) => y.last_seq - x.last_seq);
	return all[0]?.target ?? null;
}

export interface AgentFacts {
	kind: FleetAgent['kind'];
	host: string;
	online: boolean;
	last_reached: string | null;
	held: number;
	stuck: number;
}

/** An own agent's fleet facts (`fleet.list`); null while it is on no host or the fleet is not loaded. */
export function agentFacts(fleet: Fleet | null, name: string): AgentFacts | null {
	const a = fleet?.agents.find((x) => x.name === name);
	if (!a) return null;
	const online = fleet!.hosts.some((h) => h.owner === a.owner && h.host === a.host && h.online);
	return { kind: a.kind, host: a.host, online, last_reached: a.last_reached, held: a.held ?? 0, stuck: a.stuck ?? 0 };
}

export const KIND: Record<string, string> = { claude: 'Claude', codex: 'Codex', human: 'Human' };
