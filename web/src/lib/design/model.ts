/**
 * Design-only view model over the fixture: the Feed (every message any of the owner's agents
 * can read, newest first), reply-chain threads, one agent's conversations, and display helpers.
 * The shapes are what the proposed `owner.feed` and `owner.thread` ops would answer
 * (docs/web-ui-v2/ia.md); here they are derived in the browser from the fixture.
 */

import * as fx from './fixture.ts';
import type { Message } from '../types.ts';

export type Direction = 'in' | 'out' | 'own';

export interface FeedEntry {
	m: Message;
	/** The owner's agents that can read it (the "via" of the row). */
	via: string[];
	direction: Direction;
	/** Replies under it in the fixture. */
	replies: number;
	/** The state a reader should notice, if any. */
	problem: 'retrying' | 'held' | 'failed' | null;
	/** Summary delivery word for the row. */
	state: string;
}

const own = new Set(fx.ownAgents);
export const isOwn = (name: string) => own.has(name);

function direction(m: Message): Direction {
	const fromOwn = own.has(m.from);
	if (m.place === 'dm') {
		if (fromOwn && own.has(m.to)) return 'own';
		return fromOwn ? 'out' : 'in';
	}
	return fromOwn ? 'out' : 'in';
}

function stateOf(m: Message): { state: string; problem: FeedEntry['problem'] } {
	const r = m.delivery?.recipients ?? [];
	const worst = r.find((x) => ['retrying', 'held', 'denied', 'refused', 'gone'].includes(x.state));
	if (worst) {
		const problem = worst.state === 'retrying' ? 'retrying' : worst.state === 'held' ? 'held' : 'failed';
		return { state: worst.state, problem };
	}
	if (r.length > 0) return { state: r[0]!.state, problem: null };
	const rb = m.delivery?.read_by;
	if (rb) return { state: `read ${rb.read}/${rb.total}`, problem: null };
	return { state: m.place === 'dm' ? 'sent' : 'posted', problem: null };
}

export const feed: FeedEntry[] = fx.messages
	.map((m) => {
		const via = fx.ownAgents.filter((a) => fx.visibleTo(a, m));
		return {
			m,
			via,
			direction: direction(m),
			replies: fx.messages.filter((x) => x.reply_to === m.message_id).length,
			...stateOf(m)
		};
	})
	.filter((e) => e.via.length > 0)
	.sort((a, b) => b.m.seq - a.m.seq);

export const byId = new Map(fx.messages.map((m) => [m.message_id, m]));

/** The reply chain through `id`: its root, then every descendant in date order, with depth. */
export function thread(id: string): { m: Message; depth: number }[] {
	let root = byId.get(id)!;
	while (root.reply_to && byId.has(root.reply_to)) root = byId.get(root.reply_to)!;
	const out: { m: Message; depth: number }[] = [];
	// A linear chain stays flat; a message with several replies indents them one level.
	const walk = (m: Message, depth: number) => {
		out.push({ m, depth });
		const kids = fx.messages.filter((x) => x.reply_to === m.message_id);
		for (const c of kids) walk(c, kids.length > 1 ? depth + 1 : depth);
	};
	walk(root, 0);
	return out; // depth-first, siblings in date order
}

/** One DM conversation of an own agent, oldest first. */
export function dm(agent: string, other: string): Message[] {
	return fx.messages.filter(
		(m) => m.place === 'dm' && ((m.from === agent && m.to === other) || (m.from === other && m.to === agent))
	);
}

export const rail = fx.rail();
export const fleet = fx.fleet;
export const names = fx.names;
export const ids = fx.ids;
export const session = fx.session;
export const channels = fx.channels;
export const NOW = Date.parse(fx.NOW);

export function agentInfo(name: string) {
	const a = fx.fleet.agents.find((x) => x.name === name)!;
	const h = fx.fleet.hosts.find((x) => x.host === a.host && x.owner === a.owner)!;
	return { ...a, online: h.online, heartbeat: h.heartbeat };
}

