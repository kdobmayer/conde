/**
 * The colour theme, a per-browser choice: system (the default, follows prefers-color-scheme),
 * light or dark, stored under `conde.theme`. The inline script in app.html (and its copy in the
 * server's pages, crates/conde-remote/src/web/pages.rs) sets `data-theme` on the root element
 * from the same key before the first paint and follows the system and other tabs; the user
 * menu changes it through theme.svelte.ts.
 */

export type Choice = 'system' | 'light' | 'dark';
export const CHOICES: Choice[] = ['system', 'light', 'dark'];
export const LABELS: Record<Choice, string> = { system: 'System', light: 'Light', dark: 'Dark' };
export const THEME_KEY = 'conde.theme';

/** A stored value as a choice: anything but `light` or `dark` is `system`. */
export function asChoice(v: string | null | undefined): Choice {
	return v === 'light' || v === 'dark' ? v : 'system';
}

/** The theme a choice draws in. */
export function resolve(choice: Choice, systemLight: boolean): 'light' | 'dark' {
	return choice === 'system' ? (systemLight ? 'light' : 'dark') : choice;
}
