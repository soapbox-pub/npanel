import type { BlobStore } from "./blobs.ts";

/**
 * How much of a document npanel will read looking for meta tags.
 *
 * The tags that matter are in `<head>`, so this only has to cover a large one.
 * Sites are free to inline a megabyte of CSS below it; nothing is gained by
 * parsing that, and a homepage listing twenty sites would pay for all of it.
 */
export const MAX_HTML_BYTES = 128 * 1024;

/** The first {@link MAX_HTML_BYTES} of a stored blob, as text. */
export async function readHtmlHead(blobs: BlobStore, sha256: string): Promise<string> {
  const size = await blobs.size(sha256);
  if (size === undefined || size === 0) return "";

  const stream = blobs.read(sha256, { range: { start: 0, end: Math.min(size, MAX_HTML_BYTES) - 1 } });
  const chunks: Buffer[] = [];
  for await (const chunk of stream) {
    chunks.push(chunk as Buffer);
  }

  return Buffer.concat(chunks).toString("utf8");
}
