import { useQueryClient } from '@tanstack/react-query';
import { Loader2 } from 'lucide-react';
import { type FormEvent, type ReactNode, useEffect, useMemo, useState } from 'react';

import { Logo } from '@/components/Logo';
import { Perforation } from '@/components/Perforation';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { ServerContext, type ServerContextType } from '@/contexts/ServerContext';
import { parseServerOrigin, probeServer, type Server, saveServer, savedServer, resolveServer } from '@/lib/server';

/**
 * Finds the npanel server this dashboard belongs to before anything asks it a
 * question, and asks the reader for one when the page isn't served by npanel.
 */
export function ServerProvider({ children, initial }: { children: ReactNode; initial?: Server }) {
  const queryClient = useQueryClient();
  const [server, setServer] = useState<Server | undefined | 'resolving'>(initial ?? 'resolving');

  useEffect(() => {
    if (initial) return;
    const controller = new AbortController();
    resolveServer(controller.signal).then((found) => {
      if (controller.signal.aborted) return;
      if (found?.instance) queryClient.setQueryData(['npanel', found.origin, 'instance'], found.instance);
      setServer(found && { origin: found.origin, standalone: found.standalone });
    });
    return () => controller.abort();
  }, [initial, queryClient]);

  const value = useMemo<ServerContextType | undefined>(() => {
    if (server === 'resolving' || !server) return undefined;
    return {
      ...server,
      disconnect: () => {
        saveServer(undefined);
        setServer(undefined);
      },
    };
  }, [server]);

  // One request to the page's own origin, which is usually all it takes, and
  // what follows could be the homepage, the app, or the connect form: no
  // placeholder would look like whichever it turns out to be.
  if (server === 'resolving') return null;

  if (!value) {
    return (
      <ConnectPage
        onConnect={(origin) => {
          saveServer(origin);
          setServer({ origin, standalone: true });
        }}
      />
    );
  }

  return <ServerContext.Provider value={value}>{children}</ServerContext.Provider>;
}

/** Asks which npanel to manage, when this dashboard was opened from somewhere other than one. */
function ConnectPage({ onConnect }: { onConnect: (origin: string) => void }) {
  const [address, setAddress] = useState(() => {
    const saved = savedServer();
    return saved ? new URL(saved).host : '';
  });
  const [error, setError] = useState<string>();
  const [checking, setChecking] = useState(false);

  async function submit(event: FormEvent) {
    event.preventDefault();
    setError(undefined);

    let origin: string;
    try {
      origin = parseServerOrigin(address);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      return;
    }

    setChecking(true);
    const instance = await probeServer(origin);
    setChecking(false);
    if (!instance) {
      setError(`Nothing at ${new URL(origin).host} answered as an npanel dashboard. Check the address: it's the hostname you sign in at.`);
      return;
    }
    onConnect(origin);
  }

  return (
    <div className="relative isolate flex min-h-screen flex-col overflow-x-clip">
      <Perforation className="-inset-x-[50vw] top-0 h-[36rem] [mask-image:radial-gradient(ellipse_50%_70%_at_50%_30%,black,transparent)]" />
      <main className="flex flex-1 items-center justify-center px-4 pb-24">
        <form onSubmit={submit} className="w-full max-w-sm space-y-8" noValidate>
          <div className="space-y-4 text-center">
            <Logo className="mx-auto size-16 drop-shadow-lg" />
            <div className="space-y-2">
              <h1 className="text-2xl font-bold tracking-tight">Connect to npanel</h1>
              <p className="text-muted-foreground">Enter the address of the npanel server you manage.</p>
            </div>
          </div>

          <div className="space-y-2">
            <Label htmlFor="server-address">Dashboard address</Label>
            <Input
              id="server-address"
              inputMode="url"
              autoComplete="url"
              autoCapitalize="none"
              spellCheck={false}
              placeholder="panel.example.com"
              value={address}
              onChange={(e) => setAddress(e.target.value)}
              aria-invalid={error ? true : undefined}
              aria-describedby={error ? 'server-address-error' : undefined}
              autoFocus
            />
            {error && (
              <p id="server-address-error" role="alert" className="text-sm text-destructive">
                {error}
              </p>
            )}
          </div>

          <Button type="submit" className="w-full" disabled={checking}>
            {checking && <Loader2 className="animate-spin" aria-hidden />}
            Connect
          </Button>
        </form>
      </main>
    </div>
  );
}
