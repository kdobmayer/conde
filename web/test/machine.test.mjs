// The Machine page's words per state of `machine.status`, the elapsed line, the setup button, the
// sign-in lines, the sign-in relay's buttons and lines per state, and the server clock's skew.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { UNKNOWN, agentName, elapsed, machineView, settle, signInView, skew } from '../src/lib/machine.ts';

// the session's agent name, the board's one constant
const AGENT = 'praxy';

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

test('no record offers the setup step, naming the agent from the session', () => {
	const v = machineView(null, NOW, null, AGENT);
	assert.equal(v.headline, 'Praxy has no machine yet.');
	assert.equal(v.detail, 'Set it up and the board creates a machine for Praxy; it takes a few minutes.');
	assert.equal(v.setup, 'Set up Praxy');
	assert.equal(agentName(AGENT), 'Praxy');
	assert.equal(machineView(null, NOW, null, 'helper').headline, 'Helper has no machine yet.', 'the name is the passed one');
});

test('no view says "your agent"', () => {
	const states = ['requested', 'failed', 'not_connected', 'up', 'connected', 'ready', 'disconnected', 'deleting'];
	const views = [machineView(null, NOW, null, AGENT), ...states.map((s) => machineView(machine(s), NOW, at(200), AGENT))];
	for (const v of views) {
		const text = `${v.headline} ${v.detail} ${v.setup ?? ''}`;
		assert.ok(!/your agent/i.test(text), text);
	}
});

test('every state has its words, and only a failed or unsent request offers the retry', () => {
	const words = (state, o) => {
		const v = machineView(machine(state, o), NOW, at(200), AGENT);
		return [v.headline, v.detail, v.setup];
	};
	assert.deepEqual(words('requested', { deploy_state: 'Running' }), [
		'Praxy’s machine is being set up.',
		'Requested 7 min ago · Azure: running.',
		null
	]);
	assert.deepEqual(words('requested', { requested_at: null }), [
		'Praxy’s machine is being set up.',
		'The request has not reached Azure yet.',
		'Try again'
	]);
	assert.deepEqual(words('failed', { deploy_error: 'SkuNotAvailable' }), [
		'Setting up Praxy’s machine failed.',
		'SkuNotAvailable',
		'Try again'
	]);
	assert.deepEqual(words('not_connected', { requested_at: at(1900), admins: ['desk', 'krisztian-dobmayer'] }), [
		'Praxy’s machine has not connected.',
		'It did not join the board within 30 minutes. It was requested 31 min ago. Ask desk and krisztian-dobmayer to look at it.',
		null
	]);
	assert.equal(words('up')[0], 'Praxy’s machine is up.');
	assert.deepEqual(words('connected').slice(0, 2), ['Praxy’s machine is connected.', 'Praxy is not up yet.']);
	assert.deepEqual(words('ready', { ready_at: at(65) }).slice(0, 2), ['Praxy is online.', 'It answered the board 1 min ago.']);
	assert.deepEqual(words('disconnected').slice(0, 2), ['Praxy’s machine is offline.', 'Last heard from 3 min ago.']);
	assert.equal(words('deleting')[0], 'Praxy’s machine is being removed.');
});

