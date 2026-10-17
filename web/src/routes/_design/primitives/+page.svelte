<script lang="ts">
	// Design-only harness: the U8 primitives and state components on fixture data, for
	// scripts/design/interact.mjs and the renders. Its skin control sets data-skin directly.
	import FilterMenu from '#lib/ui/FilterMenu.svelte';
	import Sheet from '#lib/ui/Sheet.svelte';
	import Disclosure from '#lib/ui/Disclosure.svelte';
	import StateWord from '#lib/ui/StateWord.svelte';
	import EmptyState from '#lib/ui/EmptyState.svelte';
	import ErrorLine from '#lib/ui/ErrorLine.svelte';
	import SkeletonRows from '#lib/ui/SkeletonRows.svelte';
	import Announcer from '#lib/ui/Announcer.svelte';
	import { feed, thread, ids, fleet, session, short, clock, firstLine, NOW } from '#lib/design/model.ts';
	import type { Message } from '#lib/types.ts';
	import { tick } from 'svelte';
	import { page } from '$app/state';

	let skin = $state<'a' | 'b'>('a');
	$effect(() => {
		document.documentElement.dataset.skin = skin;
	});

	const agents = [{ value: '', label: 'all' }, ...[...new Set(feed.flatMap((e) => e.via))].sort().map((a) => ({ value: a, label: short(a) }))];
	const directions = [
		{ value: '', label: 'all' },
		{ value: 'in', label: 'received' },
		{ value: 'out', label: 'sent' },
		{ value: 'own', label: 'between yours' }
	];
	let agent = $state('');
	let dir = $state('');

	const teammates = [...new Set(fleet.hosts.filter((h) => h.owner !== session.user).map((h) => h.owner))];
	let sheetOpen = $state(false);
	let mountOpen = $state(false);
	let mountedOpen = $state(true);

	// closes that race the Sheet's history entry (interact.mjs)
	async function openThenParentCloses() {
		sheetOpen = true;
		await tick();
		sheetOpen = false;
	}
	async function openThenDismiss() {
		sheetOpen = true;
		await tick();
		document.querySelector<HTMLButtonElement>('[data-test=main-sheet] dialog header button')?.click();
	}
	function openThenParentClosesLater() {
		sheetOpen = true;
		setTimeout(() => (sheetOpen = false), 300);
	}
	const chain = thread(ids.feedThread);
	let behind = $state(0);
	let retries = $state(0);
	let announced = $state('');

	// one row per delivery word, on fixture-shaped messages
	const sample = (place: string, states: string[], read_by: { read: number; total: number } | null = null) =>
		({ place, delivery: states.length || read_by ? { recipients: states.map((state, i) => ({ recipient: `r${i}`, state })), read_by } : undefined }) as Pick<Message, 'place' | 'delivery'>;
	const samples = [
		['read', sample('dm', ['read'])],
		['delivered', sample('dm', ['delivered'])],
		['queued', sample('dm', ['queued'])],
		['retrying', sample('dm', ['retrying'])],
		['held', sample('dm', ['held'])],
		['refused', sample('dm', ['refused'])],
		['gone', sample('dm', ['gone'])],
		['channel, 3 of 4 read', sample('#infra', [], { read: 3, total: 4 })],
		['channel, held beats read', sample('#infra', ['held'], { read: 3, total: 4 })],
		['channel post', sample('#infra', [])]
	] as const;
</script>

