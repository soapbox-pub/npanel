import { Globe, ScrollText, Settings as SettingsIcon, Users } from 'lucide-react';
import type { ReactNode } from 'react';
import { Link, NavLink, Outlet } from 'react-router-dom';

import { LoginArea } from '@/components/auth/LoginArea';
import { Logo } from '@/components/Logo';
import { ServerInfo } from '@/components/ServerInfo';
import { ThemeToggle } from '@/components/ThemeToggle';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';
import { useCurrentUser } from '@/hooks/useCurrentUser';
import { useMe } from '@/hooks/useNpanel';
import { safeNpub } from '@/lib/format';
import { SOURCE_URL } from '@/lib/links';
import { cn } from '@/lib/utils';
import { HomePage } from '@/pages/HomePage';

const NAV = [
  { to: '/', label: 'Hostnames', icon: Globe, adminOnly: false, end: true },
  { to: '/users', label: 'Users', icon: Users, adminOnly: true, end: false },
  { to: '/settings', label: 'Settings', icon: SettingsIcon, adminOnly: true, end: false },
  { to: '/audit', label: 'Audit log', icon: ScrollText, adminOnly: true, end: false },
];

/** App shell: login gate, navigation, and the signed-in account switcher. */
export function DashboardLayout() {
  const { user } = useCurrentUser();
  const me = useMe();

  return (
    <div className="flex min-h-screen flex-col bg-background">
      <TopBar />

      {!user ? (
        <HomePage />
      ) : me.isLoading ? (
        <Centered>
          <Skeleton className="h-56 w-full max-w-lg" />
        </Centered>
      ) : me.isError || !me.data ? (
        <Centered>
          <NoAccess
            pubkey={user.pubkey}
            message={me.error instanceof Error ? me.error.message : undefined}
          />
        </Centered>
      ) : (
        <div className="flex flex-1 flex-col md:flex-row">
          <Rail admin={me.data.role === 'admin'} />
          <main className="grid-canvas min-w-0 flex-1">
            <div className="mx-auto max-w-6xl px-4 py-6 md:px-8 md:py-8">
              <Outlet />
            </div>
          </main>
        </div>
      )}

      <SiteFooter />
    </div>
  );
}

/** The dark bar across the top, capped with the brand gradient hairline. */
function TopBar() {
  return (
    <header className="sticky top-0 z-40 bg-sidebar text-sidebar-foreground">
      <div className="flex h-14 items-center gap-3 px-4">
        <Link
          to="/"
          className="flex items-center gap-2.5 rounded-md focus-visible:ring-2 focus-visible:ring-sidebar-ring focus-visible:ring-offset-2 focus-visible:ring-offset-sidebar focus-visible:outline-hidden"
        >
          <Logo className="size-8" />
          <span className="text-lg font-semibold tracking-tight text-white">npanel</span>
        </Link>
        <span className="hidden rounded-full border border-sidebar-border px-2.5 py-1 font-mono text-[10px] uppercase tracking-widest text-sidebar-foreground/60 sm:inline-block">
          nsite gateway
        </span>

        <div className="ml-auto flex items-center gap-1">
          <ThemeToggle />
          <LoginArea />
        </div>
      </div>
      <div className="h-0.5 bg-linear-to-r from-brand to-brand-glow" />
    </header>
  );
}

/** Navigation and server readout: a column on desktop, a strip on mobile. */
function Rail({ admin }: { admin: boolean }) {
  return (
    <aside className="bg-sidebar text-sidebar-foreground md:w-56 md:shrink-0 md:border-r md:border-sidebar-border">
      <nav className="flex overflow-x-auto p-2 md:flex-col md:overflow-visible md:py-3">
        {NAV.filter((item) => !item.adminOnly || admin).map((item) => (
          <NavLink
            key={item.to}
            to={item.to}
            end={item.end}
            className={({ isActive }) =>
              cn(
                'flex items-center gap-2.5 whitespace-nowrap rounded-md px-3 py-2 text-sm transition-colors',
                'focus-visible:ring-2 focus-visible:ring-sidebar-ring focus-visible:outline-hidden',
                isActive
                  ? 'bg-sidebar-accent font-medium text-sidebar-accent-foreground md:shadow-[inset_3px_0_0_0_var(--color-brand)]'
                  : 'hover:bg-sidebar-accent/60 hover:text-sidebar-accent-foreground',
              )
            }
          >
            <item.icon className="size-4 shrink-0" />
            {item.label}
          </NavLink>
        ))}
      </nav>

      <div className="hidden md:block">
        <ServerInfo />
      </div>
    </aside>
  );
}

/** The canvas used by the states that have no navigation yet. */
function Centered({ children }: { children: ReactNode }) {
  return (
    <main className="grid-canvas flex flex-1 items-center justify-center px-4 py-12">
      <div className="w-full max-w-lg">{children}</div>
    </main>
  );
}

function SiteFooter() {
  return (
    <footer className="border-t bg-card">
      <div className="flex flex-col gap-1 px-4 py-4 text-xs text-muted-foreground sm:flex-row sm:items-center sm:justify-between md:px-8">
        <span>npanel — a self-hostable nsite gateway.</span>
        <a
          href={SOURCE_URL}
          target="_blank"
          rel="noreferrer noopener"
          className="font-medium text-foreground underline-offset-4 hover:text-primary hover:underline"
        >
          Source code
        </a>
      </div>
    </footer>
  );
}

function NoAccess({ pubkey, message }: { pubkey: string; message?: string }) {
  return (
    <Card className="border-t-2 border-t-destructive">
      <CardHeader>
        <CardTitle className="text-2xl">No access</CardTitle>
        <CardDescription className="text-base">
          {message ?? 'This account cannot use npanel.'}
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-2">
        <p className="text-sm text-muted-foreground">Give an admin your npub so they can add you:</p>
        <code className="block break-all rounded-md bg-muted p-3 font-mono text-xs">
          {safeNpub(pubkey)}
        </code>
      </CardContent>
    </Card>
  );
}
