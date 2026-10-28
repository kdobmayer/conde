/**
 * Shared state of the app: session, the rail, the fleet, and the live link.
 * The server's event socket says *that* something changed (topics), never what; views
 * refetch what they show. Without the socket the app polls every 15 s.
 */

import { currentSession, loadSession, onSession, rpc, signIn } from './api';
import type { Fleet, Rail, Session } from './types';

export type Topic = 'messages' | 'delivery' | 'fleet';
const ALL: Topic[] = ['messages', 'delivery', 'fleet'];
const POLL_MS = 15_000;

export const board = $state({
	session: null as Session | null,
	rail: null as Rail | null,
	fleet: null as Fleet | null,
	/** Server clock minus this browser's clock, from the last fleet answer. */
	skewMs: 0,
	live: false,
	/** The last `owner.list` failure, cleared by the next success. */
	error: '' as string,
	/** The last `fleet.list` failure, cleared by the next success. */
	fleetError: '' as string,
	/** Ticks once a second: relative times and retry clocks read it. */
	now: Date.now()
});

/**
 * Every session load lands here. A new token for the same owner just replaces the session; a
 * different owner (signed in from another tab) makes everything on screen the old owner's
 * (rail, routes, open conversation), so the app starts over at home.
 */
onSession((s) => {
	if (board.session && board.session.user !== s.user) {
		location.replace('/');
		return;
	}
	board.session = s;
});

/** Whether answers now come for the owner on screen (false while switching to another one). */
export function sameOwner(): boolean {
	return board.session !== null && currentSession()?.user === board.session.user;
}

type Listener = (topics: Topic[]) => void;
const listeners = new Set<Listener>();

/** Calls `fn` on every board change (and on every poll while the socket is down). */
export function onBoard(fn: Listener): () => void {
	listeners.add(fn);
	return () => listeners.delete(fn);
}

let pending = new Set<Topic>();
let timer: ReturnType<typeof setTimeout> | null = null;

/** Gathers changes for a moment, then tells every listener once. */
export function changed(topics: Topic[]): void {
	for (const t of topics) pending.add(t);
	if (timer) return;
	timer = setTimeout(() => {
		timer = null;
		const batch = [...pending];
		pending = new Set();
		if (batch.includes('messages')) void refreshRail();
		if (batch.includes('fleet') || batch.includes('delivery')) void refreshFleet();
		for (const fn of listeners) fn(batch);
	}, 150);
}

export async function refreshRail(): Promise<void> {
	try {
		const rail = await rpc<Rail>('owner.list');
		if (!sameOwner()) return;
		board.rail = rail;
		board.error = '';
	} catch (e) {
		board.error = (e as Error).message;
	}
}

export async function refreshFleet(): Promise<void> {
	try {
		const asked = Date.now();
		const fleet = await rpc<Fleet>('fleet.list');
		if (!sameOwner()) return;
		board.skewMs = Date.parse(fleet.now) - (asked + Date.now()) / 2;
		board.fleet = fleet;
		board.fleetError = '';
	} catch (e) {
		board.fleetError = (e as Error).message;
	}
}

let socket: WebSocket | null = null;
let retryMs = 1000;
let reconnect: ReturnType<typeof setTimeout> | null = null;

function connect(): void {
	if (reconnect) {
		clearTimeout(reconnect);
		reconnect = null;
	}
	if (socket) return;
	const scheme = location.protocol === 'https:' ? 'wss://' : 'ws://';
	const ws = new WebSocket(scheme + location.host + '/v1/events');
	socket = ws;
	ws.onopen = () => {
		board.live = true;
		retryMs = 1000;
		changed(ALL);
	};
	ws.onmessage = (e) => {
		try {
			const frame = JSON.parse(String(e.data)) as { type?: string; topics?: Topic[] };
			if (frame.type === 'changed') changed(frame.topics ?? ALL);
		} catch {
			changed(ALL);
		}
	};
	ws.onclose = (e) => {
		if (socket !== ws) return;
		board.live = false;
		socket = null;
		if (e.code === 4401) {
			signIn();
			return;
		}
		reconnect = setTimeout(connect, retryMs);
		retryMs = Math.min(retryMs * 2, 30_000);
	};
}

let started = false;

/** Loads the session and the rail, opens the event socket and starts the clocks. */
export async function start(): Promise<void> {
	if (started) return;
	started = true;
	await loadSession();
	await Promise.all([refreshRail(), refreshFleet()]);
	connect();
	setInterval(() => (board.now = Date.now()), 1000);
	setInterval(() => {
		if (!board.live) changed(ALL);
	}, POLL_MS);
	document.addEventListener('visibilitychange', () => {
		if (document.visibilityState === 'visible') {
			changed(ALL);
			connect();
		}
	});
}

/** The short form of a canonical name: unqualified inside the viewer's own namespace. */
export function short(name: string): string {
	const me = board.session?.user;
	return me && name.startsWith(me + '/') ? name.slice(me.length + 1) : name;
}

/** The rail link of an agent's conversation: a DM partner or a `#channel`. */
export function threadHref(agent: string, target: string): string {
	const a = encodeURIComponent(short(agent));
	if (target.startsWith('#')) return `#/a/${a}/ch/${encodeURIComponent(target.slice(1))}`;
	const [owner, name] = target.split('/');
	return `#/a/${a}/dm/${encodeURIComponent(owner ?? '')}/${encodeURIComponent(name ?? '')}`;
}
