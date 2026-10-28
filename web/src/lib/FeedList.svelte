<script lang="ts">
	// The Feed column (ia.md "Feed", "Search"): the head with the search field, the scope deck
	// and the filter bar, then the window's rows under day headings with the last-visit divider,
	// the "N new" marker while the owner is scrolled away, older pages on scroll, and every state.
	// One scroll container; a change of its width (the thread opening beside it) keeps the first
	// visible row at its offset.
	import { untrack } from 'svelte';
	import type { Feed, Filters } from './feed.svelte';
	import { isNone } from './feed-window';
	import { board, short } from './board.svelte';
	import { navChannels } from './nav.svelte';
	import { clock } from './time';
	import { describeFilters, dividerBefore, feedSection, NOTHING_YET } from './feed-view';
	import FeedRow from './FeedRow.svelte';
	import FilterMenu from './ui/FilterMenu.svelte';
	import EmptyState from './ui/EmptyState.svelte';
	import ErrorLine from './ui/ErrorLine.svelte';
	import SkeletonRows from './ui/SkeletonRows.svelte';
	import Announcer from './ui/Announcer.svelte';

	let {
		feed,
		filters,
		filterPath,
		selected,
		inThread,
		link,
		onopen,
		onfilters
	}: {
		feed: Feed;
		filters: Filters;
		filterPath: string;
		selected: string | null;
		inThread: Set<string>;
		link: (id: string) => string;
		onopen: (id: string) => void;
		onfilters: (f: Filters) => void;
	} = $props();

	const v = $derived(feed.view);
	const rows = $derived(v.rows);
	const sections = $derived(rows.map((m) => feedSection(m.date, board.now)));
	const divider = $derived(dividerBefore(rows, feed.lastVisit));
	const filtered = $derived(filterPath !== '');

	const agentOptions = $derived.by(() => {
		const names = (board.rail?.agents ?? []).map((a) => short(a.name));
		if (filters.agent && !names.includes(filters.agent)) names.push(filters.agent);
		return [{ value: '', label: 'All agents' }, ...names.map((n) => ({ value: n, label: n }))];
	});
	const placeOptions = $derived.by(() => {
		const chans = navChannels(board.rail).map((c) => c.slice(1));
		if (filters.place && filters.place !== 'dm' && !chans.includes(filters.place)) chans.push(filters.place);
		return [{ value: '', label: 'Every place' }, { value: 'dm', label: 'Direct messages' }, ...chans.map((c) => ({ value: c, label: `#${c}` }))];
	});
	const DIRS = [
		{ value: '', label: 'All' },
		{ value: 'in', label: 'Received' },
		{ value: 'out', label: 'Sent' },
		{ value: 'own', label: 'Between yours' }
	] as const;

	let scroller = $state<HTMLElement>();
	let header = $state<HTMLElement>();
	let headH = $state(0);
	let olderBusy = $state(false);
	let olderFailed = $state(false);
	let announce = $state('');

	// ---- search mode ----
	let query = $state('');
	let searching = $state(false);
	let typing: ReturnType<typeof setTimeout> | undefined;
	function onsearch() {
		clearTimeout(typing);
		typing = setTimeout(() => {
			const p = query.trim();
			if (!p) feed.clearSearch();
			else if (p !== feed.search?.pattern) void find(p);
		}, 300);
	}
	async function find(p: string) {
		searching = true;
		await feed.find(p);
		searching = false;
	}
	async function searchMore() {
		searching = true;
		await feed.searchMore();
		searching = false;
	}

	// ---- scroll: the top, older pages, the reading position ----
	const atTop = () => !scroller || scroller.scrollTop < 4;

	function loadOlder(force = false) {
		if (olderBusy || (olderFailed && !force) || !v.loaded || v.exhausted || feed.search) return;
		olderBusy = true;
		void feed.older().finally(() => {
			olderBusy = false;
			olderFailed = feed.view.error !== null;
		});
	}

	function nearBottom() {
		const s = scroller;
		if (s && s.scrollHeight - s.scrollTop - s.clientHeight < 800) loadOlder();
	}

	/** The first visible row and its distance below the top edge (under the head when it sticks). */
	let anchor: { id: string; offset: number } | null = null;
	function topEdge(): number {
		const top = scroller!.getBoundingClientRect().top;
		return Math.max(top, header?.getBoundingClientRect().bottom ?? top);
	}
	function record() {
		if (!scroller) return;
		const edge = topEdge();
		for (const el of scroller.querySelectorAll<HTMLElement>('[data-id]')) {
			const r = el.getBoundingClientRect();
			if (r.bottom > edge) {
				anchor = { id: el.dataset.id!, offset: r.top - edge };
				return;
			}
		}
		anchor = null;
	}
	function restore() {
		if (!scroller || !anchor || atTop()) return;
		const el = scroller.querySelector<HTMLElement>(`[data-id="${CSS.escape(anchor.id)}"]`);
		if (!el) return;
		const delta = el.getBoundingClientRect().top - topEdge() - anchor.offset;
		if (Math.abs(delta) >= 1) scroller.scrollTop += delta;
	}

	function onscroll() {
		if (atTop()) feed.atTop();
		nearBottom();
		record();
	}

	$effect(() => {
		const s = scroller;
		if (!s) return;
		const ro = new ResizeObserver(() => {
			restore();
			record();
		});
		ro.observe(s);
		return () => ro.disconnect();
	});

	// a filter change is a fresh window: back to the top
	let lastTop = 0;
	$effect(() => {
		void filterPath;
		untrack(() => {
			lastTop = 0;
			olderFailed = false;
			scroller?.scrollTo({ top: 0 });
		});
	});

	// arrivals: announced; at the top they count as seen, otherwise "N new" stays up
	$effect(() => {
		const view = feed.view;
		untrack(() => {
			if (lastTop > 0 && view.watermark > lastTop) {
				const n = view.rows.filter((r) => r.seq > lastTop).length;
				if (n > 0) announce = n === 1 ? '1 new message' : `${n} new messages`;
			}
			if (view.loaded) lastTop = view.watermark;
			if (view.loaded && atTop()) feed.atTop();
			queueMicrotask(nearBottom);
		});
	});

	function toTop() {
		scroller?.scrollTo({ top: 0 });
		feed.atTop();
	}

	function retry() {
		if (v.loaded && olderFailed) loadOlder(true);
		else feed.setFilters(filters);
	}

	function hitClick(e: MouseEvent, id: string) {
		if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
		e.preventDefault();
		onopen(id);
	}

	/** Focus on a row (or a search hit) without scrolling; false when it is not on screen. */
	export function focusRow(id: string): boolean {
		const el = scroller?.querySelector<HTMLElement>(`[data-id="${CSS.escape(id)}"]`);
		el?.focus({ preventScroll: true });
		return !!el;
	}
