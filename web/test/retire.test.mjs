// Retiring an own name: offered only for a name nothing holds, and the board's answer takes that
// one name off the rail.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { retirable, withoutAgent } from '../src/lib/retire.ts';

const agent = (name, address) => ({ name, kind: 'claude', address, dms: [], channels: [] });
const rail = () => ({
	owner: 'k',
	agents: [agent('k/desk', 'mailbox:mbx-desk@homepc'), agent('k/old', null), agent('k/astra', 'codex:t-1@astra-box')]
});

test('only a name nothing holds is offered', () => {
	const [desk, old, astra] = rail().agents;
	assert.equal(retirable(old), true);
	assert.equal(retirable(desk), false, 'a published name is held');
	assert.equal(retirable(astra), false, 'a bound session holds it');
});

test('the answer takes that one name off the rail, and leaves the rail it came from alone', () => {
	const before = rail();
	const after = withoutAgent(before, 'k/old');
	assert.deepEqual(after.agents.map((a) => a.name), ['k/desk', 'k/astra']);
	assert.equal(before.agents.length, 3, 'the old rail is not mutated');
	assert.equal(after.agents[0], before.agents[0], 'the other agents are kept as they were');
});
