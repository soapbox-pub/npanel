import { decodeHTML } from "entities";
import { TRACKER_PATH } from "./tracker.ts";

/**
 * Largest document the tracker is added to, in bytes.
 *
 * A build reads each page whole to rewrite it. A page bigger than this is
 * almost certainly a generated dump rather than something a person browses,
 * and is served as published.
 */
export const MAX_INJECT_BYTES = 4 * 1024 * 1024;

/** One directive of a policy: its name, lowercased, and its sources as written. */
interface Directive {
  name: string;
  sources: string[];
}

/**
 * A page with the tracker added, or undefined for one that isn't a document.
 *
 * Edited as bytes rather than parsed and re-serialized, so everything the site
 * wrote comes back exactly as it was apart from the one tag, whatever its
 * encoding: the page is read as Latin-1, which maps every byte to one
 * character and back, and everything written into it is ASCII.
 *
 * The tag goes at the end of `<head>`, and failing that where the head would
 * have ended — before `<body>`, after `<html>`, or after the doctype. A file
 * with none of those is a fragment some page fetches and inserts, and a script
 * tag inserted that way never runs anyway, so it is left alone.
 *
 * Any `<meta>` Content Security Policy in the head is widened by
 * {@link allowAnalytics}, since otherwise the policy the site wrote would stop
 * the script npanel added. `integrity` is the tracker's SRI hash, used only by
 * a policy that trusts scripts by hash alone.
 */
export function injectAnalytics(html: Uint8Array, integrity: string): Uint8Array | undefined {
  const text = Buffer.from(html).toString("latin1");
  const lower = text.toLowerCase();

  const at = insertionPoint(lower);
  if (at === undefined) return undefined;

  const head = text.slice(0, at);
  const { head: allowed, strictDynamic } = allowMetaPolicies(head, integrity);

  // Only a policy that trusts by hash alone needs the tag to carry one, and
  // only there is it worth the cost: a hash pins the script's exact bytes, so
  // a browser still holding the last build's copy would refuse this one.
  const tag = strictDynamic
    ? `<script type="module" src="${TRACKER_PATH}" integrity="${integrity}"></script>`
    : `<script type="module" src="${TRACKER_PATH}"></script>`;

  return Buffer.from(allowed + tag + text.slice(at), "latin1");
}

/** Where in a document the tag goes, from its lowercased text. */
function insertionPoint(lower: string): number | undefined {
  const headEnd = lower.indexOf("</head");
  if (headEnd !== -1) return headEnd;

  const body = lower.search(/<body[\s>]/);
  if (body !== -1) return body;

  for (const opening of [/<html[\s>][^>]*>|<html>/, /<!doctype[^>]*>/]) {
    const match = opening.exec(lower);
    if (match) return match.index + match[0].length;
  }

  return undefined;
}

