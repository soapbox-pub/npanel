/** The system colours a settings icon tile comes in. */
export const TILE_COLORS = {
  blue: 'bg-[#0a84ff]',
  green: 'bg-[#30d158]',
  orange: 'bg-[#ff9f0a]',
  red: 'bg-[#ff453a]',
  purple: 'bg-[#bf5af2]',
  indigo: 'bg-[#5e5ce6]',
  gray: 'bg-[#8e8e93]',
  teal: 'bg-[#30b0c7]',
  pink: 'bg-[#ff375f]',
} as const;

export type TileColor = keyof typeof TILE_COLORS;
