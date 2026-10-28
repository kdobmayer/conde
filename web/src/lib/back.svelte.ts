/** The phone top bar's way back (options/README.md "B": "← label" in place of the wordmark), set
 * by the page on screen while it is mounted. */

export interface Back {
	href: string;
	label: string;
}

export const back = $state<{ link: Back | null }>({ link: null });

/** Shows `link` in the top bar until the returned function runs (an effect's cleanup). */
export function showBack(link: Back | null): () => void {
	back.link = link;
	return () => {
		if (back.link === link) back.link = null;
	};
}
