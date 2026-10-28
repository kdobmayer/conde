<script lang="ts" module>
	import { goto } from '$app/navigation';
	import { formatFilters, parseFilters } from './feed.svelte';
	import { feedHash, threadHash, type FeedState } from './feed-view';

	/** A Feed URL loaded directly (`#/t/<id>`, `#/f/<filters>`, both): onto the route `/` with the
	 * filters, then the URL shown again as a shallow entry with the thread, replacing both times.
	 * The thread opens only in the second step, so the sheet's own entry comes after it. */
	export async function enterFeed(filterParam: string, thread: string | null): Promise<void> {
		const path = formatFilters(parseFilters(filterParam));
		const filters = path || undefined;
		await goto(new URL('#/', location.href), { replace: true, state: { filters } satisfies FeedState });
		if (path || thread) {
			const state: FeedState = { filters, ...(thread ? { thread } : {}) };
			await goto(new URL(thread ? threadHash(path, thread) : feedHash(path), location.href), { shallow: true, replace: true, state });
		}
	}
</script>

<script lang="ts">
	// The Feed page with its thread (ia.md "Feed", "Thread view"). The page stays mounted on the
	// route `/`; the filters and the open thread are history state behind shallow URLs
	// (`#/f/<filters>`, `#/t/<id>`, `#/f/<filters>/t/<id>`), so opening and closing a thread or
	// changing a filter never remounts the list and the reading position survives. Loading one of
	// those URLs directly goes through its route, which hands it here (enterFeed).
	// From 1280 px the thread sits beside the Feed; below, it is the Sheet (an overlay from 720 px,
	// a bottom sheet under it). Opening beside the Feed pushes a history entry, so back and Close
	// return to it; opening in the sheet replaces the entry and the sheet pushes its own.
	import { onDestroy, tick, untrack } from 'svelte';
	import { MediaQuery } from 'svelte/reactivity';
	import { page } from '$app/state';
	import { Feed, type Filters } from './feed.svelte';
	import { ThreadView } from './thread-view.svelte';
	import { onBoard } from './board.svelte';
	import FeedList from './FeedList.svelte';
	import Thread, { threadTitle } from './Thread.svelte';
	import Sheet from './ui/Sheet.svelte';

	const st = $derived(page.state as FeedState);
	const path = $derived(st.filters ?? '');
	const filters = $derived(parseFilters(path));
	const threadId = $derived(st.thread ?? null);
	const wide = new MediaQuery('min-width: 1280px');

	const feed = new Feed(untrack(() => filters));
	let applied = untrack(() => path);
	$effect(() => {
		if (path === applied) return;
		applied = path;
		feed.setFilters(untrack(() => filters));
	});
	onDestroy(() => feed.destroy());

	const view = $derived(threadId ? new ThreadView(threadId) : null);
	$effect(() => {
		const v = view;
		if (v) untrack(() => void v.load());
	});
	// a board change re-reads the open thread, at most once per 2 s
	let rereading: ReturnType<typeof setTimeout> | null = null;
	const off = onBoard((topics) => {
		if (rereading || !(topics.includes('messages') || topics.includes('delivery'))) return;
		rereading = setTimeout(() => {
			rereading = null;
			void view?.load();
		}, 2000);
	});
	onDestroy(() => {
		off();
		if (rereading) clearTimeout(rereading);
	});

	const inThread = $derived(new Set(view?.state.messages.map((m) => m.message_id) ?? []));
	const title = $derived(threadTitle(view?.state.messages ?? [], threadId));

	let list = $state<FeedList>();
	let aside = $state<Thread>();

	const at = (hash: string) => new URL(hash, location.href);
	const link = (id: string) => threadHash(path, id);

	function open(id: string) {
		if (wide.current) {
			// a second thread replaces the first: back still returns to the Feed
			const pushed = threadId ? st.pushed : true;
			void goto(at(threadHash(path, id)), { shallow: true, replace: threadId !== null, state: { ...st, thread: id, pushed } });
		} else {
			void goto(at(threadHash(path, id)), { shallow: true, replace: true, state: { filters: st.filters, thread: id } });
		}
	}

	let refocus: string | null = null;
	function close() {
		if (!threadId) return;
		refocus = threadId;
		if (st.pushed) history.back();
		else void goto(at(feedHash(path)), { shallow: true, replace: true, state: { filters: st.filters } });
	}

	/** A filter change is a new entry: back returns to the previous filters. An open thread stays
	 * open over the new list; its entry no longer sits on the same list, so closing replaces it. */
	function setFilters(f: Filters) {
		const next = formatFilters(f);
		if (next === path) return;
		const state: FeedState = { filters: next || undefined, ...(threadId ? { thread: threadId } : {}) };
		void goto(at(threadId ? threadHash(next, threadId) : feedHash(next)), { shallow: true, state });
	}

	// closed: focus goes back to the row it was opened from
	$effect(() => {
		if (threadId !== null || !refocus) return;
		const id = refocus;
		refocus = null;
		void tick().then(() => list?.focusRow(id));
	});
	// opened beside the Feed: focus moves to the thread's heading
	$effect(() => {
		if (threadId && wide.current) untrack(() => void tick().then(() => aside?.focusHeading()));
	});

	function onkeydown(e: KeyboardEvent) {
		if (e.key !== 'Escape' || !threadId || !wide.current || e.defaultPrevented) return;
		const t = e.target as HTMLElement | null;
		if (t instanceof HTMLInputElement && t.value) return;
		e.preventDefault();
		close();
	}
</script>

<svelte:window {onkeydown} />

<div class="feedpage">
	<FeedList bind:this={list} {feed} {filters} filterPath={path} selected={threadId} {inThread} {link} onopen={open} onfilters={setFilters} />
	{#if wide.current && view && threadId}
		<aside class="side" aria-label="Thread">
			<Thread bind:this={aside} {view} selected={threadId} {link} onopen={open} onclose={close} />
		</aside>
	{/if}
	<!-- mounted at every width: widening closes it through `open`, so it leaves its history entry -->
	<div class="sheet">
		<Sheet
			label={title}
			bind:open={
				() => !wide.current && threadId !== null,
				(v) => {
					if (!v && !wide.current) close();
				}
			}
		>
			{#if view && threadId}
				<Thread {view} selected={threadId} inSheet {link} onopen={open} />
			{/if}
		</Sheet>
	</div>
</div>

<style>
	.feedpage {
		flex: 1;
		min-height: 0;
		display: flex;
	}
	.side {
		width: 500px;
		flex: none;
		min-height: 0;
		border-left: 1px solid var(--line);
		background: var(--panel);
	}
	.sheet :global(dialog header h2) {
		font: 400 10.5px/1.4 var(--sans);
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--accent);
	}
	.sheet :global(dialog header button) {
		padding: 0;
		border: none;
		background: none;
		color: var(--faint);
		font-size: 13px;
	}
	.sheet :global(dialog header) {
		padding-left: 22px;
		padding-right: 22px;
	}
	.sheet :global(dialog .body) {
		padding: 10px 22px 0;
	}
</style>
