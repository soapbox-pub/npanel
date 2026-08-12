import { Monitor, Moon, Sun } from 'lucide-react';

import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { useTheme } from '@/hooks/useTheme';
import type { Theme } from '@/contexts/AppContext';

const OPTIONS: { value: Theme; label: string; icon: typeof Sun }[] = [
  { value: 'light', label: 'Light', icon: Sun },
  { value: 'dark', label: 'Dark', icon: Moon },
  { value: 'system', label: 'System', icon: Monitor },
];

export function ThemeToggle() {
  const { theme, setTheme } = useTheme();
  const active = OPTIONS.find((option) => option.value === theme) ?? OPTIONS[2];

  return (
    <DropdownMenu modal={false}>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          aria-label={`Theme: ${active.label}`}
          className="flex size-9 items-center justify-center rounded-full text-current transition-colors hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-sidebar-ring focus-visible:outline-hidden"
        >
          <active.icon className="size-4.5" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-40">
        {OPTIONS.map((option) => (
          <DropdownMenuItem
            key={option.value}
            onClick={() => setTheme(option.value)}
            className="cursor-pointer gap-2"
          >
            <option.icon className="size-4" />
            <span className="flex-1">{option.label}</span>
            {option.value === theme && <span className="size-1.5 rounded-full bg-primary" />}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