<main class="harness">
	<header>
		<h1>Primitives</h1>
		<div class="skins" role="group" aria-label="Skin">
			<button type="button" class="quiet" data-skin-pick="a" aria-pressed={skin === 'a'} onclick={() => (skin = 'a')}>Console</button>
			<button type="button" class="quiet" data-skin-pick="b" aria-pressed={skin === 'b'} onclick={() => (skin = 'b')}>Editorial</button>
		</div>
	</header>

	<section aria-labelledby="h-menu">
		<h2 id="h-menu">Filter menu</h2>
		<div class="bar">
			<span data-test="menu-agent"><FilterMenu label="agent" options={agents} bind:value={agent} /></span>
			<span data-test="menu-dir"><FilterMenu label="direction" options={directions} bind:value={dir} /></span>
		</div>
		<p class="muted">filters: <output data-test="filters">agent={agent || 'all'};dir={dir || 'all'}</output></p>
	</section>

	<section aria-labelledby="h-sheet">
		<h2 id="h-sheet">Sheet</h2>
		<div class="bar">
			<button type="button" data-test="open-sheet" onclick={() => (sheetOpen = true)}>Open the thread</button>
			<button type="button" class="quiet" data-test="behind" onclick={() => behind++}>Behind the sheet ({behind})</button>
		</div>
		<div class="bar">
			<button type="button" class="quiet" data-test="race-parent" onclick={openThenParentCloses}>Open, parent closes at once</button>
			<button type="button" class="quiet" data-test="race-dismiss" onclick={openThenDismiss}>Open, Close at once</button>
			<button type="button" class="quiet" data-test="parent-later" onclick={openThenParentClosesLater}>Open, parent closes in 300 ms</button>
			<button type="button" class="quiet" data-test="mount-open" onclick={() => ((mountedOpen = true), (mountOpen = true))}>Mount a sheet open</button>
		</div>
		<p class="muted">history state: <output data-test="page-state">{JSON.stringify(page.state)}</output></p>
		{#if mountOpen}
			<div data-test="mounted-sheet">
				<Sheet bind:open={mountedOpen} label="Mounted open"><p>Mounted with open set.</p></Sheet>
			</div>
		{/if}
		<div data-test="main-sheet" class="contents">
		<Sheet bind:open={sheetOpen} label="Thread · {chain.length} messages">
			<ol class="thread">
				{#each chain as { m, depth } (m.message_id)}
					<li style:margin-left="{depth * 16}px">
						<div class="meta"><span>{short(m.from)} → {m.place === 'dm' ? short(m.to) : m.place}</span><span class="muted">{clock(m.date)}</span><StateWord message={m} /></div>
						<p>{m.text}</p>
					</li>
				{/each}
			</ol>
			<p><a href="#/_design/primitives" data-test="sheet-link">A link inside</a></p>
		</Sheet>
		</div>
	</section>

	<section aria-labelledby="h-disc">
		<h2 id="h-disc">Disclosure</h2>
		{#each teammates as owner (owner)}
			<div data-test="disc-{owner}">
				<Disclosure label="{owner} · {fleet.hosts.filter((h) => h.owner === owner).length} host(s)">
					<ul class="plain">
						{#each fleet.agents.filter((a) => a.owner === owner) as a (a.name)}
							<li>{short(a.name)} <span class="muted">{a.kind} on {a.host}</span></li>
						{/each}
					</ul>
				</Disclosure>
			</div>
		{/each}
	</section>

	<section aria-labelledby="h-state">
		<h2 id="h-state">State words</h2>
		<table>
			<tbody>
				{#each samples as [what, m] (what)}
					<tr><td class="muted">{what}</td><td><StateWord message={m} /></td></tr>
				{/each}
				{#each feed.slice(0, 6) as e (e.m.message_id)}
					<tr><td class="muted">{firstLine(e.m.text, 48)}</td><td><StateWord message={e.m} /></td></tr>
				{/each}
			</tbody>
		</table>
	</section>

	<section aria-labelledby="h-states">
		<h2 id="h-states">Empty, error, loading, announcer</h2>
		<EmptyState text="Nothing yet. Messages your agents send or receive on the shared board appear here as they happen." />
		<EmptyState text="No delivery problems." />
		<ErrorLine error="store unavailable" asOf={NOW} retry={() => retries++} />
		<p class="muted">retries: <output data-test="retries">{retries}</output></p>
		<SkeletonRows count={4} />
		<div class="bar">
			<button type="button" class="quiet" data-test="arrive" onclick={() => (announced = '2 new messages')}>Two arrive</button>
		</div>
		<Announcer message={announced} />
	</section>
</main>

<style>
	.harness {
		max-width: 920px;
		margin: 0 auto;
		padding: 16px;
	}
	header {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
	}
	h1 {
		margin: 0;
		font-size: 1.3em;
	}
	h2 {
		margin: 24px 0 8px;
		font-size: 1em;
		color: var(--muted);
	}
	.skins {
		display: flex;
		gap: 4px;
	}
	.skins [aria-pressed='true'] {
		border-color: var(--accent);
		color: var(--accent);
	}
	.contents {
		display: contents;
	}
	.bar {
		margin-top: 8px;
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
	}
	.thread {
		margin: 0;
		padding: 0;
		list-style: none;
	}
	.thread li {
		padding: 8px 0;
		border-bottom: 1px solid var(--line);
	}
	.thread p {
		margin: 4px 0 0;
		font-family: var(--font-text);
		white-space: pre-wrap;
	}
	.meta {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
	}
	.plain {
		margin: 0;
		padding: 0;
		list-style: none;
	}
	table {
		border-collapse: collapse;
	}
	td {
		padding: 3px 16px 3px 0;
	}
</style>
