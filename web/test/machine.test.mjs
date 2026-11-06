// The Machine page's words per state of `machine.status`, the elapsed line, the setup button, the
// sign-in lines, the sign-in relay's buttons and lines per state, and the server clock's skew.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { UNKNOWN, elapsed, machineView, settle, signInView, skew } from '../src/lib/machine.ts';

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
	const done = { relay_id: 'r1', flow: 'claude', state: 'done', url: null, user_code: null, expires_at: null, reason: null, at: at(5) };
	assert.equal(machineView(machine('connected', { relay: done }), NOW).claude, false, 'just signed in through the page');
	assert.equal(machineView(machine('connected', { relay: { ...done, flow: 'github' } }), NOW).claude, true);
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

test('the page knows nothing until a status answer or not_found; a failed read is an error, never "no machine"', () => {
	let load = settle(UNKNOWN, { error: { code: 'network', message: 'The server could not be reached' } });
	assert.deepEqual(load, { known: false, machine: null, error: 'The server could not be reached' });
	load = settle(load, { error: { code: 'internal', message: 'storage error' } });
	assert.equal(load.known, false);
	load = settle(load, { error: { code: 'not_found', message: 'no machine is set up for you' } });
	assert.deepEqual(load, { known: true, machine: null, error: '' });
	const m = machine('requested');
	load = settle(load, { machine: m });
	assert.deepEqual(load, { known: true, machine: m, error: '' });
	load = settle(load, { error: { code: 'network', message: 'The server could not be reached' } });
	assert.deepEqual(load, { known: true, machine: m, error: 'The server could not be reached' }, 'the last answer stays');
});

const relay = (flow, state, o = {}) => ({ relay_id: 'r1', flow, state, url: null, user_code: null, expires_at: null, reason: null, at: at(5), ...o });
const CLAUDE_URL = 'https://claude.com/cai/oauth/authorize?code=true';

test('the sign-in buttons show while the machine can run one and no sign-in runs', () => {
	const labels = (m) => signInView(m, NOW).start.map((f) => f.label);
	const all = ['Sign in to Claude', 'Sign in to GitHub', 'Sign in to Codex'];
	for (const state of ['up', 'connected', 'ready']) assert.deepEqual(labels(machine(state)), all, state);
	for (const state of ['requested', 'failed', 'not_connected', 'disconnected', 'deleting']) {
		const v = signInView(machine(state, { relay: relay('claude', 'url', { url: CLAUDE_URL }) }), NOW);
		assert.deepEqual(v, { start: [], running: null, outcome: null }, state);
	}
	assert.deepEqual(signInView(null, NOW), { start: [], running: null, outcome: null });
	for (const state of ['starting', 'url', 'waiting']) {
		assert.deepEqual(labels(machine('connected', { relay: relay('github', state) })), [], state);
	}
	assert.deepEqual(labels(machine('connected', { relay: relay('claude', 'cancelled') })), all);
});

test('the Claude sign-in shows its link, takes the pasted code while it shows the link, then checks it', () => {
	const run = (state, o) => signInView(machine('connected', { relay: relay('claude', state, o) }), NOW).running;
	assert.deepEqual(run('starting', { url: null }), {
		relay_id: 'r1',
		flow: 'claude',
		text: 'Starting the Claude sign-in on your machine…',
		url: null,
		link: 'Open the Claude sign-in',
		userCode: null,
		paste: false
	});
	const url = run('url', { url: CLAUDE_URL });
	assert.equal(url.url, CLAUDE_URL);
	assert.equal(url.link, 'Open the Claude sign-in');
	assert.equal(url.paste, true);
	assert.equal(url.text, 'Open the Claude sign-in, sign in as yourself, then paste the code Claude shows you here.');
	const waiting = run('waiting', { url: CLAUDE_URL });
	assert.equal(waiting.paste, false, 'one code per sign-in');
	assert.equal(waiting.text, 'Checking the code with Claude…');
});

test('GitHub and Codex show the device code to type on their page and how long it is good for', () => {
	const run = (flow, state) =>
		signInView(machine('ready', { relay: relay(flow, state, { url: 'https://github.com/login/device', user_code: 'WDJB-MJHT', expires_at: new Date(NOW + 14 * 60_000).toISOString() }) }), NOW)
			.running;
	const gh = run('github', 'url');
	assert.deepEqual([gh.text, gh.link, gh.userCode, gh.paste], ['Open GitHub’s page and type this code there. The code is good for 14 min.', 'github.com/login/device', 'WDJB-MJHT', false]);
	assert.equal(run('codex', 'url').text, 'Open OpenAI’s page and type this code there. The code is good for 14 min.');
	const waiting = run('github', 'waiting');
	assert.deepEqual([waiting.text, waiting.userCode], ['Waiting for you to approve on GitHub…', null]);
	assert.equal(run('codex', 'waiting').text, 'Waiting for you to approve on OpenAI…');
});

test('a finished sign-in says how it ended above the buttons; a cancelled one says nothing', () => {
	const outcome = (flow, state, reason = null) => signInView(machine('connected', { relay: relay(flow, state, { reason }) }), NOW).outcome;
	assert.equal(outcome('claude', 'done'), 'Signed in to Claude.');
	assert.equal(outcome('github', 'done'), 'Signed in to GitHub.');
	assert.equal(outcome('claude', 'failed', 'expired'), 'The Claude sign-in expired. Start it again.');
	assert.equal(outcome('github', 'failed', 'denied'), 'GitHub refused the sign-in.');
	assert.equal(outcome('claude', 'failed', 'bad_code'), 'Claude did not take that code. Start again for a new link.');
	assert.equal(outcome('codex', 'failed', 'timeout'), 'The Codex sign-in timed out. Start it again.');
	assert.equal(outcome('github', 'failed', 'busy'), 'Another GitHub sign-in is running on your machine. Try again in a minute.');
	assert.equal(outcome('codex', 'failed', 'process_exited'), 'The Codex sign-in stopped on your machine. Start it again.');
	assert.equal(outcome('claude', 'cancelled'), null);
	assert.equal(signInView(machine('connected'), NOW).outcome, null);
});
