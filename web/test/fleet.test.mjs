// The Fleet order (ia.md "Fleet"): own hosts first, problems first; teammates per owner, never
// with an address or delivery health.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { orderFleet } from '../src/lib/fleet-order.ts';

const NOW = Date.parse('2026-10-14T12:20:00Z');
const at = (s) => new Date(NOW - s * 1000).toISOString();
const host = (owner, name, heartbeat = 10, bridge = true) => ({
	host: name,
	owner,
	bridge,
	online: true,
	heartbeat: bridge ? at(heartbeat) : null,
	last_seen: bridge ? at(heartbeat) : null
});
const agent = (owner, name, hostName, o = {}) => {
	const own = owner === 'me';
	return {
		name: `${owner}/${name}`,
		owner,
		display: null,
		kind: o.kind ?? 'claude',
		host: hostName,
		address: o.address ?? null,
		pinned: null,
		discovered: o.seen === false ? null : at(5),
		held: own ? (o.held ?? 0) : (o.held ?? null),
		stuck: own ? (o.stuck ?? 0) : (o.stuck ?? null),
		last_reached: own ? at(60) : (o.last_reached ?? null),
		ever_reached: own ? true : null,
		own
	};
};
const fleet = (hosts, agents) => ({ now: new Date(NOW).toISOString(), online_ttl_s: 90, hosts, agents });

test('own hosts come first, then teammates per owner in name order', () => {
	const v = orderFleet(
		fleet(
			[host('zed', 'z1'), host('anna', 'a2'), host('me', 'm1'), host('anna', 'a1')],
			[agent('zed', 'x', 'z1'), agent('anna', 'y', 'a2'), agent('me', 'desk', 'm1'), agent('anna', 'w', 'a1')]
		),
		'me',
		NOW
	);
	assert.deepEqual(v.own.map((h) => h.host), ['m1']);
	assert.deepEqual(
		v.teammates.map((t) => [t.owner, t.hosts.map((h) => h.host)]),
		[
			['anna', ['a1', 'a2']],
			['zed', ['z1']]
		]
	);
});

test('own hosts with held or stuck agents come first, then by name', () => {
	const v = orderFleet(
		fleet(
			[host('me', 'alpha'), host('me', 'beta'), host('me', 'gamma'), host('me', 'delta')],
			[
				agent('me', 'a', 'alpha'),
				agent('me', 'b', 'beta', { stuck: 2 }),
				agent('me', 'g', 'gamma'),
				agent('me', 'd', 'delta', { held: 1 })
			]
		),
		'me',
		NOW
	);
	assert.deepEqual(v.own.map((h) => [h.host, h.problems]), [
		['beta', 2],
		['delta', 1],
		['alpha', 0],
		['gamma', 0]
	]);
});

test('within a host, agents with problems come first, then by name', () => {
	const v = orderFleet(
		fleet(
			[host('me', 'box')],
			[agent('me', 'ann', 'box'), agent('me', 'zoe', 'box', { held: 1 }), agent('me', 'bob', 'box'), agent('me', 'kim', 'box', { stuck: 1 })]
		),
		'me',
		NOW
	);
	assert.deepEqual(v.own[0].agents.map((a) => a.name), ['me/kim', 'me/zoe', 'me/ann', 'me/bob']);
});

test('a teammate agent carries name, kind and seen only, whatever the answer holds', () => {
	const v = orderFleet(
		fleet([host('anna', 'a1')], [agent('anna', 'y', 'a1', { address: 'a1:claude:%3', held: 4, stuck: 1, last_reached: at(9) })]),
		'me',
		NOW
	);
	assert.deepEqual(v.teammates[0].hosts[0].agents, [{ name: 'anna/y', kind: 'claude', seen: true }]);
	assert.equal('problems' in v.teammates[0].hosts[0], false);
});

test('online by heartbeat age on the given clock; seen only on an online host', () => {
	const v = orderFleet(
		fleet(
			[host('me', 'up', 89), host('me', 'down', 91)],
			[agent('me', 'a', 'up'), agent('me', 'b', 'down'), agent('me', 'c', 'up', { seen: false }), agent('me', 'h', 'up', { kind: 'human' })]
		),
		'me',
		NOW
	);
	const byHost = Object.fromEntries(v.own.map((h) => [h.host, h]));
	assert.equal(byHost.up.online, true);
	assert.equal(byHost.down.online, false);
	assert.deepEqual(byHost.up.agents.map((a) => [a.name, a.seen]), [
		['me/a', true],
		['me/c', false],
		['me/h', false]
	]);
	assert.equal(byHost.down.agents[0].seen, false);
});

test('a host with no bridge and no agent is left out; one with agents is kept', () => {
	const v = orderFleet(
		fleet([host('me', 'web', 0, false), host('me', 'fresh', 0, false)], [agent('me', 'new', 'fresh')]),
		'me',
		NOW
	);
	assert.deepEqual(v.own.map((h) => [h.host, h.online]), [['fresh', false]]);
});
