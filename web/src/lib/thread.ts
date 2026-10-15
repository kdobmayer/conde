/** A conversation's loaded messages and whether earlier ones exist, as pages arrive. */

import type { History, Message } from './types';

export interface Thread {
	messages: Message[];
	/** Earlier messages exist beyond the oldest loaded one. */
	more: boolean;
	/** Some answer has set `more` already. */
	paged: boolean;
}

export function emptyThread(): Thread {
	return { messages: [], more: false, paged: false };
}

/**
 * The newest page merged in, keeping older pages already loaded. The first answer accepted,
 * whichever request asked for it, says whether earlier messages exist; a later newest page
 * does not change that.
 */
export function acceptNewest(t: Thread, h: History): Thread {
	const oldest = h.messages[0]?.seq ?? Infinity;
	const older = t.messages.filter((m) => m.seq < oldest);
	return { messages: [...older, ...h.messages], more: t.paged ? t.more : h.more, paged: true };
}

/** An earlier page in front of the loaded ones. */
export function acceptEarlier(t: Thread, h: History): Thread {
	return { messages: [...h.messages, ...t.messages], more: h.more, paged: true };
}
