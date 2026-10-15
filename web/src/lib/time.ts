/** Times as the UI shows them, all in the viewer's local zone. */

export function clock(iso: string): string {
	const d = new Date(iso);
	return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

export function day(iso: string): string {
	const d = new Date(iso);
	const today = new Date();
	const yesterday = new Date(today.getTime() - 86_400_000);
	if (d.toDateString() === today.toDateString()) return 'Today';
	if (d.toDateString() === yesterday.toDateString()) return 'Yesterday';
	return d.toLocaleDateString([], { weekday: 'short', year: 'numeric', month: 'short', day: 'numeric' });
}

/** "12 s ago", "5 min ago", "3 h ago", or a date. */
export function ago(iso: string | null, now: number): string {
	if (!iso) return 'never';
	const s = Math.max(0, Math.round((now - Date.parse(iso)) / 1000));
	if (s < 60) return `${s} s ago`;
	if (s < 3600) return `${Math.floor(s / 60)} min ago`;
	if (s < 86_400) return `${Math.floor(s / 3600)} h ago`;
	return new Date(iso).toLocaleDateString([], { month: 'short', day: 'numeric' });
}

/** Seconds until `iso` as "in 0:23", or "now" once due. */
export function countdown(iso: string, now: number): string {
	const s = Math.round((Date.parse(iso) - now) / 1000);
	if (s <= 0) return 'now';
	const m = Math.floor(s / 60);
	return `in ${m}:${String(s % 60).padStart(2, '0')}`;
}

/** The rail's compact time: clock today, else a short date. */
export function stamp(iso: string | null): string {
	if (!iso) return '';
	const d = new Date(iso);
	if (d.toDateString() === new Date().toDateString()) return clock(iso);
	return d.toLocaleDateString([], { month: 'short', day: 'numeric' });
}

export function bytes(n: number): string {
	if (n < 1024) return `${n} B`;
	if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
	return `${(n / 1024 / 1024).toFixed(1)} MiB`;
}
