<script lang="ts">
	// Machine: the person's agent machine, from `machine.status`; refetched on the machine and fleet
	// topics and every 30 s while visible (heartbeats announce nothing). A first sign-in lands here.
	import { onMount } from 'svelte';
	import { mutate, rpc, RpcError } from '#lib/api.ts';
	import { board, onBoard } from '#lib/board.svelte.ts';
	import { machineView, skew, type Machine } from '#lib/machine.ts';
	import Disclosure from '#lib/ui/Disclosure.svelte';
	import ErrorLine from '#lib/ui/ErrorLine.svelte';
	import SkeletonRows from '#lib/ui/SkeletonRows.svelte';

	let machine = $state<Machine | null>(null);
	let loaded = $state(false);
	let error = $state('');
	let failedSetup = $state('');
	let busy = $state(false);

	async function load(): Promise<void> {
		try {
			const asked = Date.now();
			const m = await rpc<Machine>('machine.status');
			board.skewMs = skew(m.now, asked, Date.now());
			machine = m;
			error = '';
		} catch (e) {
			if (e instanceof RpcError && e.code === 'not_found') {
				machine = null;
				error = '';
			} else error = (e as Error).message;
		} finally {
			loaded = true;
		}
	}

	async function setup(): Promise<void> {
		busy = true;
		try {
			machine = await mutate<Machine>('machine.setup');
			failedSetup = '';
		} catch (e) {
			failedSetup = (e as Error).message;
		} finally {
			busy = false;
		}
	}

	onMount(() => {
		void load();
		const off = onBoard((topics) => {
			if (topics.includes('machine') || topics.includes('fleet')) void load();
		});
		const timer = setInterval(() => {
			if (document.visibilityState === 'visible') void load();
		}, 30_000);
		return () => {
			off();
			clearInterval(timer);
		};
	});

	const now = $derived(board.now + board.skewMs);
	const lastSeen = $derived(
		board.fleet?.hosts.find((h) => h.owner === board.session?.user && h.host === machine?.host_id)?.last_seen ?? null
	);
	const view = $derived(loaded ? machineView(machine, now, lastSeen) : null);
</script>

<section class="machine">
	<header class="head">
		<h1>Machine</h1>
		<p class="deck">Your agent’s own machine: where it runs, always on, signed in as you.</p>
	</header>

	<div class="body">
		{#if !view}
			{#if error}
				<ErrorLine {error} retry={() => void load()} />
			{:else}
				<SkeletonRows count={2} height={40} />
			{/if}
		{:else}
			{#if error}
				<ErrorLine {error} retry={() => void load()} />
			{/if}
			<p class="state" data-state={machine?.state ?? 'none'}>{view.headline}</p>
			<p class="detail">{view.detail}</p>
			{#if machine}<p class="host">{machine.host_id}</p>{/if}
			{#if view.claude}
				<p class="line">Sign in to Claude on the machine: attach to the agent’s session from a terminal (below) and run <code>/login</code>.</p>
			{/if}
			{#if view.signIn.length > 0}
				<p class="line">Needs sign-in: {view.signIn.join(', ')}.</p>
			{/if}
			{#if view.setup}
				<button type="button" class="setup" disabled={busy} onclick={() => void setup()}>{view.setup}</button>
			{/if}
			{#if failedSetup}
				<ErrorLine error={failedSetup} what="Could not set up" retry={() => void setup()} />
			{/if}
			{#if machine}
				<div class="hatch">
					<Disclosure label="From a terminal">
						<p>Over the hub VPN, with your Entra account:</p>
						<pre>az ssh vm -g conde-vm -n {machine.host_id}
sudo -u agent tmux -L agent attach -t agent</pre>
						<p>The second line attaches to your agent’s own session; Ctrl-b d detaches. <code>journalctl -u agentvm-firstboot</code> shows how the machine joined the board.</p>
					</Disclosure>
				</div>
			{/if}
		{/if}
	</div>
</section>

<style>
	.head {
		padding: 34px 48px 18px;
		border-bottom: 1px solid var(--line);
	}
	h1 {
		margin: 0;
		font: 600 44px/1 var(--serif);
		letter-spacing: -0.015em;
	}
	.deck {
		margin: 10px 0 4px;
		max-width: 560px;
		font: 17px/1.45 var(--serif);
		color: var(--muted);
	}
	.body {
		padding: 22px 48px 40px;
		max-width: 760px;
	}
	.state {
		margin: 0;
		font: 600 24px/1.25 var(--serif);
	}
	.detail,
	.line {
		margin: 8px 0 0;
		font-size: 15px;
		line-height: 1.5;
		color: var(--muted);
		overflow-wrap: anywhere;
	}
	.host {
		margin: 6px 0 0;
		font-size: 12.5px;
		color: var(--faint);
	}
	.setup {
		margin-top: 18px;
	}
	.hatch {
		margin-top: 28px;
	}
	pre {
		overflow-x: auto;
		font-size: 12.5px;
		line-height: 1.5;
	}
	@media (max-width: 1100px) {
		.head,
		.body {
			padding-left: 36px;
			padding-right: 36px;
		}
	}
	@media (max-width: 720px) {
		.head {
			padding: 18px 18px 12px;
		}
		h1 {
			font-size: 30px;
		}
		.deck {
			font-size: 14px;
			margin: 6px 0 2px;
		}
		.body {
			padding: 16px 18px 30px;
		}
	}
</style>
