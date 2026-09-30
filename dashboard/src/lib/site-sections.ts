import { AtSign, Boxes, Braces, FolderOpen, History, KeyRound, type LucideIcon, Server, Shield } from 'lucide-react';

import type { TileColor } from '@/lib/tile-colors';

export type SiteSectionId = 'nsite' | 'serving' | 'privacy' | 'access' | 'versions' | 'files' | 'nip05' | 'env';

export interface SiteSection {
  id: SiteSectionId;
  label: string;
  icon: LucideIcon;
  color: TileColor;
  adminOnly?: boolean;
}

/**
 * A site's settings, a page each, in the groups its menu shows them in: what
 * it is and how it's served, then what it has published.
 */
export const SITE_SECTIONS: SiteSection[][] = [
  [
    { id: 'nsite', label: 'nsite', icon: Boxes, color: 'indigo' },
    { id: 'serving', label: 'Serving', icon: Server, color: 'green' },
    { id: 'privacy', label: 'Privacy & Security', icon: Shield, color: 'blue' },
    { id: 'access', label: 'Access', icon: KeyRound, color: 'purple', adminOnly: true },
  ],
  [
    { id: 'versions', label: 'Versions', icon: History, color: 'orange' },
    { id: 'files', label: 'Files', icon: FolderOpen, color: 'gray' },
    { id: 'nip05', label: 'NIP-05', icon: AtSign, color: 'teal' },
    { id: 'env', label: 'Environment', icon: Braces, color: 'pink' },
  ],
];

export function findSiteSection(id: string | undefined): SiteSection | undefined {
  return SITE_SECTIONS.flat().find((section) => section.id === id);
}
