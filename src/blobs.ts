import { createHash, randomUUID } from "node:crypto";
import { createReadStream, createWriteStream, openAsBlob } from "node:fs";
import { type FileHandle, mkdir, open, readdir, rename, rm, stat, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import type { Duplex } from "node:stream";
import { pipeline } from "node:stream/promises";
import { constants, createBrotliCompress, createGzip } from "node:zlib";
import { isPublicUrl } from "./net.ts";

const SHA256 = /^[0-9a-f]{64}$/;

/** How much of a file is read per pull. One page-aligned chunk per turn. */
const READ_CHUNK = 64 * 1024;

/**
 * A file's bytes, or a byte range of them, as a web stream.
 *
 * Built on `open`/`read` rather than wrapping `createReadStream`, so the
 * descriptor's whole life is visible here: `pull` reads only what the consumer
 * asked for, and `cancel` — the ordinary end of an abandoned read, not an
 * error — closes the handle instead of leaving it to garbage collection.
 *
 * `end` is inclusive, matching both `createReadStream` and HTTP's `Range`.
 */
export function fileStream(path: string, range?: { start: number; end: number }): ReadableStream<Uint8Array> {
  let handle: FileHandle | undefined;
  let position = range?.start ?? 0;
  const last = range?.end;

  async function close(): Promise<void> {
    const open = handle;
    handle = undefined;
    await open?.close();
  }

  return new ReadableStream<Uint8Array>({
    async start() {
      handle = await open(path);
    },

    async pull(controller) {
      if (!handle) return;

      const wanted = last === undefined ? READ_CHUNK : Math.min(READ_CHUNK, last - position + 1);
      if (wanted <= 0) {
        await close();
        controller.close();
        return;
      }

      const buffer = new Uint8Array(wanted);
      const { bytesRead } = await handle.read(buffer, 0, wanted, position);

      if (bytesRead === 0) {
        await close();
        controller.close();
        return;
      }

      position += bytesRead;
      controller.enqueue(bytesRead === wanted ? buffer : buffer.subarray(0, bytesRead));
    },

    async cancel() {
      await close();
    },
  });
}

/**
 * Compressed copies npanel stores alongside a blob.
 *
 * Both, rather than only the better one: Tor Browser reaches an onion over
 * plain HTTP, and Firefox only offers `br` on HTTPS — so the visitors who gain
 * most from compression are exactly the ones who can only accept `gzip`.
 */
export type Encoding = "br" | "gzip";

export const ENCODINGS: readonly Encoding[] = ["br", "gzip"];

/** File extension each encoding's sidecar gets. */
const SUFFIX: Record<Encoding, string> = { br: ".br", gzip: ".gz" };

/**
 * Blobs too small to bother compressing.
 *
 * Anything under a TCP segment arrives in one packet either way, so a variant
 * could only save the client a decompression, never a round trip.
 */
const MIN_VARIANT_SIZE = 512;

/**
 * How much smaller a variant has to be before it's worth keeping.
 *
 * A copy that saves a percent or two still costs every visitor a decompression
 * and costs the disk a second file.
 */
const MIN_SAVING = 0.05;

/**
 * Size above which brotli drops from its slowest setting.
 *
 * Quality 11 runs around a megabyte a second, which is a fine price to pay
 * once for a page or a bundle, and not one to pay for a large dump that would
 * hold up every other blob in the deploy.
 */
const BROTLI_SLOW_ABOVE = 4 * 1024 * 1024;

/** Sizes of a stored blob and whatever compressed copies it has. */
export interface BlobVariants {
  /** Size of the blob itself, which is always the one that exists. */
  identity: number;
  br?: number;
  gzip?: number;
}

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
 *
 * Compressed copies sit beside their blob as `<sha256>.br` and `<sha256>.gz`.
 * They are an attribute of a blob rather than blobs themselves: nothing
 * references them by name, {@link list} doesn't yield them, and {@link delete}
 * takes them along — so pruning keeps working on hashes alone and a sidecar
 * can never outlive what it was made from.
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

  /** Filesystem path one of a blob's compressed copies occupies. */
  variantPath(sha256: string, encoding: Encoding): string {
    return this.path(sha256) + SUFFIX[encoding];
  }

  /** Size in bytes, or undefined when the blob isn't stored. */
  async size(sha256: string): Promise<number | undefined> {
    return sizeOf(this.path(sha256));
  }

  async has(sha256: string): Promise<boolean> {
    return (await this.size(sha256)) !== undefined;
  }

  /**
   * Sizes of a blob and every compressed copy it has, or undefined when the
   * blob isn't stored.
   *
   * All three are stat'd at once because the caller is about to pick the
   * smallest of them, and it can't know which without asking.
   */
  async variants(sha256: string): Promise<BlobVariants | undefined> {
    const [identity, br, gzip] = await Promise.all([
      this.size(sha256),
      sizeOf(this.variantPath(sha256, "br")),
      sizeOf(this.variantPath(sha256, "gzip")),
    ]);

    if (identity === undefined) return undefined;

    return { identity, br, gzip };
  }

  /**
   * Open a blob for reading — or one of its compressed copies, and optionally
   * a byte range within whichever was asked for (inclusive, as in HTTP).
   *
   * For npanel's own consumption of the bytes. A response body should come
   * from {@link open} instead, which keeps the bytes out of the process.
   */
  read(
    sha256: string,
    opts: { encoding?: Encoding; range?: { start: number; end: number } } = {},
  ): ReadableStream<Uint8Array> {
    const path = opts.encoding ? this.variantPath(sha256, opts.encoding) : this.path(sha256);
    return fileStream(path, opts.range);
  }

  /**
   * The first `maxBytes` of a blob, decoded as UTF-8.
   *
   * For the handful of places npanel reads a site's own file to answer a
   * question about it — a document's `<meta>` policy, a `robots.txt` — rather
   * than to serve it. The cap is the whole point: these run on the serving
   * path, and a site is free to publish a file of any size at any of those
   * names.
   */
  async readText(sha256: string, maxBytes: number): Promise<string> {
    const size = await this.size(sha256);
    if (size === undefined || size === 0) return "";

    const reader = this.read(sha256, { range: { start: 0, end: Math.min(size, maxBytes) - 1 } }).getReader();
    const chunks: Uint8Array[] = [];

    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        chunks.push(value);
      }
    } finally {
      reader.releaseLock();
    }

    return Buffer.concat(chunks).toString("utf8");
  }

  /**
   * A blob — or one of its compressed copies, optionally a byte range of
   * whichever was asked for (inclusive, as in HTTP) — as a file-backed Blob.
   *
   * This is what a `Response` body is made from: Bun recognizes a file-backed
   * Blob and answers with `sendfile`, so the bytes travel disk → socket inside
   * the kernel without ever entering the JS heap, and the kernel tears the
   * transfer down when the visitor disconnects. Slicing keeps that path —
   * verified by strace, one `sendfile` call per response, offsets intact.
   * Wrapping the file in a ReadableStream instead would put every chunk
   * through the process, which is the copying this exists to avoid.
   */
  async open(
    sha256: string,
    opts: { encoding?: Encoding; range?: { start: number; end: number } } = {},
  ): Promise<Blob> {
    const path = opts.encoding ? this.variantPath(sha256, opts.encoding) : this.path(sha256);
    const blob = await openAsBlob(path);
    // Blob.slice is end-exclusive where HTTP's Range is inclusive.
    return opts.range ? blob.slice(opts.range.start, opts.range.end + 1) : blob;
  }

  /**
   * Compress a stored blob, if it isn't already and it's worth doing.
   *
   * Returns the encodings written by this call, so an idempotent re-run over a
   * store that already has them reports nothing rather than pretending to have
   * done work. Callers decide *whether* to compress — this only knows how.
   */
  async ensureVariants(sha256: string): Promise<Encoding[]> {
    const size = await this.size(sha256);
    if (size === undefined || size < MIN_VARIANT_SIZE) return [];

    const written: Encoding[] = [];

    // One encoding at a time: a deploy already compresses several blobs at
    // once, and doubling that again would just contend for the same cores.
    for (const encoding of ENCODINGS) {
      if ((await sizeOf(this.variantPath(sha256, encoding))) !== undefined) continue;
      if (await this.writeVariant(sha256, encoding, size)) written.push(encoding);
    }

    return written;
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

  /** Remove a blob and its compressed copies. Returns whether it existed. */
  async delete(sha256: string): Promise<boolean> {
    const existed = await this.has(sha256);

    await rm(this.path(sha256), { force: true });
    for (const encoding of ENCODINGS) {
      await rm(this.variantPath(sha256, encoding), { force: true });
    }

    return existed;
  }

  /**
   * Every sha256 currently stored. Used by blob pruning.
   *
   * Compressed sidecars are skipped by the same test that keeps stray files
   * out: their names carry a suffix, so they are not 64 hex characters.
   */
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

  /**
   * Compress one blob into one encoding. Returns whether the result was worth
   * keeping.
   *
   * Streamed rather than buffered, so a blob at the top of `max_file_size`
   * costs a fixed amount of memory, and written through a temp file for the
   * same reason downloads are: an interrupted pass must not leave a truncated
   * variant behind, because nothing downstream re-checks a sidecar's contents
   * the way a blob's hash gets re-checked.
   */
  private async writeVariant(sha256: string, encoding: Encoding, size: number): Promise<boolean> {
    const tmp = await this.tempPath();

    try {
      await pipeline(createReadStream(this.path(sha256)), compressor(encoding, size), createWriteStream(tmp));

      const compressed = await sizeOf(tmp);
      if (compressed === undefined || compressed > size * (1 - MIN_SAVING)) {
        await rm(tmp, { force: true });
        return false;
      }

      await rename(tmp, this.variantPath(sha256, encoding));
      return true;
    } catch (error) {
      await rm(tmp, { force: true });
      throw error;
    }
  }
}

function assertSha256(value: string): void {
  if (!SHA256.test(value)) {
    throw new Error(`Not a sha256 hash: ${value}`);
  }
}

/** Size of a file, or undefined when it isn't there. */
async function sizeOf(path: string): Promise<number | undefined> {
  try {
    const stats = await stat(path);
    return stats.size;
  } catch {
    return undefined;
  }
}

/** The compressor for an encoding, tuned for a blob of a known size. */
function compressor(encoding: Encoding, size: number): Duplex {
  if (encoding === "gzip") {
    return createGzip({ level: 9 });
  }

  return createBrotliCompress({
    params: {
      [constants.BROTLI_PARAM_QUALITY]: size > BROTLI_SLOW_ABOVE ? 5 : 11,
      // A window as large as the input, capped at what every decoder handles
      // without the large-window extension.
      [constants.BROTLI_PARAM_LGWIN]: 24,
      [constants.BROTLI_PARAM_SIZE_HINT]: size,
    },
  });
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
