// The Machine page's words per state of `machine.status`, the elapsed line, the setup button, the
// sign-in lines, and the server clock's skew.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { elapsed, machineView, skew } from '../src/lib/machine.ts';

const NOW = Date.parse('2026-11-05T22:30:00Z');
const at = (s) => new Date(NOW - s * 1000).toISOString();
const machine = (state, o = {}) => ({
	state,
	host_id: 'vm-agent-krisztian-dobmayer',
	requested_at: at(420),
	minted_at: null,
	deploy_state: null,
	deploy_error: null,
	probe_id: null,
	ready_at: null,
	session_online_at: null,
	sign_in: null,
	relay: null,
	admins: [],
	now: new Date(NOW).toISOString(),
	...o
});

test('no record offers the setup step', () => {
	const v = machineView(null, NOW);
	assert.equal(v.headline, 'Your agent has no machine yet.');
	assert.equal(v.setup, 'Set up my agent');
});

test('every state has its words, and only a failed or unsent request offers the retry', () => {
	const words = (state, o) => {
		const v = machineView(machine(state, o), NOW, at(200));
		return [v.headline, v.detail, v.setup];
	};
	assert.deepEqual(words('requested', { deploy_state: 'Running' }), [
		'Your agent’s machine is being set up.',
		'Requested 7 min ago · Azure: running.',
		null
	]);
	assert.deepEqual(words('requested', { requested_at: null }), [
		'Your agent’s machine is being set up.',
		'The request has not reached Azure yet.',
		'Try again'
	]);
	assert.deepEqual(words('failed', { deploy_error: 'SkuNotAvailable' }), [
		'Setting up your agent’s machine failed.',
		'SkuNotAvailable',
		'Try again'
	]);
	assert.deepEqual(words('not_connected', { requested_at: at(1900), admins: ['desk', 'krisztian-dobmayer'] }), [
		'Your agent’s machine has not connected.',
		'It did not join the board within 30 minutes. It was requested 31 min ago. Ask desk and krisztian-dobmayer to look at it.',
		null
	]);
	assert.equal(words('up')[0], 'Your agent’s machine is up.');
	assert.deepEqual(words('connected').slice(0, 2), ['Your agent’s machine is connected.', 'The agent is not up yet.']);
	assert.deepEqual(words('ready', { ready_at: at(65) }).slice(0, 2), ['Your agent is online.', 'It answered the board 1 min ago.']);
	assert.deepEqual(words('disconnected').slice(0, 2), ['Your agent’s machine is offline.', 'Last heard from 3 min ago.']);
	assert.equal(words('deleting')[0], 'Your agent’s machine is being removed.');
});

test('the Claude sign-in shows while connected and not known ok; expiring and expired flows are named', () => {
	assert.equal(machineView(machine('connected'), NOW).claude, true, 'unknown');
	assert.equal(machineView(machine('connected', { sign_in: { claude: 'ok' } }), NOW).claude, false);
	assert.equal(machineView(machine('ready'), NOW).claude, false);
	const v = machineView(machine('ready', { sign_in: { claude: 'expiring', github: 'expired', codex: 'ok' } }), NOW);
	assert.deepEqual(v.signIn, ['Claude', 'GitHub']);
});

test('elapsed reads seconds, minutes, then hours and minutes; the future reads zero', () => {
	assert.equal(elapsed(at(42), NOW), '42 s');
	assert.equal(elapsed(at(7 * 60 + 10), NOW), '7 min');
	assert.equal(elapsed(at(3900), NOW), '1 h 5 min');
	assert.equal(elapsed(at(-30), NOW), '0 s');
});

test('the skew is the server clock minus the midpoint of the call', () => {
	assert.equal(skew('2026-11-05T22:30:10Z', NOW, NOW + 2000), 9000);
	// a browser clock 5 min behind reads the server's elapsed time with the skew added
	const browser = NOW - 300_000;
	const s = skew(new Date(NOW).toISOString(), browser, browser);
	assert.equal(elapsed(at(60), browser + s), '1 min');
});
