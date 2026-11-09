/**
 * The Machine page's words (`machine.status`): one headline and one detail line per state, whether
 * the setup button shows and what it says, the sign-in flows that need the person, the sign-in
 * relay's step, and the setup as steps. Times come from the browser clock corrected by the server's skew.
 */

export type MachineState =
	| 'requested'
	| 'failed'
	| 'not_connected'
	| 'up'
	| 'connected'
	| 'ready'
	| 'disconnected'
	| 'deleting';

export interface Machine {
	state: MachineState;
	host_id: string;
	requested_at: string | null;
	minted_at: string | null;
	deploy_state: string | null;
	deploy_error: string | null;
	probe_id: string | null;
	ready_at: string | null;
	session_online_at: string | null;
	/** The flows the machine last reported (`{"claude": "expired", ...}`). */
	sign_in: Record<string, string> | null;
	relay: Relay | null;
	admins: string[];
	now: string;
}

export type Flow = 'claude' | 'github' | 'codex';

/** The machine's one sign-in relay, as `machine.status` reports it. */
export interface Relay {
	relay_id: string;
	flow: Flow;
	state: 'starting' | 'url' | 'waiting' | 'done' | 'failed' | 'cancelled';
	url: string | null;
	/** GitHub's and OpenAI's device code, typed on their page. */
	user_code: string | null;
	expires_at: string | null;
	reason: string | null;
	at: string | null;
}

export interface MachineView {
	headline: string;
	detail: string;
	/** The setup button's label; null when it does not show. */
	setup: string | null;
	/** Flows whose sign-in is expiring or has expired. */
	signIn: string[];
	/** Connected while the Claude sign-in is not known to be ok and the last relay did not just
	 * sign in to Claude: the person signs in. */
	claude: boolean;
}

/** What the page knows: `known` once a status answered or said `not_found` (no machine). */
export interface Load {
	known: boolean;
	machine: Machine | null;
	/** The last failed read, cleared by the next answer. */
	error: string;
}

export const UNKNOWN: Load = { known: false, machine: null, error: '' };

/** The page after one status read: an answer or `not_found` is known; any other failure keeps
 * what was known (nothing, before the first answer) and says why. */
export function settle(prev: Load, read: { machine: Machine } | { error: { code: string; message: string } }): Load {
	if ('machine' in read) return { known: true, machine: read.machine, error: '' };
	if (read.error.code === 'not_found') return { known: true, machine: null, error: '' };
	return { ...prev, error: read.error.message };
}

/** Server clock minus this browser's clock, from an answer's `now` read between `asked` and `answered`. */
export function skew(serverNow: string, asked: number, answered: number): number {
	return Date.parse(serverNow) - (asked + answered) / 2;
}

/** "42 s", "7 min", "1 h 5 min" since `iso`. */
export function elapsed(iso: string, now: number): string {
	const s = Math.max(0, Math.round((now - Date.parse(iso)) / 1000));
	if (s < 60) return `${s} s`;
	const m = Math.floor(s / 60);
	if (m < 60) return `${m} min`;
	return `${Math.floor(m / 60)} h ${m % 60} min`;
}

const FLOWS: Record<string, string> = { claude: 'Claude', github: 'GitHub', codex: 'Codex' };
/** Where a device code is typed. */
const SITES: Record<string, string> = { github: 'GitHub', codex: 'OpenAI' };

/** The person's names joined for a sentence: "a", "a and b", "a, b and c". */
function names(list: string[]): string {
	if (list.length <= 1) return list[0] ?? '';
	return `${list.slice(0, -1).join(', ')} and ${list[list.length - 1]}`;
}

/** The agent's name as the page says it: the session's name with a capital, `Praxy`. */
export function agentName(agent: string): string {
	return agent.charAt(0).toUpperCase() + agent.slice(1);
}

/** What the page says for `m` (null: no record yet); `lastSeen` is the host's last bridge
 * activity, `agent` the session's name of the agent. */
