/** Gradients a site without an icon is drawn in, picked by its name so it keeps one. */
const TILES = [
  'from-[#64d2ff] to-[#0a84ff]',
  'from-[#ffd60a] to-[#ff9f0a]',
  'from-[#ff9f0a] to-[#ff453a]',
  'from-[#30d158] to-[#00a86b]',
  'from-[#da8fff] to-[#bf5af2]',
  'from-[#7d7aff] to-[#5e5ce6]',
  'from-[#ff6482] to-[#ff2d55]',
  'from-[#6ac4dc] to-[#30b0c7]',
];

/** The gradient classes for a hostname's monogram tile, the same on every visit. */
export function siteTile(hostname: string): string {
  let hash = 0;
  for (const char of hostname) hash = (hash * 31 + char.charCodeAt(0)) >>> 0;
  return TILES[hash % TILES.length];
}
