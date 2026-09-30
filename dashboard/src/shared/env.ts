/**
 * A site's environment: variables staff set on a host, which its pages read as
 * `window.ENV`.
 *
 * The gateway serves them as a script at `/.npanel/env.js`, loaded from the
 * start of every page's head, so a site that knows to look can read
 * `window.ENV.API_URL` rather than a constant baked in when it was built.
 * Everything in it is public — it is served to every visitor.
 *
 * The dashboard's copy of the rules `src/site_env.rs` enforces on the server,
 * so the variable a form accepts is the variable the server will store.
 * Change both together.
 */

/** Where a host's variables are served. */
export const ENV_PATH = "/.npanel/env.js";

/** Most variables one host may have. */
export const MAX_VARIABLES = 100;

/** Longest name npanel will store. */
const MAX_NAME_LENGTH = 128;

/** Longest value npanel will store, in characters. */
const MAX_VALUE_LENGTH = 4096;

/** Letters, digits and underscores, not starting with a digit. */
const NAME_PATTERN = /^[A-Za-z_][A-Za-z0-9_]*$/;

/**
 * Accept a variable name, exactly as written apart from surrounding space.
 *
 * Names follow the rule shells hold environment variables to, which makes
 * every one a plain identifier, readable as `window.ENV.NAME`. Case is kept:
 * `api_url` and `API_URL` are two variables.
 */
export function parseEnvName(input: string): string {
  const name = input.trim();

  if (!name) {
    throw new Error("Enter a name for this variable.");
  }
  if ([...name].length > MAX_NAME_LENGTH) {
    throw new Error(`A name can be at most ${MAX_NAME_LENGTH} characters.`);
  }
  if (!NAME_PATTERN.test(name)) {
    throw new Error(`A name may only use letters, digits, and _, and can't start with a digit — ${name} doesn't fit.`);
  }

  return name;
}

/** Accept a value. Kept verbatim, spaces and all, and it may be empty. */
export function parseEnvValue(input: string): string {
  if ([...input].length > MAX_VALUE_LENGTH) {
    throw new Error(`A value can be at most ${MAX_VALUE_LENGTH} characters.`);
  }
  return input;
}