export function machineView(m: Machine | null, now: number, lastSeen: string | null, agent: string): MachineView {
	const a = agentName(agent);
	if (!m) {
		return {
			headline: `${a} has no machine yet.`,
			detail: `Set it up and the board creates a machine for ${a}; it takes a few minutes.`,
			setup: `Set up ${a}`,
			signIn: [],
			claude: false
		};
	}
	const signIn = Object.entries(m.sign_in ?? {})
		.filter(([, v]) => v === 'expiring' || v === 'expired')
		.map(([k]) => FLOWS[k] ?? k);
	const signedIn = m.relay?.flow === 'claude' && m.relay.state === 'done';
	const claude = m.state === 'connected' && m.sign_in?.claude !== 'ok' && !signedIn;
	const view = (headline: string, detail: string, setup: string | null = null): MachineView => ({ headline, detail, setup, signIn, claude });
	switch (m.state) {
		case 'requested':
			if (!m.requested_at) return view(`${a}’s machine is being set up.`, 'The request has not reached Azure yet.', 'Try again');
			return view(
				`${a}’s machine is being set up.`,
				`Requested ${elapsed(m.requested_at, now)} ago${m.deploy_state ? ` · Azure: ${m.deploy_state.toLowerCase()}` : ''}.`
			);
		case 'failed':
			return view(`Setting up ${a}’s machine failed.`, m.deploy_error ?? 'Azure gave no reason.', 'Try again');
		case 'not_connected': {
			const since = m.requested_at ? ` It was requested ${elapsed(m.requested_at, now)} ago.` : '';
			const who = m.admins.length > 0 ? `Ask ${names(m.admins)} to look at it.` : 'Ask your board admin to look at it.';
			return view(`${a}’s machine has not connected.`, `It did not join the board within 30 minutes.${since} ${who}`);
		}
		case 'up':
			return view(`${a}’s machine is up.`, `It has not connected ${a} to the board yet.`);
		case 'connected':
			return view(`${a}’s machine is connected.`, `${a} is not up yet.`);
		case 'ready':
			return view(`${a} is online.`, m.ready_at ? `It answered the board ${elapsed(m.ready_at, now)} ago.` : 'It answered the board.');
		case 'disconnected':
			return view(`${a}’s machine is offline.`, lastSeen ? `Last heard from ${elapsed(lastSeen, now)} ago.` : 'Its bridge stopped answering.');
		case 'deleting':
			return view(`${a}’s machine is being removed.`, 'Set it up again once it is gone.');
	}
}

/** What the sign-in part of the page shows. */
export interface SignInView {
	/** The flows the person can start now, each with what the machine last reported of it: none
	 * while one runs or the machine cannot run one. A signed-in flow's button is the quiet
	 * `again`; `status` is null while the flow's state is not known. */
	start: { flow: Flow; name: string; label: string; status: string | null; warn: boolean; again: boolean }[];
	/** The running sign-in: its line, its link, the device code to type there, and whether the
	 * page takes the code Claude shows. */
	running: null | { relay_id: string; flow: Flow; text: string; url: string | null; link: string; userCode: string | null; paste: boolean };
	/** How the last sign-in ended; null when it was cancelled or none ran. */
	outcome: string | null;
}

/** The machine's bridge is there to run a sign-in. */
const RELAYS: MachineState[] = ['up', 'connected', 'ready'];
const ORDER: Flow[] = ['claude', 'github', 'codex'];

function failure(flow: string, reason: string | null): string {
	const f = FLOWS[flow] ?? flow;
	switch (reason) {
		case 'expired':
			return `The ${f} sign-in expired. Start it again.`;
		case 'denied':
			return `${SITES[flow] ?? f} refused the sign-in.`;
		case 'bad_code':
			return 'Claude did not take that code. Start again for a new link.';
		case 'timeout':
			return `The ${f} sign-in timed out. Start it again.`;
		case 'busy':
			return `Another ${f} sign-in is running on your machine. Try again in a minute.`;
		default:
			return `The ${f} sign-in stopped on your machine. Start it again.`;
	}
}

