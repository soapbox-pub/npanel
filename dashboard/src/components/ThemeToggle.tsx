import { Check, Monitor, Moon, Sun } from 'lucide-react';

import type { Theme } from '@/contexts/AppContext';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { useTheme } from '@/hooks/useTheme';

const OPTIONS: { value: Theme; label: string; icon: typeof Sun }[] = [
  { value: 'light', label: 'Light', icon: Sun },
  { value: 'dark', label: 'Dark', icon: Moon },
  { value: 'system', label: 'System', icon: Monitor },
];

/** The three appearances, the current one ticked. */
function AppearanceItems() {
  const { theme, setTheme } = useTheme();

  return OPTIONS.map((option) => (
    <DropdownMenuItem key={option.value} onClick={() => setTheme(option.value)} className="cursor-pointer gap-2">
      <option.icon className="size-4" />
      <span className="flex-1">{option.label}</span>
      {option.value === theme && <Check className="size-4" />}
    </DropdownMenuItem>
  ));
}

/** A button of its own, for the places with no account menu to put Appearance in. */
export function ThemeToggle() {
  const { theme } = useTheme();
  const active = OPTIONS.find((option) => option.value === theme) ?? OPTIONS[2];

  return (
    <DropdownMenu modal={false}>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          aria-label={`Appearance: ${active.label}`}
          className="flex size-9 shrink-0 items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-sidebar-accent hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring/40 focus-visible:outline-hidden"
        >
          <active.icon className="size-4.5" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-40">
        <AppearanceItems />
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** Appearance as a submenu of the account menu, where a Mac app keeps its settings. */
export function AppearanceSubmenu() {
  const { theme } = useTheme();
  const active = OPTIONS.find((option) => option.value === theme) ?? OPTIONS[2];

  return (
    <DropdownMenuSub>
      <DropdownMenuSubTrigger className="gap-2 p-2">
        <active.icon className="size-4" />
        Appearance
      </DropdownMenuSubTrigger>
      <DropdownMenuSubContent className="w-40">
        <AppearanceItems />
      </DropdownMenuSubContent>
    </DropdownMenuSub>
  );
}
