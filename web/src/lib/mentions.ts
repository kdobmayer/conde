/** @mention completion over the fleet's names. */

import { board, short } from './board.svelte';

/** The `@token` ending at `caret`, if the caret is inside one. */
export function mentionAt(text: string, caret: number): { start: number; query: string } | null {
	const m = /(^|[\s(])@([A-Za-z0-9_./-]*)$/.exec(text.slice(0, caret));
	if (!m) return null;
	return { start: caret - m[2]!.length - 1, query: m[2]!.toLowerCase() };
}

/** Names that complete `query`: unqualified within the viewer's namespace, else owner/name. */
export function candidates(query: string, channel: boolean): string[] {
	const names = (board.fleet?.agents ?? []).map((a) => short(a.name));
	if (channel) names.push('everybody');
	const unique = [...new Set(names)];
	const starts = unique.filter((n) => n.toLowerCase().startsWith(query));
	const inside = unique.filter((n) => !n.toLowerCase().startsWith(query) && n.toLowerCase().includes(query));
	return [...starts.sort(), ...inside.sort()].slice(0, 8);
}
