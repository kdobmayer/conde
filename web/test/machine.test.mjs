// The Machine page's words per state of `machine.status`, the elapsed line, the setup button, the
// sign-in lines, the sign-in relay's buttons and lines per state, the setup steps, and the server clock's skew.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { UNKNOWN, agentName, elapsed, machineView, settle, setupSteps, signInView, skew } from '../src/lib/machine.ts';

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

// the setup steps as [id, state, note] rows; `sending` is the page's own "Set up pressed, no answer yet"
const steps = (m, sending = false) => setupSteps(m, NOW, AGENT, sending).map((s) => [s.id, s.state, s.note]);
const stateOf = (m, id, sending = false) => setupSteps(m, NOW, AGENT, sending).find((s) => s.id === id);
const fresh = (o = {}) => machine('requested', { requested_at: null, ...o });

test('the steps run request, accept, build, join, Claude, online, GitHub, with their labels', () => {
	const list = setupSteps(fresh({ requested_at: at(420), deploy_state: 'Running' }), NOW, AGENT, false);
	assert.deepEqual(
		list.map((s) => [s.id, s.label]),
		[
			['request', 'Request sent'],
			['accept', 'Azure accepts the request'],
			['build', 'Azure builds the machine'],
			['join', 'The machine joins the board'],
			['claude', 'Claude sign-in'],
			['online', 'Praxy is online'],
			['github', 'GitHub sign-in']
		]
	);
});

test('no record shows no steps until Set up is pressed; then the request is being sent', () => {
	assert.deepEqual(setupSteps(null, NOW, AGENT, false), []);
	assert.deepEqual(steps(null, true), [
		['request', 'active', null],
		['accept', 'pending', null],
		['build', 'pending', null],
		['join', 'pending', null],
		['claude', 'pending', null],
		['online', 'pending', null],
		['github', 'pending', null]
	]);
	assert.equal(stateOf(null, 'request', true).label, 'Sending the request');
	assert.equal(stateOf(machine('failed', { deploy_error: 'SkuNotAvailable' }), 'request', true).state, 'active', 'a retry is being sent');
});

test('Azure accepting the request is the active step until the request is accepted, then the deployment status is awaited', () => {
	const unsent = stateOf(fresh(), 'accept');
	assert.deepEqual([unsent.state, unsent.label, unsent.note], ['active', 'Waiting for Azure to accept the request', null]);
	assert.equal(stateOf(fresh(), 'request').state, 'done');
	// requested_at is set when Azure accepted the PUT; the deployment's state comes with the next read
	assert.deepEqual(steps(fresh({ requested_at: at(420) })).slice(0, 4), [
		['request', 'done', null],
		['accept', 'done', null],
		['build', 'active', '7 min since requested'],
		['join', 'pending', null]
	]);
	assert.equal(stateOf(fresh({ requested_at: at(420) }), 'build').label, 'Waiting for deployment status');
	assert.equal(stateOf(fresh({ requested_at: at(420) }), 'accept').label, 'Azure accepts the request');
});

test('Azure builds while the deployment is accepted or running; the time is the total since the request', () => {
	for (const deploy_state of ['Accepted', 'Running']) {
		assert.deepEqual(steps(fresh({ requested_at: at(420), deploy_state })).slice(1, 4), [
			['accept', 'done', null],
			['build', 'active', '7 min since requested'],
			['join', 'pending', null]
		], deploy_state);
		assert.equal(stateOf(fresh({ requested_at: at(420), deploy_state }), 'build').label, 'Azure builds the machine');
	}
});

test('any other deployment state without an error keeps Azure building', () => {
	const b = stateOf(fresh({ requested_at: at(420), deploy_state: 'Created' }), 'build');
	assert.deepEqual([b.state, b.label, b.note], ['active', 'Azure builds the machine', '7 min since requested']);
});

test('a succeeded deployment waits for the machine to join the board', () => {
	assert.deepEqual(steps(fresh({ requested_at: at(900), deploy_state: 'Succeeded' })).slice(2, 5), [
		['build', 'done', null],
		['join', 'active', '15 min since requested'],
		['claude', 'pending', null]
	]);
});

test('a refused request fails the accept step, a failed deployment the build step, each with Azure\'s words', () => {
	assert.deepEqual(steps(machine('failed', { requested_at: null, deploy_error: 'QuotaExceeded' })).slice(0, 3), [
		['request', 'done', null],
		['accept', 'failed', 'QuotaExceeded'],
		['build', 'pending', null]
	]);
	assert.deepEqual(steps(machine('failed', { deploy_state: 'Failed', deploy_error: 'SkuNotAvailable' })).slice(1, 4), [
		['accept', 'done', null],
		['build', 'failed', 'SkuNotAvailable'],
		['join', 'pending', null]
	]);
	assert.equal(stateOf(machine('failed', { deploy_state: 'Canceled' }), 'build').note, 'Azure gave no reason.');
});

test('a machine that did not join in time fails the join step', () => {
	assert.deepEqual(stateOf(machine('not_connected', { requested_at: at(1900), deploy_state: 'Succeeded' }), 'join').state, 'failed');
	assert.equal(stateOf(machine('not_connected', { requested_at: at(1900), deploy_state: 'Succeeded' }), 'join').note, 'It did not join the board within 30 minutes.');
});