/** What the relay part says for `m` at `now`. */
export function signInView(m: Machine | null, now: number): SignInView {
	const none: SignInView = { start: [], running: null, outcome: null };
	if (!m || !RELAYS.includes(m.state)) return none;
	const r = m.relay;
	const f = r ? (FLOWS[r.flow] ?? r.flow) : '';
	if (r && (r.state === 'starting' || r.state === 'url' || r.state === 'waiting')) {
		const site = SITES[r.flow] ?? f;
		// a device flow waits for the person to type the code: the code and its page stay
		const device = r.flow !== 'claude' && r.state !== 'starting';
		let text: string;
		if (r.state === 'starting') text = `Starting the ${f} sign-in on your machine…`;
		else if (device) {
			const left = r.expires_at ? Math.round((Date.parse(r.expires_at) - now) / 60_000) : null;
			const until = left !== null && left > 0 ? ` The code is good for ${left} min.` : '';
			const waiting = r.state === 'waiting' ? ` Waiting for your approval on ${site}…` : '';
			text = `Open ${site}’s page and type this code there.${until}${waiting}`;
		} else if (r.state === 'waiting') text = 'Checking the code with Claude…';
		else text = 'Open the Claude sign-in, sign in as yourself, then paste the code Claude shows you here.';
		const url = r.state === 'starting' ? null : r.url;
		const link = r.flow === 'claude' ? 'Open the Claude sign-in' : (url ?? '').replace(/^https:\/\//, '');
		return {
			start: [],
			running: { relay_id: r.relay_id, flow: r.flow, text, url, link, userCode: device ? r.user_code : null, paste: r.flow === 'claude' && r.state === 'url' },
			outcome: null
		};
	}
	const outcome = !r ? null : r.state === 'done' ? `Signed in to ${f}.` : r.state === 'failed' ? failure(r.flow, r.reason) : null;
	return { start: ORDER.map((flow) => row(flow, reported(m, flow))), running: null, outcome };
}

/** `flow`'s state: the last report's, except that a relay of it that ended `done` reads ok unless
 * a later report says the person is needed (the server sets ok on `done`, so such a report is newer). */
function reported(m: Machine, flow: Flow): string | undefined {
	const said = m.sign_in?.[flow];
	if (said === 'expiring' || said === 'expired') return said;
	return m.relay?.flow === flow && m.relay.state === 'done' ? 'ok' : said;
}

function row(flow: Flow, state: string | undefined): SignInView['start'][number] {
	const name = FLOWS[flow] ?? flow;
	const start: SignInView['start'][number] = { flow, name, label: `Sign in to ${name}`, status: null, warn: false, again: false };
	switch (state) {
		case 'ok':
			return { ...start, label: 'Sign in again', status: 'Signed in', again: true };
		case 'expiring':
			// the board keeps the state word, not the check's expiry; expiring is under 3 days left (agentvm-signin-check)
			return { ...start, status: 'Sign-in expires within 3 days', warn: true };
		case 'expired':
			return { ...start, status: 'Sign-in expired', warn: true };
		default:
			return start;
	}
}

export type StepId = 'request' | 'accept' | 'build' | 'join' | 'claude' | 'online' | 'github';
/** `turn`: the person acts (a sign-in); `skipped`: an adopted machine had no deployment. */
export type StepState = 'done' | 'active' | 'failed' | 'turn' | 'pending' | 'skipped';
export interface Step {
	id: StepId;
	label: string;
	state: StepState;
	note: string | null;
}

/** The machine reached the board and its bridge can run a sign-in. */
const JOINED: MachineState[] = ['up', 'connected', 'ready'];
const RUNNING: Relay['state'][] = ['starting', 'url', 'waiting'];

/** The setup as steps for `m` at `now`; `sending` while the page's Set up has no answer yet.
 * Empty when nothing is to show: no record and nothing sent, offline, being removed, or all done. */
export function setupSteps(m: Machine | null, now: number, agent: string, sending: boolean): Step[] {
	const a = agentName(agent);
	const step = (id: StepId, label: string, state: StepState = 'pending', note: string | null = null): Step => ({ id, label, state, note });
	const labels: [StepId, string][] = [
		['request', 'Request sent'],
		['accept', 'Azure accepts the request'],
		['build', 'Azure builds the machine'],
		['join', 'The machine joins the board'],
		['claude', 'Claude sign-in'],
		['online', `${a} is online`],
		['github', 'GitHub sign-in']
	];
	if (sending) {
		return labels.map(([id, label]) => (id === 'request' ? step(id, 'Sending the request', 'active') : step(id, label)));
	}
	if (!m || m.state === 'disconnected' || m.state === 'deleting') return [];
	const since = m.requested_at ? `${elapsed(m.requested_at, now)} since requested` : null;
	const joined = JOINED.includes(m.state);
	const adopted = joined && !m.requested_at;
	const deployed = m.deploy_state === 'Succeeded' || joined;
	const failed = m.state === 'failed';
	const why = m.deploy_error ?? 'Azure gave no reason.';
	const running = m.relay && RUNNING.includes(m.relay.state) ? m.relay.flow : null;
	const signIn = (flow: Flow, name: string): [StepState, string | null] => {
		if (!joined) return ['pending', null];
		const said = reported(m, flow);
		if (said === 'ok' || said === 'expiring' || (flow === 'claude' && m.state === 'ready')) return ['done', null];
		if (running === flow) return ['active', null];
		if (m.relay?.flow === flow && m.relay.state === 'failed') return ['failed', failure(flow, m.relay.reason)];
		return ['turn', running ? 'Your turn, once the running sign-in ends.' : `Your turn: use “Sign in to ${name}” below.`];
	};
	const claude = signIn('claude', 'Claude');
	const steps: Record<StepId, [StepState, string | null]> = {
		request: adopted ? ['skipped', 'Not needed'] : ['done', null],
		accept: adopted
			? ['skipped', 'Not needed']
			: m.deploy_state || joined
				? ['done', null]
				: failed
					? ['failed', why]
					: ['active', since],
		build: adopted
			? ['skipped', 'Not needed']
			: deployed
				? ['done', null]
				: m.deploy_state
					? failed
						? ['failed', why]
						: ['active', since]
					: ['pending', null],
		join: joined
			? ['done', null]
			: m.state === 'not_connected'
				? ['failed', 'It did not join the board within 30 minutes.']
				: deployed && m.state === 'requested'
					? ['active', since]
					: ['pending', null],
		claude,
		online: m.state === 'ready' ? ['done', null] : joined && claude[0] === 'done' ? ['active', since] : ['pending', null],
		github: signIn('github', 'GitHub')
	};
	const list = labels.map(([id, label]) => {
		const [state, note] = steps[id];
		return step(id, id === 'accept' && state === 'active' ? 'Waiting for Azure to accept the request' : label, state, note);
	});
	return list.every((s) => s.state === 'done' || s.state === 'skipped') ? [] : list;
}
