import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

/**
 * Where a site with analytics on serves its tracker, and where the tracker
 * reports to.
 *
 * Both on the site's own origin: a script from anywhere else would need every
 * site's Content Security Policy widened to let it in, and a visitor on the
 * site's onion address could not reach another origin at all. The prefix is a
 * neutral one because the words `plausible` and `analytics` are exactly what
 * blocklists match on, and a site with analytics on has these paths shadowed
 * whatever it published there.
 */
export const TRACKER_PATH = "/.npanel/script.js";
export const EVENT_PATH = "/.npanel/api/event";

/** The published `@plausible-analytics/tracker` module, and a hash naming it. */
export interface TrackerSource {
  code: string;
  /** Hex sha256 of {@link code}, so a new tracker release rebuilds every site. */
  sha256: string;
}

let source: Promise<TrackerSource> | undefined;

/**
 * The tracker module as npm ships it, read once.
 *
 * Plausible's own tracker, unmodified, rather than one of npanel's: it is what
 * the ingest endpoint is modelled on, so the two can't disagree about a field.
 * It is MIT licensed, and resolved through the module system so the version is
 * whatever `bun.lock` pinned.
 */
export function trackerSource(): Promise<TrackerSource> {
  source ??= (async () => {
    const path = fileURLToPath(import.meta.resolve("@plausible-analytics/tracker"));
    const code = await readFile(path, "utf8");
    return { code, sha256: createHash("sha256").update(code).digest("hex") };
  })();

  return source;
}

/**
 * The script one host serves at {@link TRACKER_PATH}: the tracker, and the call
 * that starts it.
 *
 * The domain is baked in rather than read from the page, so a visit over the
 * site's onion address is counted under the site's name — which is how the
 * event server knows which site it was. `bindToWindow` is off so a site that
 * runs Plausible of its own keeps its `window.plausible`, and `logging` is off
 * because a visitor's console is no place for a gateway's chatter.
 */
export function trackerScript(tracker: TrackerSource, domain: string): string {
  const config = { domain, endpoint: EVENT_PATH, bindToWindow: false, logging: false };
  return `${tracker.code.trimEnd()}\ninit(${JSON.stringify(config)});\n`;
}