test('a joined machine asks for the Claude sign-in before Praxy can be online, and GitHub alongside', () => {
	const deployed = { requested_at: at(1200), deploy_state: 'Succeeded' };
	assert.deepEqual(steps(machine('up', deployed)), [
		['request', 'done', null],
		['accept', 'done', null],
		['build', 'done', null],
		['join', 'done', null],
		['claude', 'turn', 'Your turn: use “Sign in to Claude” below.'],
		['online', 'pending', null],
		['github', 'turn', 'Your turn: use “Sign in to GitHub” below.']
	]);
	assert.deepEqual(steps(machine('connected', { ...deployed, sign_in: { claude: 'ok' } })).slice(4), [
		['claude', 'done', null],
		['online', 'active', '20 min since requested'],
		['github', 'turn', 'Your turn: use “Sign in to GitHub” below.']
	]);
	assert.deepEqual(steps(machine('ready', { ...deployed, sign_in: { claude: 'ok' } })).slice(4), [
		['claude', 'done', null],
		['online', 'done', null],
		['github', 'turn', 'Your turn: use “Sign in to GitHub” below.']
	]);
	assert.equal(stateOf(machine('ready', deployed), 'claude').state, 'done', 'online means the machine signed in to Claude');
	assert.equal(stateOf(machine('connected', { ...deployed, sign_in: { claude: 'expiring' } }), 'claude').state, 'done', 'expiring is still signed in');
});

test('a running sign-in spins its own row; the other row waits for it to end', () => {
	const m = machine('connected', { sign_in: { claude: 'ok' }, relay: relay('github', 'waiting') });
	assert.deepEqual(steps(m).slice(4), [
		['claude', 'done', null],
		['online', 'active', '7 min since requested'],
		['github', 'active', null]
	]);
	const c = machine('up', { relay: relay('claude', 'url', { url: CLAUDE_URL }) });
	assert.deepEqual(steps(c).slice(4), [
		['claude', 'active', null],
		['online', 'pending', null],
		['github', 'turn', 'Your turn, once the running sign-in ends.']
	]);
});

test('signing in again to a signed-in flow spins its row and keeps the list', () => {
	const m = machine('ready', { sign_in: { claude: 'ok', github: 'ok' }, relay: relay('github', 'url', { url: 'https://github.com/login/device' }) });
	assert.equal(stateOf(m, 'github').state, 'active');
	assert.equal(stateOf(m, 'claude').state, 'done');
	assert.equal(stateOf(machine('ready', { sign_in: { claude: 'ok', github: 'ok' }, relay: relay('claude', 'waiting') }), 'claude').state, 'active', 'online does not mask a running Claude sign-in');
	assert.equal(stateOf(machine('connected', { sign_in: { claude: 'expiring' }, relay: relay('claude', 'starting') }), 'claude').state, 'active');
});

test('a sign-in row reads the flow\'s own state: a newer expired report beats an older done relay', () => {
	assert.equal(stateOf(machine('connected', { sign_in: { claude: 'expired' }, relay: relay('claude', 'done') }), 'claude').state, 'turn');
	assert.equal(stateOf(machine('connected', { relay: relay('claude', 'done') }), 'claude').state, 'done', 'done before any report');
	assert.equal(stateOf(machine('connected', { sign_in: { claude: 'unknown' } }), 'claude').state, 'turn', 'unknown is not signed in');
	const failed = stateOf(machine('up', { relay: relay('claude', 'failed', { reason: 'bad_code' }) }), 'claude');
	assert.deepEqual([failed.state, failed.note], ['failed', 'Claude did not take that code. Start again for a new link.']);
	assert.equal(stateOf(machine('up', { sign_in: { claude: 'ok' }, relay: relay('claude', 'failed') }), 'claude').state, 'done', 'a later ok report wins');
});

test('an adopted machine skips the deployment steps, never pending and never claimed done', () => {
	const adopted = machine('up', { requested_at: null, minted_at: at(60), deploy_state: null });
	assert.deepEqual(steps(adopted).slice(0, 4), [
		['request', 'skipped', 'Not needed'],
		['accept', 'skipped', 'Not needed'],
		['build', 'skipped', 'Not needed'],
		['join', 'done', null]
	]);
	assert.deepEqual(setupSteps(machine('ready', { requested_at: null, minted_at: at(60), sign_in: { claude: 'ok', github: 'ok' } }), NOW, AGENT, false), []);
});

test('the steps hide once all is done, and while the machine is offline or being removed', () => {
	assert.deepEqual(setupSteps(machine('ready', { deploy_state: 'Succeeded', sign_in: { claude: 'ok', github: 'ok' } }), NOW, AGENT, false), []);
	assert.deepEqual(setupSteps(machine('ready', { relay: relay('github', 'done') }), NOW, AGENT, false), [], 'GitHub just signed in, Claude by being online');
	assert.deepEqual(setupSteps(machine('disconnected'), NOW, AGENT, false), []);
	assert.deepEqual(setupSteps(machine('deleting'), NOW, AGENT, false), []);
});

test('a failed status read shows the last answer\'s steps, nothing newer', () => {
	const m = fresh({ requested_at: at(420), deploy_state: 'Running' });
	const before = setupSteps(settle(UNKNOWN, { machine: m }).machine, NOW, AGENT, false);
	const after = settle(settle(UNKNOWN, { machine: m }), { error: { code: 'network', message: 'The server could not be reached' } });
	assert.deepEqual(setupSteps(after.machine, NOW, AGENT, false), before);
});
