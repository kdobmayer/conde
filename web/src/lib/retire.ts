/**
 * Retiring an own name nothing holds any more (`agent.retire`): it leaves the owner's lists and
 * the directory, and its messages stay readable. The board refuses a name that is still bound.
 */

import type { OwnerAgent, Rail } from './types.ts';

/** Whether the agent page offers Retire: one of the owner's names that nothing holds. */
export function retirable(a: OwnerAgent): boolean {
	return a.address === null;
}

/** The rail without the retired name (the answer of `agent.retire`). */
export function withoutAgent(rail: Rail, name: string): Rail {
	return { ...rail, agents: rail.agents.filter((a) => a.name !== name) };
}
