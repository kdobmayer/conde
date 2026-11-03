<script lang="ts">
	// An own agent (ia.md "Agents"): its status line and its conversations, DMs then channels,
	// newest first, beside the open conversation; on the phone the list, then the conversation.
	import type { Snippet } from 'svelte';
	import { page } from '$app/state';
	import { mutate } from '#lib/api.ts';
	import { board, changeRail, refreshRail, short } from '#lib/board.svelte.ts';
	import { KIND, agentFacts, conversationsOf, latestOf } from '#lib/conversation.ts';
	import { ago, stamp } from '#lib/time.ts';
	import { nextNotify, notifyHint, notifyLabel, withNotify } from '#lib/notify.ts';
	import type { Notify } from '#lib/types.ts';
	import EmptyState from '#lib/ui/EmptyState.svelte';
	import ErrorLine from '#lib/ui/ErrorLine.svelte';
	import SkeletonRows from '#lib/ui/SkeletonRows.svelte';
	import { MAX_WIDTH, MIN_WIDTH, clampWidth, readWidth, writeWidth } from '#lib/list-width.ts';

	let { children }: { children: Snippet } = $props();

	const name = $derived(`${board.session?.user}/${page.params.agent}`);
	const me = $derived(board.rail?.agents.find((a) => a.name === name) ?? null);
	const facts = $derived(agentFacts(board.fleet, name));
	const lists = $derived(me ? conversationsOf(me) : { dms: [], channels: [] });
	/** The conversation on screen: the DM route's counterpart, else the most recent one. */
	const open = $derived(
		page.route.id?.startsWith('/a/[agent]/dm/') ? `${page.params.owner}/${page.params.name}` : me ? latestOf(me) : null
	);
	const atList = $derived(page.route.id === '/a/[agent]');

	function dmHref(target: string): string {
		const [owner, n] = target.split('/');
		return `#/a/${encodeURIComponent(page.params.agent ?? '')}/dm/${encodeURIComponent(owner ?? '')}/${encodeURIComponent(n ?? '')}`;
	}
	/** The list's width chosen with the handle, null for the layout's default; per browser. */
	let width = $state<number | null>(readWidth());
	/** The width on screen, for the handle's value and as the start of a drag or a key press. */
	let shown = $state(0);
	let drag: { x: number; w: number } | null = null;

	function set(w: number) {
		width = clampWidth(w);
		writeWidth(width);
	}
	function down(e: PointerEvent) {
		if (e.button !== 0) return;
		e.preventDefault();
		(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
		drag = { x: e.clientX, w: shown };
	}
	function move(e: PointerEvent) {
		if (drag) width = clampWidth(drag.w + e.clientX - drag.x);
	}
	function up() {
		if (!drag) return;
		drag = null;
		if (width !== null) writeWidth(width);
	}
	function key(e: KeyboardEvent) {
		const step = e.shiftKey ? 48 : 16;
		const now = width ?? shown;
		const to = { ArrowLeft: now - step, ArrowRight: now + step, Home: MIN_WIDTH, End: MAX_WIDTH }[e.key];
		if (to === undefined) return;
		e.preventDefault();
		set(to);
	}
	function reset() {
		width = null;
		writeWidth(null);
	}

	/** The change in flight and the last change that failed, each with its agent: this layout
	 * stays mounted from one agent's page to the next. */
	let changing = $state<{ agent: string; channel: string } | null>(null);
	let failed = $state<{ agent: string; channel: string; to: Notify; error: string } | null>(null);

	/** Sets `agent`'s push setting for `channel`; the row shows the board's answer, never a guess. */
	async function setNotify(agent: string, channel: string, to: Notify) {
		if (changing) return;
		changing = { agent, channel };
		failed = null;
		try {
			await mutate('owner.notify', { agent, channel, notify: to });
			changeRail((rail) => withNotify(rail, agent, channel, to));
			void refreshRail();
		} catch (e) {
			failed = { agent, channel, to, error: (e as Error).message };
		} finally {
			changing = null;
		}
	}

	const health = $derived.by(() => {
		if (!facts) return '';
		const problems = [facts.held ? `${facts.held} held` : '', facts.stuck ? `${facts.stuck} stuck` : ''].filter(Boolean);
		return problems.length ? problems.join(', ') : 'nothing held or stuck';
	});
</script>

<div class="agent-page" class:at-list={atList} style:--list-w={width === null ? null : `${width}px`}>
	<nav class="list" id="agent-list" aria-label={`${page.params.agent}’s conversations`} bind:offsetWidth={shown}>
		<div class="kick">Your agent</div>
		<h1>{page.params.agent}</h1>
		{#if !board.rail}
			<SkeletonRows count={5} height={36} />
		{:else if !me}
			<EmptyState text={`${page.params.agent} is not one of your agents on the shared board.`} />
		{:else}
			<p class="facts">
				{#if facts}
					{KIND[facts.kind] ?? facts.kind} on {facts.host}, {facts.online ? 'online' : 'offline'}. Last reached {ago(facts.last_reached, board.now)};
					<span class:warn={facts.held || facts.stuck}>{health}</span>.
				{:else}
					Not on any host yet.
				{/if}
			</p>
			<h2 class="grp">Direct</h2>
			{#each lists.dms as d (d.target)}
				<a href={dmHref(d.target)} class:on={open === d.target} aria-current={open === d.target ? 'page' : undefined}>
					<span>{@render peer(short(d.target))}</span><time datetime={d.last_at}>{stamp(d.last_at)}</time>
				</a>
			{:else}
				<p class="none">No direct messages yet.</p>
			{/each}
			<h2 class="grp">Channels</h2>
			{#each lists.channels as c (c.target)}
				{@const all = c.notify === 'all'}
				<div class="ch">
					<a href={`#/c/${encodeURIComponent(c.target.slice(1))}`} class:on={open === c.target}>
						<span>{c.target}</span><time datetime={c.last_at}>{stamp(c.last_at)}</time>
					</a>
					<button
						type="button"
						class="notify"
						class:all
						aria-pressed={all}
						aria-label={notifyLabel(page.params.agent ?? '', c.target)}
						aria-busy={changing?.agent === name && changing.channel === c.target}
						title={notifyHint(c.notify ?? 'mentions')}
						onclick={() => setNotify(name, c.target, nextNotify(c.notify ?? 'mentions'))}
					>
						<svg viewBox="0 0 16 16" width="15" height="15" aria-hidden="true">
							<path d="M8 2.1c-2 0-3.5 1.6-3.5 3.6v2.5L3 10.7v.7h10v-.7l-1.5-2.5V5.7c0-2-1.5-3.6-3.5-3.6Z" />
							<path class="clap" d="M6.5 12.9a1.5 1.5 0 0 0 3 0" />
						</svg>
					</button>
				</div>
			{:else}
				<p class="none">In no channel.</p>
			{/each}
			{#if failed && failed.agent === name}
				{@const f = failed}
				<div class="notify-error">
					<ErrorLine what={`Could not change ${f.channel}`} error={f.error} retry={() => setNotify(f.agent, f.channel, f.to)} />
				</div>
			{/if}
			<p class="visible">Visible here: {page.params.agent}’s own DMs and its channels.</p>
		{/if}
	</nav>
	<!-- the window-splitter pattern: a focusable separator is a widget (WAI-ARIA 1.2), which
	     Svelte's checks do not know; arrows resize, Home and End go to the limits -->
	<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
	<div
		class="handle"
		role="separator"
		aria-orientation="vertical"
		aria-controls="agent-list"
		aria-label="Resize the conversation list (double-click resets)"
		aria-valuemin={MIN_WIDTH}
		aria-valuemax={MAX_WIDTH}
		aria-valuenow={width ?? shown}
		tabindex="0"
		onpointerdown={down}
		onpointermove={move}
		onpointerup={up}
		onpointercancel={up}
		onkeydown={key}
		ondblclick={reset}
	></div>
	<div class="open">{@render children()}</div>
</div>

{#snippet peer(n: string)}
	{@const cut = n.indexOf('/') + 1}
	{#if cut > 0}{n.slice(0, cut)}<wbr />{n.slice(cut)}{:else}{n}{/if}
{/snippet}

<style>
	.agent-page {
		display: flex;
		flex: 1;
		min-height: 0;
	}
	.list {
		width: var(--list-w, 290px);
		flex: none;
		display: flex;
		flex-direction: column;
		padding: 34px 28px;
		overflow-y: auto;
	}
	/* the list's right border, widened into a grip */
	.handle {
		position: relative;
		z-index: 1;
		flex: none;
		width: 9px;
		margin: 0 -4px;
		cursor: col-resize;
		touch-action: none;
	}
	.handle::before {
		content: '';
		position: absolute;
		inset: 0 4px;
		background: var(--line);
	}
	.handle:hover::before,
	.handle:active::before {
		inset: 0 3px;
		background: var(--faint);
	}
	.handle:focus-visible {
		outline: none;
	}
	.handle:focus-visible::before {
		inset: 0 3px;
		background: var(--accent);
	}
	.kick,
	.grp {
		margin: 0;
		font: 400 10.5px/1.4 var(--sans);
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--faint);
	}
	.grp {
		margin: 26px 0 6px;
	}
	h1 {
		margin: 6px 0 8px;
		font: 600 34px/1.05 var(--serif);
		overflow-wrap: anywhere;
	}
	.facts {
		margin: 0;
		font: 14.5px/1.5 var(--serif);
		color: var(--muted);
	}
	.warn {
		color: var(--accent);
	}
	.list a {
		display: flex;
		flex: none; /* the column's flex-shrink would squash a wrapped name to min-height */
		justify-content: space-between;
		align-items: baseline;
		gap: 10px;
		min-height: 36px;
		padding: 7px 0 7px 12px;
		margin-left: -14px;
		border-left: 2px solid transparent;
		font: 16.5px/1.3 var(--serif);
		color: var(--muted);
		text-decoration: none;
	}
	.list a span {
		min-width: 0;
		overflow-wrap: anywhere;
	}
	.list a:hover,
	.list a.on {
		color: var(--fg);
	}
	.list a.on {
		border-left-color: var(--accent);
	}
	.ch {
		display: flex;
		align-items: flex-start;
	}
	.ch a {
		flex: 1;
		min-width: 0;
	}
	/* the per-channel push setting: a quiet bell, outlined for mentions, filled for every post */
	.notify {
		flex: none;
		display: grid;
		place-items: center;
		width: 28px;
		height: 36px;
		margin-right: -8px;
		padding: 0;
		border: none;
		border-radius: 4px;
		background: none;
		color: var(--faint);
	}
	.notify svg {
		fill: none;
		stroke: currentColor;
		stroke-width: 1.3;
		stroke-linejoin: round;
		stroke-linecap: round;
	}
	.notify:hover {
		color: var(--fg);
	}
	.notify.all {
		color: var(--accent);
	}
	.notify.all svg path:not(.clap) {
		fill: currentColor;
	}
	.notify[aria-busy='true'] {
		opacity: 0.55;
	}
	.notify-error {
		margin-top: 8px;
		font-size: 13.5px;
	}
	time {
		flex: none;
		font: 12px/1.6 var(--sans);
		color: var(--faint);
	}
	.none {
		margin: 4px 0;
		font-size: 13.5px;
		color: var(--faint);
	}
	.visible {
		margin: auto 0 0;
		padding-top: 24px;
		font: italic 13.5px/1.5 var(--serif);
		color: var(--muted);
	}
	.open {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
	}
	@media (max-width: 1280px) {
		.list {
			width: var(--list-w, 250px);
			padding: 28px 22px;
		}
	}
	@media (max-width: 720px) {
		.list {
			width: auto;
			flex: 1;
			padding: 20px 18px;
			border-right: none;
		}
		.list a {
			margin-left: 0;
			padding-left: 0;
			border-left: none;
		}
		.list a.on {
			color: var(--muted);
		}
		.notify {
			width: 44px;
			height: 44px;
			margin: -4px -12px -4px 0;
		}
		.handle,
		.agent-page:not(.at-list) .list,
		.agent-page.at-list .open {
			display: none;
		}
	}
</style>
