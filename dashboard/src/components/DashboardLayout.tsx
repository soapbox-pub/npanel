import { Globe, History, Lock, Settings as SettingsIcon, Users } from 'lucide-react';
import type { ReactNode } from 'react';
import { Link, Outlet, useLocation } from 'react-router-dom';

import { LoginArea } from '@/components/auth/LoginArea';
import { CopyButton } from '@/components/CopyButton';
import { Logo } from '@/components/Logo';
import { ThemeToggle } from '@/components/ThemeToggle';
import { Skeleton } from '@/components/ui/skeleton';
import { useCurrentUser } from '@/hooks/useCurrentUser';
import { useMe } from '@/hooks/useNpanel';
import { safeNpub } from '@/lib/format';
import { cn } from '@/lib/utils';
import { HomePage } from '@/pages/HomePage';

const NAV = [
  { to: '/', label: 'Sites', icon: Globe, adminOnly: false },
  { to: '/users', label: 'Users', icon: Users, adminOnly: true },
  { to: '/settings', label: 'Settings', icon: SettingsIcon, adminOnly: true },
  { to: '/audit', label: 'Activity', icon: History, adminOnly: true },
];

type NavItem = (typeof NAV)[number];

/**
 * App shell: a sidebar on a desktop and a tab bar on a phone, the way a Mac
 * app and its iPhone version lay out the same places.
 */
export function DashboardLayout() {
  const { user } = useCurrentUser();
  const me = useMe();

  if (!user) return <HomePage />;

  if (me.isLoading) {
    return (
      <Centered>
        <Skeleton className="mx-auto size-16 rounded-2xl" />
      </Centered>
    );
  }

  if (me.isError || !me.data) {
    return (
      <Centered>
        <NoAccess pubkey={user.pubkey} message={me.error instanceof Error ? me.error.message : undefined} />
      </Centered>
    );
  }

  const items = NAV.filter((item) => !item.adminOnly || me.data.role === 'admin');

  return (
    <div className="min-h-screen">
      <Sidebar items={items} />
      <MobileBar />

      <main className="md:pl-60">
        <div
          className={cn(
            'mx-auto max-w-5xl px-4 pt-4 pb-12 sm:px-6 md:px-10 md:pt-10',
            // Room for the tab bar, which only exists when there is somewhere else to go.
            items.length > 1 && 'pb-28 md:pb-12',
          )}
        >
          <Outlet />
        </div>
      </main>

      {items.length > 1 && <TabBar items={items} />}
    </div>
  );
}

/** Whether a nav item is where the reader is: Sites owns every page under a site. */
function useActive() {
  const { pathname } = useLocation();
  return (to: string) => (to === '/' ? pathname === '/' || pathname.startsWith('/hosts/') : pathname.startsWith(to));
}

function Sidebar({ items }: { items: NavItem[] }) {
  const active = useActive();

  return (
    <aside className="material fixed inset-y-0 left-0 z-30 hidden w-60 flex-col border-r border-sidebar-border bg-sidebar text-sidebar-foreground md:flex">
      <Link
        to="/"
        className="mx-3 mt-4 mb-3 flex items-center gap-2.5 rounded-lg px-2 py-1.5 focus-visible:ring-[3px] focus-visible:ring-ring/40 focus-visible:outline-hidden"
      >
        <Logo className="size-7 drop-shadow-sm" />
        <span className="text-[15px] font-semibold tracking-tight">npanel</span>
      </Link>

      <nav className="flex-1 space-y-0.5 px-3" aria-label="Main">
        {items.map((item) => (
          <Link
            key={item.to}
            to={item.to}
            aria-current={active(item.to) ? 'page' : undefined}
            className={cn(
              'flex h-8 items-center gap-2.5 rounded-md px-2.5 text-sm transition-colors',
              'focus-visible:ring-[3px] focus-visible:ring-ring/40 focus-visible:outline-hidden',
              active(item.to) ? 'bg-sidebar-accent font-medium' : 'hover:bg-sidebar-accent/60',
            )}
          >
            <item.icon className="size-4 shrink-0 text-primary" aria-hidden />
            {item.label}
          </Link>
        ))}
      </nav>

      <div className="flex items-center gap-1 border-t border-sidebar-border p-3">
        <LoginArea showName className="min-w-0 flex-1" />
        <ThemeToggle />
      </div>
    </aside>
  );
}

function MobileBar() {
  return (
    <header className="material sticky top-0 z-30 flex h-13 items-center gap-2 border-b bg-material px-4 md:hidden">
      <Link to="/" className="flex items-center gap-2 rounded-lg">
        <Logo className="size-7" />
        <span className="text-[15px] font-semibold tracking-tight">npanel</span>
      </Link>
      <div className="ml-auto flex items-center gap-1">
        <ThemeToggle />
        <LoginArea />
      </div>
    </header>
  );
}

/** The iPhone's tab bar: every top-level place, one tap away, above the home indicator. */
function TabBar({ items }: { items: NavItem[] }) {
  const active = useActive();

  return (
    <nav
      aria-label="Main"
      className="material fixed inset-x-0 bottom-0 z-30 border-t bg-material pb-[env(safe-area-inset-bottom)] md:hidden"
    >
      <div className="grid" style={{ gridTemplateColumns: `repeat(${items.length}, minmax(0, 1fr))` }}>
        {items.map((item) => (
          <Link
            key={item.to}
            to={item.to}
            aria-current={active(item.to) ? 'page' : undefined}
            className={cn(
              'flex flex-col items-center gap-0.5 pt-2 pb-1.5 text-[10px] font-medium transition-colors',
              active(item.to) ? 'text-primary' : 'text-muted-foreground',
            )}
          >
            <item.icon className="size-6" aria-hidden />
            {item.label}
          </Link>
        ))}
      </div>
    </nav>
  );
}

/** The states before there is anywhere to navigate: a card in the middle of the window. */
function Centered({ children }: { children: ReactNode }) {
  return (
    <div className="flex min-h-screen flex-col">
      <header className="flex h-14 items-center justify-between px-4 sm:px-6">
        <Link to="/" className="flex items-center gap-2">
          <Logo className="size-7" />
          <span className="text-[15px] font-semibold tracking-tight">npanel</span>
        </Link>
        <div className="flex items-center gap-1">
          <ThemeToggle />
          <LoginArea />
        </div>
      </header>
      <main className="flex flex-1 items-center justify-center px-4 pb-24">
        <div className="w-full max-w-sm">{children}</div>
      </main>
    </div>
  );
}

function NoAccess({ pubkey, message }: { pubkey: string; message?: string }) {
  const npub = safeNpub(pubkey);

  return (
    <div className="space-y-6 text-center">
      <div className="mx-auto flex size-16 items-center justify-center rounded-2xl bg-secondary text-muted-foreground">
        <Lock className="size-7" aria-hidden />
      </div>
      <div className="space-y-2">
        <h1 className="text-2xl font-bold tracking-tight">No access</h1>
        <p className="text-muted-foreground">{message ?? 'This account cannot use npanel.'}</p>
      </div>
      <div className="space-y-2">
        <p className="text-sm text-muted-foreground">Send an admin your npub so they can add you.</p>
        <div className="flex items-center gap-1 rounded-xl bg-card py-2 pr-2 pl-3 text-left shadow-sm">
          <code className="min-w-0 flex-1 truncate font-mono text-xs">{npub}</code>
          <CopyButton value={npub} label="Copy your npub" />
        </div>
      </div>
    </div>
  );
}
