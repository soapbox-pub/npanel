import { createHash, randomUUID } from "node:crypto";
import { createReadStream } from "node:fs";
import { mkdir, readdir, rename, rm, stat, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import type { Readable } from "node:stream";
import { isPublicUrl } from "./net.ts";

const SHA256 = /^[0-9a-f]{64}$/;

/** Redirects followed before giving up on a blob. */
const MAX_REDIRECTS = 3;

/** Statuses that mean "look somewhere else". */
const REDIRECT_STATUSES = new Set([301, 302, 303, 307, 308]);

/** A download that failed, with fields suitable for the deploy log. */
export class BlobDownloadError extends Error {
  constructor(
    message: string,
    readonly reason: "http" | "too_large" | "hash_mismatch" | "network" | "blocked",
    readonly status?: number,
  ) {
    super(message);
    this.name = "BlobDownloadError";
  }
}

/**
 * Content-addressed store on the local filesystem.
 *
 * Blobs live at `<dir>/<ab>/<cd>/<sha256>` — a two-level fanout keeps any one
 * directory small enough for fast lookups. Downloads are hashed while they
 * stream into a temp file and are only renamed into place once the hash
 * matches, so a truncated transfer or a lying server can never leave a bad
 * blob behind, and the rename is atomic so readers never see a partial file.
 */
export class BlobStore {
  private readonly dir: string;
  private readonly tmpDir: string;
  private readonly isPublic: (url: URL) => Promise<boolean>;

  /**
   * `isPublic` decides whether a download may reach a given address. It
   * defaults to a real DNS-resolving check and is overridden only by tests,
   * whose Blossom servers listen on loopback — exactly what the real check is
   * there to refuse.
   */
  constructor(dir: string, opts: { isPublic?: (url: URL) => Promise<boolean> } = {}) {
    this.dir = resolve(dir);
    this.tmpDir = join(this.dir, "tmp");
    this.isPublic = opts.isPublic ?? isPublicUrl;
  }

  /** Filesystem path a blob occupies, whether or not it exists. */
  path(sha256: string): string {
    assertSha256(sha256);
    return join(this.dir, sha256.slice(0, 2), sha256.slice(2, 4), sha256);
  }

  /** Size in bytes, or undefined when the blob isn't stored. */
  async size(sha256: string): Promise<number | undefined> {
    try {
      const stats = await stat(this.path(sha256));
      return stats.size;
    } catch {
      return undefined;
    }
  }

  async has(sha256: string): Promise<boolean> {
    return (await this.size(sha256)) !== undefined;
  }

  /**
   * Open a blob for reading, optionally a byte range (inclusive, as in HTTP).
   */
  read(sha256: string, range?: { start: number; end: number }): Readable {
    return createReadStream(this.path(sha256), range);
  }

  /** Store bytes already in memory, verifying them against `sha256`. */
  async put(sha256: string, bytes: Uint8Array): Promise<number> {
    assertSha256(sha256);

    const digest = createHash("sha256").update(bytes).digest("hex");
    if (digest !== sha256) {
      throw new BlobDownloadError(`Expected ${sha256}, got ${digest}`, "hash_mismatch");
    }

    const tmp = await this.tempPath();
    await writeFile(tmp, bytes);
    await this.commit(tmp, sha256);

    return bytes.byteLength;
  }

  /**
   * Download a blob from `url`, verifying it hashes to `sha256`.
   *
   * The target — and every redirect it leads to — must resolve to a public
   * address, because the URL ultimately comes from a manifest that anyone can
   * publish.
   *
   * Returns the size in bytes. Throws {@link BlobDownloadError} on any failure,
   * leaving the store untouched.
   */
  async download(
    url: string,
    sha256: string,
    opts: { maxSize: number; signal?: AbortSignal; timeout?: number },
  ): Promise<number> {
    assertSha256(sha256);

    const timeout = AbortSignal.timeout(opts.timeout ?? 30_000);
    const signal = opts.signal ? AbortSignal.any([opts.signal, timeout]) : timeout;

    const response = await request(url, { signal, isPublic: this.isPublic });

    if (!response.ok || !response.body) {
      throw new BlobDownloadError(`HTTP ${response.status}`, "http", response.status);
    }

    // Reject oversized blobs before reading a single byte, when possible.
    const declared = Number(response.headers.get("content-length"));
    if (Number.isFinite(declared) && declared > opts.maxSize) {
      throw new BlobDownloadError(`${declared} bytes exceeds the ${opts.maxSize} byte limit`, "too_large");
    }

    const hash = createHash("sha256");
    let size = 0;

    async function* verified(body: ReadableStream<Uint8Array>): AsyncGenerator<Uint8Array> {
      for await (const chunk of body as unknown as AsyncIterable<Uint8Array>) {
        size += chunk.byteLength;
        // Servers may omit content-length, so enforce the cap mid-stream too.
        if (size > opts.maxSize) {
          throw new BlobDownloadError(`Body exceeds the ${opts.maxSize} byte limit`, "too_large");
        }
        hash.update(chunk);
        yield chunk;
      }
    }

    const tmp = await this.tempPath();
    try {
      await writeFile(tmp, verified(response.body), { signal });
    } catch (error) {
      await rm(tmp, { force: true });
      if (error instanceof BlobDownloadError) throw error;
      throw new BlobDownloadError(String(error instanceof Error ? error.message : error), "network");
    }

    const digest = hash.digest("hex");
    if (digest !== sha256) {
      await rm(tmp, { force: true });
      throw new BlobDownloadError(`Expected ${sha256}, server returned ${digest}`, "hash_mismatch");
    }

    await this.commit(tmp, sha256);

    return size;
  }

  /** Remove a blob. Returns whether it existed. */
  async delete(sha256: string): Promise<boolean> {
    const path = this.path(sha256);
    try {
      await stat(path);
    } catch {
      return false;
    }
    await rm(path, { force: true });
    return true;
  }

  /** Every sha256 currently stored. Used by blob pruning. */
  async *list(): AsyncGenerator<string> {
    let outer: string[];
    try {
      outer = await readdir(this.dir);
    } catch {
      return;
    }

    for (const first of outer) {
      if (first === "tmp" || first.length !== 2) continue;
      for (const second of await readdir(join(this.dir, first))) {
        for (const name of await readdir(join(this.dir, first, second))) {
          if (SHA256.test(name)) yield name;
        }
      }
    }
  }

  /** Delete any temp files left behind by an interrupted download. */
  async clearTemp(): Promise<void> {
    await rm(this.tmpDir, { recursive: true, force: true });
  }

  private async tempPath(): Promise<string> {
    await mkdir(this.tmpDir, { recursive: true });
    return join(this.tmpDir, randomUUID());
  }

  private async commit(tmp: string, sha256: string): Promise<void> {
    const target = this.path(sha256);
    await mkdir(dirname(target), { recursive: true });
    await rename(tmp, target);
  }
}

function assertSha256(value: string): void {
  if (!SHA256.test(value)) {
    throw new Error(`Not a sha256 hash: ${value}`);
  }
}

/**
 * Fetch a URL, following redirects one at a time.
 *
 * `fetch` can follow redirects itself, but then only the first URL is ever
 * checked — a cooperative server could redirect npanel anywhere. Each hop is
 * vetted before it is requested.
 */
async function request(
  url: string,
  opts: { signal: AbortSignal; isPublic: (url: URL) => Promise<boolean> },
): Promise<Response> {
  let target: URL;
  try {
    target = new URL(url);
  } catch {
    throw new BlobDownloadError(`Not a valid URL: ${url}`, "blocked");
  }

  for (let hop = 0; ; hop++) {
    if (target.protocol !== "http:" && target.protocol !== "https:") {
      throw new BlobDownloadError(`Refusing to fetch a ${target.protocol} URL`, "blocked");
    }
    await assertPublic(target, opts.isPublic);

    let response: Response;
    try {
      response = await fetch(target, { signal: opts.signal, redirect: "manual" });
    } catch (error) {
      throw new BlobDownloadError(String(error instanceof Error ? error.message : error), "network");
    }

    if (!REDIRECT_STATUSES.has(response.status)) {
      return response;
    }

    const location = response.headers.get("location");
    await response.body?.cancel().catch(() => {});

    if (!location) {
      throw new BlobDownloadError(`HTTP ${response.status} without a Location header`, "http", response.status);
    }
    if (hop >= MAX_REDIRECTS) {
      throw new BlobDownloadError(`More than ${MAX_REDIRECTS} redirects`, "http", response.status);
    }

    try {
      target = new URL(location, target);
    } catch {
      throw new BlobDownloadError(`Redirected to an invalid URL: ${location}`, "blocked");
    }
  }
}

/** Refuse to fetch anything that isn't on the public internet. */
async function assertPublic(url: URL, isPublic: (url: URL) => Promise<boolean>): Promise<void> {
  let publicAddress: boolean;
  try {
    publicAddress = await isPublic(url);
  } catch (error) {
    throw new BlobDownloadError(String(error instanceof Error ? error.message : error), "network");
  }

  if (!publicAddress) {
    throw new BlobDownloadError(
      `${url.hostname} is not a public address. Publish this site's blobs to a Blossom server reachable on the public internet.`,
      "blocked",
    );
  }
}