const META_TAG = /<meta\b[^>]*>/gi;
const ATTRIBUTE = /([^\s"'=/>]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'>]+)))?/g;

/**
 * Every `<meta http-equiv="Content-Security-Policy">` in `head`, widened.
 *
 * Also reports whether any of them trusts scripts by hash alone, since the tag
 * about to be added has to carry its hash for those.
 */
function allowMetaPolicies(head: string, integrity: string): { head: string; strictDynamic: boolean } {
  let strictDynamic = false;

  const rewritten = head.replace(META_TAG, (tag) => {
    let httpEquiv: string | undefined;
    let content: { value: string; start: number; end: number } | undefined;

    for (const match of tag.slice("<meta".length).matchAll(ATTRIBUTE)) {
      const name = match[1].toLowerCase();
      const value = match[2] ?? match[3] ?? match[4] ?? "";
      if (name === "http-equiv") httpEquiv = value.trim().toLowerCase();
      if (name === "content") {
        const start = "<meta".length + (match.index ?? 0);
        content = { value, start, end: start + match[0].length };
      }
    }

    if (httpEquiv !== "content-security-policy" || !content) return tag;

    const policy = decodeHTML(content.value);
    // Nothing a policy can say is outside printable ASCII, and anything that
    // is would not survive being written back as Latin-1.
    if (/[^\t\x20-\x7e]/.test(policy)) return tag;

    if (usesStrictDynamic(policy)) strictDynamic = true;

    const widened = allowAnalytics(policy, integrity);
    if (widened === policy) return tag;

    return `${tag.slice(0, content.start)}content="${escapeAttribute(widened)}"${tag.slice(content.end)}`;
  });

  return { head: rewritten, strictDynamic };
}

/**
 * A Content Security Policy that lets the tracker load and report, and is
 * otherwise exactly what the site wrote.
 *
 * Two things are needed from the site's own origin: loading a script, and
 * `fetch`ing to it. Each is granted by adding `'self'` to the directive that
 * governs it — `script-src-elem` and `script-src` where the site wrote them,
 * `connect-src` likewise, and a copy of `default-src` where it relied on the
 * fallback, so nothing that fallback allowed is taken away. `'none'` gives way
 * to `'self'`, and a directive that already allows the origin is untouched.
 *
 * The one policy `'self'` does nothing for is one using `'strict-dynamic'`,
 * which ignores host sources and trusts scripts by nonce or hash alone. There
 * the tracker's hash is added instead, and the tag carries it as `integrity`.
 * Only there: a hash in a policy that relied on `'unsafe-inline'` would switch
 * that off and break the site's own inline scripts.
 *
 * `policy` may be several policies joined with commas, as a header carries
 * them; each is widened on its own.
 */
export function allowAnalytics(policy: string, integrity: string): string {
  const policies = policy.split(",").map((one) => one.trim());
  const widened = policies.map((one) => allowInPolicy(one, integrity));

  return widened.every((one, index) => one === policies[index]) ? policy : widened.join(", ");
}

function allowInPolicy(policy: string, integrity: string): string {
  const directives: Directive[] = policy
    .split(";")
    .map((directive) => directive.trim())
    .filter(Boolean)
    .map((directive) => {
      const [name, ...sources] = directive.split(/\s+/);
      return { name: name.toLowerCase(), sources };
    });

  const find = (name: string) => directives.find((directive) => directive.name === name);
  const fallback = find("default-src");
  let changed = false;

  /**
   * Widen whichever of `names` the policy wrote, or — where it wrote none and
   * leans on `default-src` — a copy of the fallback under `added`.
   */
  const widen = (names: string[], added: string, hash?: string): void => {
    const present = names.map(find).filter((directive): directive is Directive => directive !== undefined);
    const targets = present.length ? present : fallback ? [{ name: added, sources: [...fallback.sources] }] : [];

    for (const directive of targets) {
      if (allows(directive, hash)) continue;

      grant(directive, hash);
      if (!present.length) directives.push(directive);
      changed = true;
    }
  };

  widen(["script-src-elem", "script-src"], "script-src", integrity);
  widen(["connect-src"], "connect-src");

  if (!changed) return policy;

  return directives.map((directive) => [directive.name, ...directive.sources].join(" ")).join("; ");
}

/**
 * Whether a directive already lets the tracker through: by its hash, under
 * `'strict-dynamic'`, and by the site's own origin otherwise.
 */
function allows(directive: Directive, hash?: string): boolean {
  const sources = directive.sources.map((source) => source.toLowerCase());

  if (hash && sources.includes("'strict-dynamic'")) return directive.sources.includes(`'${hash}'`);

  return sources.includes("'self'") || sources.includes("*");
}

/** Make a directive let the tracker through, in whichever way it needs. */
function grant(directive: Directive, hash?: string): void {
  const sources = directive.sources.map((source) => source.toLowerCase());

  if (hash && sources.includes("'strict-dynamic'")) {
    directive.sources.push(`'${hash}'`);
    return;
  }

  directive.sources = [...directive.sources.filter((source) => source.toLowerCase() !== "'none'"), "'self'"];
}

/** Whether a policy's scripts are trusted by nonce or hash alone. */
function usesStrictDynamic(policy: string): boolean {
  return /'strict-dynamic'/i.test(policy);
}

function escapeAttribute(value: string): string {
  return value.replaceAll("&", "&amp;").replaceAll('"', "&quot;");
}
