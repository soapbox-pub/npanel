/** Links out of the dashboard, to npanel itself and to the wider Nostr web. */

/** Where npanel is developed, for the footer and the homepage's call to action. */
export const SOURCE_URL = 'https://gitworkshop.dev/alex@gleasonator.com/git.shakespeare.diy/npanel';

/**
 * A Nostr profile on the web.
 *
 * The homepage names whoever runs the instance to a visitor who most likely
 * has no Nostr client at all, so the link has to land somewhere a browser can
 * render rather than a `nostr:` URI nothing is registered to handle.
 */
export function profileUrl(identifier: string): string {
  return `https://ditto.pub/${identifier}`;
}
