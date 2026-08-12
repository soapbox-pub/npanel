import { Globe, ScrollText, Settings as SettingsIcon, Users } from 'lucide-react';
import { NavLink, Outlet } from 'react-router-dom';

import { LoginArea } from '@/components/auth/LoginArea';
import { safeNpub } from '@/lib/format';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';
import { useCurrentUser } from '@/hooks/useCurrentUser';
import { useMe } from '@/hooks/useNpanel';
import { cn } from '@/lib/utils';

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
    <div className="min-h-screen bg-background">
      <header className="border-b">
        <div className="mx-auto flex max-w-6xl items-center justify-between gap-4 px-4 py-3">
          <div className="flex items-center gap-2">
            <span className="text-lg font-semibold tracking-tight">npanel</span>
            <span className="hidden text-sm text-muted-foreground sm:inline">nsite gateway</span>
          </div>
          <LoginArea />
        </div>
      </header>

      <main className="mx-auto max-w-6xl px-4 py-6">
        {!user ? (
          <SignedOut />
        ) : me.isLoading ? (
          <div className="space-y-3">
            <Skeleton className="h-8 w-48" />
            <Skeleton className="h-32 w-full" />
          </div>
        ) : me.isError ? (
          <NoAccess pubkey={user.pubkey} message={me.error instanceof Error ? me.error.message : undefined} />
        ) : (
          <div className="flex flex-col gap-6 md:flex-row">
            <nav className="flex gap-1 overflow-x-auto md:w-48 md:flex-col md:overflow-visible">
              {NAV.filter((item) => !item.adminOnly || me.data?.role === 'admin').map((item) => (
                <NavLink
                  key={item.to}
                  to={item.to}
                  end={item.end}
                  className={({ isActive }) =>
                    cn(
                      'flex items-center gap-2 whitespace-nowrap rounded-md px-3 py-2 text-sm transition-colors',
                      isActive ? 'bg-muted font-medium' : 'text-muted-foreground hover:bg-muted/60',
                    )
                  }
                >
                  <item.icon className="h-4 w-4" />
                  {item.label}
                </NavLink>
              ))}
            </nav>

            <div className="min-w-0 flex-1">
              <Outlet />
            </div>
          </div>
        )}
      </main>
    </div>
  );
}

function SignedOut() {
  return (
    <Card className="mx-auto max-w-lg">
      <CardHeader>
        <CardTitle>Sign in</CardTitle>
        <CardDescription>
          npanel authenticates with your Nostr key. Sign in to manage your team's hostnames.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <LoginArea />
      </CardContent>
    </Card>
  );
}

function NoAccess({ pubkey, message }: { pubkey: string; message?: string }) {
  return (
    <Card className="mx-auto max-w-lg">
      <CardHeader>
        <CardTitle>No access</CardTitle>
        <CardDescription>{message ?? 'This account cannot use npanel.'}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-2">
        <p className="text-sm text-muted-foreground">Give an admin your npub so they can add you:</p>
        <code className="block break-all rounded-md bg-muted p-3 text-xs">{safeNpub(pubkey)}</code>
      </CardContent>
    </Card>
  );
}
