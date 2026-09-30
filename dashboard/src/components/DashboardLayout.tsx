import { Globe, History, Lock, Settings as SettingsIcon, Users } from 'lucide-react';
import type { ReactNode } from 'react';
import { Link, Outlet, useLocation } from 'react-router-dom';

import { LoginArea } from '@/components/auth/LoginArea';
import { CopyButton } from '@/components/CopyButton';
import { Logo } from '@/components/Logo';
import { Perforation } from '@/components/Perforation';
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

      <main className="relative isolate md:pl-64">
        <Perforation />
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

/**
 * The sidebar: a brushed-metal panel floating inside the window's edge, like
 * the faceplate of the machine it describes, with the admin places under a
 * heading of their own and the account at the foot.
 */
function Sidebar({ items }: { items: NavItem[] }) {
  const [main, ...admin] = items;

  return (
    <aside className="brushed fixed top-2 bottom-2 left-2 z-30 hidden w-60 flex-col rounded-2xl text-sidebar-foreground shadow-[0_8px_32px_rgb(0_0_0/0.12),0_0_0_0.5px_rgb(0_0_0/0.15),inset_0_1px_0_rgb(255_255_255/0.9)] md:flex dark:shadow-[0_8px_32px_rgb(0_0_0/0.5),inset_0_0_0_0.5px_rgb(255_255_255/0.1),inset_0_1px_0_rgb(255_255_255/0.08)]">
      <Link
        to="/"
        className="mx-2 mt-3 mb-4 flex items-center gap-2 self-start rounded-lg px-2 py-1 focus-visible:ring-[3px] focus-visible:ring-ring/40 focus-visible:outline-hidden"
      >
        <Logo className="size-6" />
        <span className="text-[15px] font-semibold tracking-tight">npanel</span>
      </Link>

      <nav className="flex-1 space-y-4 overflow-y-auto px-2" aria-label="Main">
        <NavGroup items={[main]} />
        {admin.length > 0 && <NavGroup title="Admin" items={admin} />}
      </nav>

      <div className="p-2">
        <LoginArea showName className="flex w-full" />
      </div>
    </aside>
  );
}

function NavGroup({ title, items }: { title?: string; items: NavItem[] }) {
  const active = useActive();

  return (
    <div>
      {title && <h2 className="px-2.5 pb-1 text-[11px] font-semibold text-muted-foreground">{title}</h2>}
      <ul className="space-y-px">
        {items.map((item) => (
          <li key={item.to}>
            <Link
              to={item.to}
              aria-current={active(item.to) ? 'page' : undefined}
              className={cn(
                'flex h-7 items-center gap-2 rounded-md px-2.5 text-[13px] transition-colors',
                'focus-visible:ring-[3px] focus-visible:ring-ring/40 focus-visible:outline-hidden',
                active(item.to) ? 'bg-foreground/[0.08] font-medium' : 'hover:bg-foreground/[0.04]',
              )}
            >
              <item.icon className="size-4 shrink-0 text-primary" aria-hidden />
              {item.label}
            </Link>
          </li>
        ))}
      </ul>
    </div>
  );
}

function MobileBar() {
  return (
    <header className="material sticky top-0 z-30 flex h-13 items-center gap-2 border-b bg-material px-4 md:hidden">
      <Link to="/" className="flex items-center gap-2 rounded-lg">
        <Logo className="size-7" />
        <span className="text-[15px] font-semibold tracking-tight">npanel</span>
      </Link>
      <LoginArea className="ml-auto" />
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
        <LoginArea />
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
        <div className="flex items-center gap-1 rounded-xl bg-card py-2 pr-2 pl-3 text-left surface">
          <code className="min-w-0 flex-1 truncate font-mono text-xs">{npub}</code>
          <CopyButton value={npub} label="Copy your npub" />
        </div>
      </div>
    </div>
  );
}