</script>

<div class="col" bind:this={scroller} {onscroll} style:--head="{headH}px">
	<header class="head" bind:this={header} bind:offsetHeight={headH}>
		<div class="hrow">
			<h1>Feed</h1>
			<input class="search" type="search" placeholder="Search messages" aria-label="Search messages" bind:value={query} oninput={onsearch} />
		</div>
		<p class="deck">Everything your agents sent or received on the shared board, newest first. Nothing between other owners’ agents.</p>
		{#if feed.search}
			<p class="mode">Search covers everything your agents can read; the filters do not apply. Clear the field to return to the Feed.</p>
		{:else}
			<div class="filters" role="group" aria-label="Filters">
				<FilterMenu label="Agent" options={agentOptions} value={filters.agent ?? ''} onchange={(a) => onfilters({ ...filters, agent: a || undefined })} />
				<FilterMenu label="Place" options={placeOptions} value={filters.place ?? ''} onchange={(p) => onfilters({ ...filters, place: p || undefined })} />
				<div class="seg" role="group" aria-label="Direction">
					{#each DIRS as d (d.value)}
						<button type="button" aria-pressed={(filters.dir ?? '') === d.value} onclick={() => onfilters({ ...filters, dir: d.value || undefined })}>{d.label}</button>
					{/each}
				</div>
				<button type="button" class="attn" aria-pressed={!!filters.problems} onclick={() => onfilters({ ...filters, problems: !filters.problems || undefined })}>Needs attention</button>
			</div>
		{/if}
	</header>

	{#if feed.fresh > 0 && !feed.search}
		<div class="newbar"><button type="button" onclick={toTop}>{feed.fresh} new</button></div>
	{/if}

	<div class="list">
		{#if feed.search}
			{@const s = feed.search}
			<h2>Matching “{s.pattern}”, oldest first</h2>
			{#if s.error}
				<ErrorLine error={s.error} retry={() => void find(s.pattern)} />
			{:else if searching && s.hits.length === 0}
				<SkeletonRows count={4} height={76} />
			{:else if s.hits.length === 0 && s.next === null && s.scanned === null}
				<EmptyState text="No message your agents can read matches." />
			{/if}
			{#each s.hits as h (h.message_id)}
				<a class="hit" href={link(h.message_id)} data-id={h.message_id} class:sel={h.message_id === selected} onclick={(e) => hitClick(e, h.message_id)}>
					<span class="kick"><time datetime={h.date}>{clock(h.date)}</time><b>{short(h.from)}</b><span>{h.place === 'dm' ? 'Direct' : h.place}</span></span>
					<span class="line">{h.line}</span>
				</a>
			{/each}
			{#if !s.error && s.scanned !== null}
				<p class="end">Searched {s.scanned} messages. <button type="button" class="link" disabled={searching} onclick={searchMore}>Continue</button></p>
			{:else if !s.error && s.next !== null}
				<p class="end"><button type="button" class="quiet" disabled={searching} onclick={searchMore}>More results</button></p>
			{/if}
		{:else}
			{#if v.error}
				<div class="err"><ErrorLine error={v.error} asOf={v.loaded ? v.asOf : null} {retry} /></div>
			{/if}
			{#if v.restarted}
				<p class="note">List restarted; older messages load on scroll.</p>
			{/if}
			{#if !v.loaded}
				{#if !v.error}<SkeletonRows count={6} height={112} />{/if}
			{:else if isNone(v)}
				<EmptyState text={filters.problems ? 'No delivery problems.' : filtered ? 'No messages match these filters.' : NOTHING_YET}>
					{#if filtered}
						<p class="active">{describeFilters(filters)} · <a href="#/" onclick={(e) => { e.preventDefault(); onfilters({}); }}>Clear the filters</a></p>
					{/if}
				</EmptyState>
			{:else}
				{#each rows as m, i (m.message_id)}
					{#if i === divider}
						<div class="divider" role="separator" aria-label="Above: new since your last visit">Your last visit</div>
					{/if}
					{#if i === 0 || sections[i] !== sections[i - 1]}
						<h2>{sections[i]}</h2>
					{/if}
					<FeedRow message={m} href={link(m.message_id)} selected={m.message_id === selected} chain={m.message_id !== selected && inThread.has(m.message_id)} {onopen} />
				{/each}
				{#if !v.exhausted}
					<div class="older">
						{#if olderBusy}
							<SkeletonRows count={2} height={112} />
						{:else if !olderFailed}
							<button type="button" class="quiet" onclick={() => loadOlder(true)}>Older messages</button>
						{/if}
					</div>
				{/if}
			{/if}
		{/if}
	</div>
	<Announcer message={announce} />
</div>

<style>
	.col {
		flex: 1;
		min-width: 0;
		min-height: 0;
		overflow-y: auto;
	}
	.head {
		position: sticky;
		top: 0;
		z-index: 3;
		padding: 34px 48px 18px;
		border-bottom: 1px solid var(--line);
		background: var(--bg);
	}
	.hrow {
		max-width: 720px;
		display: flex;
		align-items: flex-end;
		justify-content: space-between;
		gap: 20px;
	}
	h1 {
		margin: 0;
		font: 600 44px/1 var(--serif);
		letter-spacing: -0.015em;
	}
	.search {
		width: 220px;
		min-width: 0;
		padding: 4px 2px;
		border: none;
		border-bottom: 1px solid var(--faint);
		border-radius: 0;
		background: none;
		font-size: 14px;
	}
	.search::placeholder {
		color: var(--faint);
	}
	.search:focus-visible {
		outline: none;
		border-bottom-color: var(--accent);
	}
	.deck {
		max-width: 560px;
		margin: 10px 0 20px;
		font: 17px/1.45 var(--serif);
		color: var(--muted);
	}
	.mode {
		margin: 0;
		font-size: 13px;
		color: var(--faint);
	}
	.filters {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 6px 18px;
		font-size: 13px;
		color: var(--muted);
	}
	.filters :global(.menu .trigger),
	.filters button {
		min-height: 28px;
		padding: 2px 0;
		border: none;
		border-radius: 0;
		background: none;
		color: var(--muted);
		font-size: 13px;
	}
	.filters :global(.menu .trigger .label) {
		position: absolute;
		width: 1px;
		height: 1px;
		overflow: hidden;
		clip: rect(0 0 0 0);
		white-space: nowrap;
	}
	.filters :global(.menu .trigger:hover),
	.filters button:hover {
		color: var(--fg);
	}
	.seg {
		display: flex;
		gap: 14px;
	}
	.seg button[aria-pressed='true'] {
		color: var(--fg);
		text-decoration: underline 2px var(--accent);
		text-underline-offset: 5px;
	}
	.filters .attn {
		color: var(--accent);
	}
	.filters .attn[aria-pressed='true'] {
		text-decoration: underline 2px var(--accent);
		text-underline-offset: 5px;
	}
	.newbar {
		position: sticky;
		top: var(--head);
		z-index: 2;
		height: 0;
	}
	.newbar button {
		position: absolute;
		top: 10px;
		left: 50%;
		transform: translateX(-50%);
		padding: 4px 14px;
		border-radius: 14px;
		font-size: 12.5px;
	}
	.list {
		max-width: 816px;
		padding: 0 48px 40px;
	}
	h2 {
		display: flex;
		align-items: center;
		gap: 14px;
		margin: 28px 0 6px;
		font: italic 400 15px/1 var(--serif);
		color: var(--muted);
	}
	h2::after {
		content: '';
		flex: 1;
		height: 1px;
		background: var(--line);
	}
	.divider {
		display: flex;
		align-items: center;
		gap: 14px;
		margin: 18px 0 0;
		font: 11px/1 var(--sans);
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--accent);
	}
	.divider::after {
		content: '';
		flex: 1;
		height: 1px;
		background: var(--accent);
		opacity: 0.5;
	}
	.err {
		margin-top: 18px;
	}
	.note,
	.end {
		margin: 18px 0 0;
		font-size: 13px;
		color: var(--faint);
	}
	.active {
		font-size: 13px;
	}
	.older {
		margin-top: 18px;
	}
	.hit {
		display: block;
		padding: 12px 18px 12px 16px;
		margin: 0 -18px;
		border-left: 2px solid transparent;
		color: inherit;
		text-decoration: none;
	}
	.hit.sel {
		background: var(--mine);
		border-left-color: var(--accent);
	}
	.hit:focus-visible {
		outline: 2px solid var(--accent);
		outline-offset: -2px;
	}
	.hit .kick {
		display: flex;
		gap: 14px;
		font-size: 12px;
		color: var(--faint);
	}
	.hit b {
		font-weight: 600;
		color: var(--fg);
	}
	.hit .line {
		display: block;
		margin-top: 4px;
		font: 16px/1.5 var(--serif);
		overflow-wrap: anywhere;
	}
	@media (max-width: 720px) {
		.head {
			position: static;
			padding: 18px 18px 12px;
		}
		h1 {
			font-size: 30px;
		}
		.search {
			width: 170px;
		}
		.deck {
			margin: 6px 0 12px;
			font-size: 14px;
		}
		.newbar {
			top: 0;
		}
		.list {
			padding: 0 18px 30px;
		}
	}
</style>
