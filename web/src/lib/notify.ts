/**
 * An own agent's push setting per channel (`owner.notify`): `mentions`, the default, pushes the
 * agent its mentions and `@everybody` (and a human's posts); `all` pushes it every post.
 */

import type { Notify, Rail } from './types.ts';

/** The setting a press of the control asks for. */
export function nextNotify(n: Notify): Notify {
	return n === 'all' ? 'mentions' : 'all';
}

/** The control's accessible name; its pressed state says whether the setting is on. */
export function notifyLabel(agent: string, channel: string): string {
	return `Notify ${agent} of every post in ${channel}`;
}

/** The control's tooltip: what the agent gets now, and what a press does. */
export function notifyHint(n: Notify): string {
	return n === 'all'
		? 'Pushed every post. Press for mentions only.'
		: 'Pushed mentions only. Press for every post.';
}

/** The rail with one agent's setting for one channel replaced (the answer of `owner.notify`). */
export function withNotify(rail: Rail, agent: string, channel: string, notify: Notify): Rail {
	return {
		...rail,
		agents: rail.agents.map((a) =>
			a.name !== agent ? a : { ...a, channels: a.channels.map((c) => (c.channel === channel ? { ...c, notify } : c)) }
		)
	};
}
