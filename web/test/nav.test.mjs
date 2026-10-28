// The nav model (ia.md "Navigation"): an agent's state comes from its own owner's host, channels
// are listed once, and every route belongs to its section.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { navAgents, navChannels, sectionOf } from '../src/lib/nav.svelte.ts';

const agent = (name, channels = []) => ({ name, kind: 'claude', address: null, dms: [], channels: channels.map((channel) => ({ channel })) });
const bound = (owner, name, host, held = 0) => ({ name: `${owner}/${name}`, owner, host, held, stuck: 0 });

test("an agent on an offline host reads offline though another owner's host of that name is online", () => {
	const rail = { owner: 'alice', agents: [agent('alice/scribe'), agent('alice/idle')] };
	const fleet = {
		hosts: [
			{ host: 'laptop', owner: 'alice', online: false },
			{ host: 'laptop', owner: 'bob', online: true }
		],
		agents: [bound('alice', 'scribe', 'laptop', 2), bound('bob', 'scribe', 'laptop')]
	};
	assert.deepEqual(
		navAgents(rail, fleet).map((a) => [a.name, a.state, a.held]),
		[['alice/scribe', 'offline', 2], ['alice/idle', 'unbound', null]]
	);
	fleet.hosts[0].online = true;
	assert.equal(navAgents(rail, fleet)[0].state, 'online', "its own host's bridge decides");
	assert.equal(navAgents(rail, null)[0].state, 'unbound', 'fleet not loaded yet');
});

test('a channel two own agents share is listed once', () => {
	const rail = { owner: 'alice', agents: [agent('alice/a', ['#dev', '#ops']), agent('alice/b', ['#dev'])] };
	assert.deepEqual(navChannels(rail), ['#dev', '#ops']);
	assert.deepEqual(navChannels(null), []);
});

test('every route belongs to its section', () => {
	const cases = {
		'/': 'feed',
		'/t/[id]': 'feed',
		'/agents': 'agents',
		'/a/[agent]': 'agents',
		'/a/[agent]/dm/[owner]/[name]': 'agents',
		'/channels': 'channels',
		'/c/[name]': 'channels',
		'/fleet': 'fleet'
	};
	for (const [route, section] of Object.entries(cases)) assert.equal(sectionOf(route), section, route);
	assert.equal(sectionOf(null), null, 'no route');
});