/** Unqualified inside the owner's namespace. */
export function short(name: string): string {
	return name.startsWith(fx.OWNER + '/') ? name.slice(fx.OWNER.length + 1) : name;
}
/** The name part only. */
export function bare(name: string): string {
	return name.includes('/') ? name.split('/')[1]! : name;
}
export function ownerOf(name: string): string {
	return name.split('/')[0] ?? '';
}

const hm = new Intl.DateTimeFormat('en-GB', { hour: '2-digit', minute: '2-digit', timeZone: 'Europe/Budapest' });
export function clock(iso: string): string {
	return hm.format(new Date(iso));
}
export function ago(iso: string | null): string {
	if (!iso) return 'never';
	const s = Math.max(0, Math.round((NOW - Date.parse(iso)) / 1000));
	if (s < 60) return `${s} s`;
	if (s < 3600) return `${Math.floor(s / 60)} min`;
	if (s < 86_400) return `${Math.floor(s / 3600)} h`;
	return `${Math.floor(s / 86_400)} d`;
}
export function dayLabel(iso: string): string {
	const d = new Date(iso).toLocaleDateString('en-GB', { timeZone: 'Europe/Budapest' });
	const today = new Date(NOW).toLocaleDateString('en-GB', { timeZone: 'Europe/Budapest' });
	const y = new Date(NOW - 86_400_000).toLocaleDateString('en-GB', { timeZone: 'Europe/Budapest' });
	return d === today ? 'Today' : d === y ? 'Yesterday' : d;
}
export function firstLine(text: string, max = 90): string {
	const l = text.split('\n').find((x) => x.trim()) ?? '';
	if (l.length <= max) return l;
	const cut = l.slice(0, max - 1);
	const space = cut.lastIndexOf(' ');
	return (space > max * 0.6 ? cut.slice(0, space) : cut).replace(/[,;:]$/, '') + '…';
}
/** A thread's title: the root's first sentence when it is short, else its first line cut. */
export function headline(text: string, max = 80): string {
	const l = text.split('\n').find((x) => x.trim()) ?? '';
	const end = l.search(/[.?!](\s|$)/);
	return end > 0 && end < max ? l.slice(0, end + 1) : firstLine(l, max);
}
/** The last date in a set of messages. */
export function lastDate(ms: { date: string }[]): string {
	return ms.reduce((a, m) => (m.date > a ? m.date : a), ms[0]?.date ?? '');
}
export function bytes(n: number): string {
	if (n < 1024) return `${n} B`;
	if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
	return `${(n / 1024 / 1024).toFixed(1)} MiB`;
}

/** Counts for filter chips. */
export const counts = {
	all: feed.length,
	in: feed.filter((e) => e.direction === 'in').length,
	out: feed.filter((e) => e.direction === 'out').length,
	own: feed.filter((e) => e.direction === 'own').length,
	problems: feed.filter((e) => e.problem).length,
	dms: feed.filter((e) => e.m.place === 'dm').length,
	channels: feed.filter((e) => e.m.place !== 'dm').length
};

/** The own agents as the nav lists them, with unread totals and state. */
export const agentsNav = rail.agents.map((a) => {
	const info = agentInfo(a.name);
	return {
		name: a.name,
		kind: a.kind,
		host: info.host,
		online: info.online && !!info.discovered,
		unread: [...a.dms, ...a.channels].reduce((n, x) => n + x.unread, 0),
		held: info.held ?? 0,
		stuck: info.stuck ?? 0,
		reached: info.last_reached
	};
});

/** Channels deduped across the owner's agents, with the agents that are members. */
export const channelsNav = Object.entries(fx.channels)
	.map(([name, c]) => ({
		name,
		topic: c.topic,
		members: c.members.length,
		mine: fx.ownAgents.filter((a) => c.members.includes(a)),
		unread: rail.agents.reduce((n, a) => n + (a.channels.find((x) => x.channel === name)?.unread ?? 0), 0),
		last: [...fx.messages].reverse().find((m) => m.to === name)?.date ?? null
	}))
	.filter((c) => c.mine.length > 0);
