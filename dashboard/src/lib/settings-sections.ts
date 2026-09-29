import { BadgeCheck, BarChart3, Globe, House, type LucideIcon, Radio, Rocket, Waypoints } from 'lucide-react';

import type { TileColor } from '@/lib/tile-colors';

export type SettingsSectionId = 'homepage' | 'domains' | 'nostr' | 'deploys' | 'certificates' | 'tor' | 'analytics';

export interface SettingsSection {
  id: SettingsSectionId;
  label: string;
  icon: LucideIcon;
  color: TileColor;
}

/** The gateway's settings, a page each, grouped as the Settings menu shows them. */
export const SETTINGS_SECTIONS: SettingsSection[][] = [
  [
    { id: 'homepage', label: 'Homepage', icon: House, color: 'blue' },
    { id: 'domains', label: 'Domains', icon: Globe, color: 'teal' },
  ],
  [
    { id: 'nostr', label: 'Nostr', icon: Radio, color: 'purple' },
    { id: 'deploys', label: 'Deploys', icon: Rocket, color: 'red' },
    { id: 'certificates', label: 'Certificates', icon: BadgeCheck, color: 'green' },
  ],
  [
    { id: 'tor', label: 'Tor', icon: Waypoints, color: 'indigo' },
    { id: 'analytics', label: 'Analytics', icon: BarChart3, color: 'orange' },
  ],
];

export function findSettingsSection(id: string | undefined): SettingsSection | undefined {
  return SETTINGS_SECTIONS.flat().find((section) => section.id === id);
}
