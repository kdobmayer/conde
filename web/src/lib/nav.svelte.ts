/**
 * The navigation model (ia.md "Navigation"): five sections, the owner's agents with their state,
 * and the channels any of them is in, once each.
 */

import type { Fleet, Rail } from './types';

export type SectionId = 'feed' | 'agents' | 'channels' | 'fleet' | 'keys';

export const SECTIONS: { id: SectionId; label: string; href: string }[] = [
	{ id: 'feed', label: 'Feed', href: '#/' },
	{ id: 'agents', label: 'Agents', href: '#/agents' },
	{ id: 'channels', label: 'Channels', href: '#/channels' },
	{ id: 'fleet', label: 'Fleet', href: '#/fleet' },
	{ id: 'keys', label: 'Keys', href: '/keys' }
];

export type AgentState = 'online' | 'offline' | 'unbound';

export interface NavAgent {
	name: string;
	state: AgentState;
	/** From `fleet.list`; null while the agent is unbound or the fleet is not loaded. */
	held: number | null;
	stuck: number | null;
}

/** The owner's agents (`owner.list`) with their state from `fleet.list`: unbound when the agent is
 * on no host, else whether that host's bridge is online. Hosts and agent names are per owner, so
 * both are matched with the owner. */
export function navAgents(rail: Rail | null, fleet: Fleet | null): NavAgent[] {
	const owner = rail?.owner;
	return (rail?.agents ?? []).map((a) => {
		const bound = fleet?.agents.find((f) => f.owner === owner && f.name === a.name);
		const online = bound ? fleet!.hosts.some((h) => h.owner === owner && h.host === bound.host && h.online) : false;
		return {
			name: a.name,
			state: !bound ? 'unbound' : online ? 'online' : 'offline',
			held: bound?.held ?? null,
			stuck: bound?.stuck ?? null
		};
	});
}

/** Every channel one of the owner's agents is in, once, by name. */
export function navChannels(rail: Rail | null): string[] {
	return [...new Set((rail?.agents ?? []).flatMap((a) => a.channels.map((c) => c.channel)))].sort();
}

/** The section a route belongs to; none for a route the app does not have. */
export function sectionOf(routeId: string | null): SectionId | null {
	if (routeId === '/' || routeId?.startsWith('/t/')) return 'feed';
	if (routeId === '/agents' || routeId?.startsWith('/a/')) return 'agents';
	if (routeId === '/channels' || routeId?.startsWith('/c/')) return 'channels';
	if (routeId === '/fleet') return 'fleet';
	return null;
}
