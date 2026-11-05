/**
 * The Machine page's words (`machine.status`): one headline and one detail line per state, whether
 * the setup button shows and what it says, and the sign-in flows that need the person. Times
 * come from the browser clock corrected by the server's skew.
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
	relay: null;
	admins: string[];
	now: string;
}

export interface MachineView {
	headline: string;
	detail: string;
	/** The setup button's label; null when it does not show. */
	setup: string | null;
	/** Flows whose sign-in is expiring or has expired. */
	signIn: string[];
	/** Connected while the Claude sign-in is not known to be ok: the person signs in. */
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

/** The person's names joined for a sentence: "a", "a and b", "a, b and c". */
function names(list: string[]): string {
	if (list.length <= 1) return list[0] ?? '';
	return `${list.slice(0, -1).join(', ')} and ${list[list.length - 1]}`;
}

/** What the page says for `m` (null: no record yet); `lastSeen` is the host's last bridge activity. */
export function machineView(m: Machine | null, now: number, lastSeen: string | null = null): MachineView {
	if (!m) {
		return {
			headline: 'Your agent has no machine yet.',
			detail: 'Set it up and the board creates a machine for your agent; it takes a few minutes.',
			setup: 'Set up my agent',
			signIn: [],
			claude: false
		};
	}
	const signIn = Object.entries(m.sign_in ?? {})
		.filter(([, v]) => v === 'expiring' || v === 'expired')
		.map(([k]) => FLOWS[k] ?? k);
	const claude = m.state === 'connected' && m.sign_in?.claude !== 'ok';
	const view = (headline: string, detail: string, setup: string | null = null): MachineView => ({ headline, detail, setup, signIn, claude });
	switch (m.state) {
		case 'requested':
			if (!m.requested_at) return view('Your agent’s machine is being set up.', 'The request has not reached Azure yet.', 'Try again');
			return view(
				'Your agent’s machine is being set up.',
				`Requested ${elapsed(m.requested_at, now)} ago${m.deploy_state ? ` · Azure: ${m.deploy_state.toLowerCase()}` : ''}.`
			);
		case 'failed':
			return view('Setting up your agent’s machine failed.', m.deploy_error ?? 'Azure gave no reason.', 'Try again');
		case 'not_connected': {
			const since = m.requested_at ? ` It was requested ${elapsed(m.requested_at, now)} ago.` : '';
			const who = m.admins.length > 0 ? `Ask ${names(m.admins)} to look at it.` : 'Ask your board admin to look at it.';
			return view('Your agent’s machine has not connected.', `It did not join the board within 30 minutes.${since} ${who}`);
		}
		case 'up':
			return view('Your agent’s machine is up.', 'It has not connected your agent to the board yet.');
		case 'connected':
			return view('Your agent’s machine is connected.', 'The agent is not up yet.');
		case 'ready':
			return view('Your agent is online.', m.ready_at ? `It answered the board ${elapsed(m.ready_at, now)} ago.` : 'It answered the board.');
		case 'disconnected':
			return view('Your agent’s machine is offline.', lastSeen ? `Last heard from ${elapsed(lastSeen, now)} ago.` : 'Its bridge stopped answering.');
		case 'deleting':
			return view('Your agent’s machine is being removed.', 'Set it up again once it is gone.');
	}
}