test('the Claude sign-in shows while connected and not known ok; expiring and expired flows are named', () => {
	assert.equal(machineView(machine('connected'), NOW, null, AGENT).claude, true, 'unknown');
	assert.equal(machineView(machine('connected', { sign_in: { claude: 'ok' } }), NOW, null, AGENT).claude, false);
	assert.equal(machineView(machine('ready'), NOW, null, AGENT).claude, false);
	const done = { relay_id: 'r1', flow: 'claude', state: 'done', url: null, user_code: null, expires_at: null, reason: null, at: at(5) };
	assert.equal(machineView(machine('connected', { relay: done }), NOW, null, AGENT).claude, false, 'just signed in through the page');
	assert.equal(machineView(machine('connected', { relay: { ...done, flow: 'github' } }), NOW, null, AGENT).claude, true);
	const v = machineView(machine('ready', { sign_in: { claude: 'expiring', github: 'expired', codex: 'ok' } }), NOW, null, AGENT);
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

test('GitHub and Codex show the device code, its page and how long it is good for, while it shows and while it waits', () => {
	const run = (flow, state) =>
		signInView(machine('ready', { relay: relay(flow, state, { url: 'https://github.com/login/device', user_code: 'WDJB-MJHT', expires_at: new Date(NOW + 14 * 60_000).toISOString() }) }), NOW)
			.running;
	const gh = run('github', 'url');
	assert.deepEqual([gh.text, gh.link, gh.userCode, gh.paste], ['Open GitHub’s page and type this code there. The code is good for 14 min.', 'github.com/login/device', 'WDJB-MJHT', false]);
	assert.equal(run('codex', 'url').text, 'Open OpenAI’s page and type this code there. The code is good for 14 min.');
	// the helpers print `waiting` right after the code: the person still has to type it
	for (const [flow, site] of [['github', 'GitHub'], ['codex', 'OpenAI']]) {
		const w = run(flow, 'waiting');
		assert.deepEqual(
			[w.text, w.url, w.link, w.userCode, w.paste],
			[`Open ${site}’s page and type this code there. The code is good for 14 min. Waiting for your approval on ${site}…`, 'https://github.com/login/device', 'github.com/login/device', 'WDJB-MJHT', false],
			flow
		);
	}
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

const row = (m, flow) => {
	const r = signInView(m, NOW).start.find((f) => f.flow === flow);
	return r && { label: r.label, status: r.status, warn: r.warn, again: r.again };
};
const signedIn = (sign_in, o = {}) => machine('ready', { sign_in, ...o });

test('a signed-in flow says so and offers a quiet sign-in again in place of its button', () => {
	assert.deepEqual(row(signedIn({ claude: 'ok', github: 'ok', codex: 'expired' }), 'claude'), { label: 'Sign in again', status: 'Signed in', warn: false, again: true });
	assert.deepEqual(row(signedIn({ claude: 'ok', github: 'ok', codex: 'expired' }), 'github'), { label: 'Sign in again', status: 'Signed in', warn: false, again: true });
});

test('an expiring Claude sign-in warns and keeps the sign-in button', () => {
	assert.deepEqual(row(signedIn({ claude: 'expiring' }), 'claude'), { label: 'Sign in to Claude', status: 'Sign-in expires within 3 days', warn: true, again: false });
});

test('an expired flow says it expired and keeps the sign-in button', () => {
	assert.deepEqual(row(signedIn({ claude: 'expired', github: 'expired', codex: 'expired' }), 'codex'), { label: 'Sign in to Codex', status: 'Sign-in expired', warn: true, again: false });
	assert.deepEqual(row(signedIn({ github: 'expired' }), 'github'), { label: 'Sign in to GitHub', status: 'Sign-in expired', warn: true, again: false });
});

test('an unknown or unreported flow shows the sign-in button alone', () => {
	const plain = (flow, name) => ({ label: `Sign in to ${name}`, status: null, warn: false, again: false });
	assert.deepEqual(row(signedIn({ claude: 'unknown' }), 'claude'), plain('claude', 'Claude'));
	assert.deepEqual(row(signedIn(null), 'github'), plain('github', 'GitHub'));
	assert.deepEqual(row(signedIn({ claude: 'ok' }), 'codex'), plain('codex', 'Codex'));
});

test('a finished sign-in reads signed in at once; a later report that it needs the person wins', () => {
	const done = (flow, sign_in) => signedIn(sign_in, { relay: relay(flow, 'done') });
	assert.equal(row(done('github', null), 'github').status, 'Signed in', 'before any report');
	assert.equal(row(done('claude', { claude: 'unknown' }), 'claude').status, 'Signed in');
	assert.equal(row(done('claude', { claude: 'ok' }), 'claude').again, true, 'the server set it ok');
	assert.equal(row(done('github', null), 'claude').status, null, 'only its own flow');
	assert.equal(row(done('claude', { claude: 'expired' }), 'claude').status, 'Sign-in expired');
	assert.equal(row(signedIn(null, { relay: relay('claude', 'failed', { reason: 'timeout' }) }), 'claude').status, null);
});
