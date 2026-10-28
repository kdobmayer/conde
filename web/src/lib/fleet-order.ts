/**
 * The Fleet page's order (ia.md "Fleet"): the owner's own hosts first, problems first within
 * them; teammates' hosts below, grouped per owner. A teammate's agent is reduced to name, kind
 * and seen here, so the page cannot show an address or delivery health for it.
 */

import type { Fleet, FleetAgent, FleetHost } from './types';

export interface OwnAgent {
	name: string;
	kind: FleetAgent['kind'];
	seen: boolean;
	last_reached: string | null;
	ever_reached: boolean;
	held: number;
	stuck: number;
}

export interface TeamAgent {
	name: string;
	kind: FleetAgent['kind'];
	seen: boolean;
}

export interface HostRow<A> {
	host: string;
	bridge: boolean;
	online: boolean;
	heartbeat: string | null;
	last_seen: string | null;
	agents: A[];
}

export interface OwnHost extends HostRow<OwnAgent> {
	/** Held plus stuck over the host's agents. */
	problems: number;
}

export interface Teammate {
	owner: string;
	hosts: HostRow<TeamAgent>[];
}

export interface FleetView {
	own: OwnHost[];
	teammates: Teammate[];
}

/** Online while the bridge heartbeat is younger than the server's TTL, `now` on the server's clock. */
export function isOnline(h: FleetHost, ttlS: number, now: number): boolean {
	return h.heartbeat !== null && Date.parse(h.heartbeat) + ttlS * 1000 > now;
}

const byName = (a: { name: string }, b: { name: string }) => a.name.localeCompare(b.name);
const byHost = (a: { host: string }, b: { host: string }) => a.host.localeCompare(b.host);
const problems = (a: OwnAgent) => a.held + a.stuck;

export function orderFleet(fleet: Fleet, me: string, now: number): FleetView {
	const own: OwnHost[] = [];
	const team = new Map<string, HostRow<TeamAgent>[]>();
	for (const h of fleet.hosts) {
		const agents = fleet.agents.filter((a) => a.owner === h.owner && a.host === h.host);
		// A host with no bridge and no agent is the browser's own `web` host: nothing to show.
		if (!h.bridge && agents.length === 0) continue;
		const online = isOnline(h, fleet.online_ttl_s, now);
		const seen = (a: FleetAgent) => a.kind !== 'human' && a.discovered !== null && online;
		const row = { host: h.host, bridge: h.bridge, online, heartbeat: h.heartbeat, last_seen: h.last_seen };
		if (h.owner === me) {
			const mine = agents
				.map((a) => ({
					name: a.name,
					kind: a.kind,
					seen: seen(a),
					last_reached: a.last_reached,
					ever_reached: a.ever_reached ?? false,
					held: a.held ?? 0,
					stuck: a.stuck ?? 0
				}))
				.sort((a, b) => Number(problems(b) > 0) - Number(problems(a) > 0) || byName(a, b));
			own.push({ ...row, agents: mine, problems: mine.reduce((n, a) => n + problems(a), 0) });
		} else {
			const theirs = agents.map((a) => ({ name: a.name, kind: a.kind, seen: seen(a) })).sort(byName);
			const list = team.get(h.owner) ?? [];
			list.push({ ...row, agents: theirs });
			team.set(h.owner, list);
		}
	}
	own.sort((a, b) => Number(b.problems > 0) - Number(a.problems > 0) || byHost(a, b));
	const teammates = [...team.entries()]
		.map(([owner, hosts]) => ({ owner, hosts: hosts.sort(byHost) }))
		.sort((a, b) => a.owner.localeCompare(b.owner));
	return { own, teammates };
}
