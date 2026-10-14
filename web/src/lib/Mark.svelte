<script lang="ts">
	import { mark } from './mark';
</script>

<svg class="mark" viewBox="0 0 16 16" width="16" height="16" aria-hidden="true" focusable="false">
	<path style="fill: var(--muted)" d={mark.back} />
	<path fill="currentColor" d={mark.front} />
</svg>

<style>
	/* The glyph's bottom edge (y = 14 of 16) sits on the text baseline. */
	.mark {
		display: inline-block;
		vertical-align: -2px;
	}
</style>
