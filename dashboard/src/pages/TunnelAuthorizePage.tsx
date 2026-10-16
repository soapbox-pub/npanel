import { useSeoMeta } from '@unhead/react';
import { Cable, Check } from 'lucide-react';
import { type ReactNode, useState } from 'react';
import { useNavigate, useSearchParams } from 'react-router-dom';

import { DnsRecords } from '@/components/DnsRecords';
import { PageHeader } from '@/components/PageHeader';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Skeleton } from '@/components/ui/skeleton';
import { useApproveTunnel, useDenyTunnel, useMe, useTunnelRequest } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';
import type { TunnelRequest } from '@/lib/api-types';
import { timeAgo } from '@/lib/format';

const BACK = { to: '/tunnels', label: 'Tunnels' };

/**
 * Where `npanel tunnel` sends whoever runs it: the agent's code to check
 * against the one it printed, and the domain to give it. Admin only.
 */
export function TunnelAuthorizePage() {
  useSeoMeta({ title: 'Authorize a tunnel — npanel' });

  const [params, setParams] = useSearchParams();
  const code = params.get('code') ?? undefined;
  const me = useMe();
  const admin = me.data?.role === 'admin';
  const request = useTunnelRequest(code, admin);

  let body;
  if (!admin) {
    body = <Notice>Only admins can authorize tunnels. Send this page to one.</Notice>;
  } else if (!code) {
    body = <CodeForm onCode={(code) => setParams({ code })} />;
  } else if (request.isLoading) {
    body = <Skeleton className="h-64 w-full rounded-xl" />;
  } else if (!request.data) {
    body = (
      <Notice>
        {request.error instanceof Error ? request.error.message : 'That code has expired, or was already answered.'}
      </Notice>
    );
  } else {
    body = <Approve request={request.data.request} fromYou={request.data.from_you} />;
  }

  return (
    <div className="space-y-8">
      <PageHeader title="Authorize a tunnel" back={BACK} />
      <div className="max-w-lg">{body}</div>
    </div>
  );
}

function Notice({ children }: { children: ReactNode }) {
  return <p className="rounded-xl bg-card p-4 text-sm text-muted-foreground surface">{children}</p>;
}

/** For when the agent's link didn't open: its code, typed in by hand. */
function CodeForm({ onCode }: { onCode(code: string): void }) {
  const [code, setCode] = useState('');

  return (
    <form
      className="space-y-3 rounded-xl bg-card p-4 surface"
      onSubmit={(event) => {
        event.preventDefault();
        onCode(code.trim());
      }}
    >
      <Label htmlFor="code">The code the agent shows</Label>
      <div className="flex gap-2">
        <Input
          id="code"
          value={code}
          onChange={(event) => setCode(event.target.value)}
          placeholder="BCDF-GHJK"
          autoComplete="off"
          spellCheck={false}
          className="font-mono uppercase"
        />
        <Button type="submit" disabled={!code.trim()}>
          Continue
        </Button>
      </div>
    </form>
  );
}

function Approve({ request, fromYou }: { request: TunnelRequest; fromYou: boolean | null }) {
  const approveTunnel = useApproveTunnel();
  const denyTunnel = useDenyTunnel();
  const navigate = useNavigate();
  const { toast } = useToast();
  const [hostname, setHostname] = useState('');
  const domain = hostname.trim().toLowerCase();

  /** Do `action`, then go where it says: the new tunnel's page, or the list. */
  const run = async (action: () => Promise<string>, success: string) => {
    try {
      const next = await action();
      toast({ title: success });
      navigate(next);
    } catch (error) {
      toast({
        title: "That didn't work",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  const busy = approveTunnel.isPending || denyTunnel.isPending;

  return (
    <form
      className="space-y-6"
      onSubmit={(event) => {
        event.preventDefault();
        void run(async () => {
          const { tunnel } = await approveTunnel.mutateAsync({ code: request.code, hostname });
          return `/tunnels/${encodeURIComponent(tunnel.hostname)}`;
        }, `${domain} authorized`);
      }}
    >
      <p className="text-sm text-muted-foreground">
        Approve a tunnel you started yourself, or one someone you know asked you to. It can serve anything on the
        domain you give it.
      </p>
      <div className="space-y-4 rounded-xl bg-card p-5 surface">
        <div className="flex items-center gap-3">
          <span className="flex size-10 shrink-0 items-center justify-center rounded-xl bg-primary/10 text-primary">
            <Cable className="size-5" aria-hidden />
          </span>
          <div className="min-w-0">
            <p className="truncate font-semibold">{request.name}</p>
            <p className="truncate text-sm text-muted-foreground">
              Forwards to <span className="font-mono">{request.target}</span>
            </p>
          </div>
        </div>
        <div className="rounded-xl bg-muted p-4 text-center">
          <p className="font-mono text-3xl font-semibold tracking-[0.2em]">{request.code}</p>
          <p className="mt-1 text-xs text-muted-foreground">Check that the agent shows this same code.</p>
        </div>
        <Provenance request={request} fromYou={fromYou} />
      </div>

      <div className="space-y-2">
        <Label htmlFor="tunnel-hostname">Domain</Label>
        <Input
          id="tunnel-hostname"
          value={hostname}
          onChange={(event) => setHostname(event.target.value)}
          placeholder="dev.example.com"
          autoComplete="off"
          spellCheck={false}
          autoFocus
        />
        <p className="text-xs text-muted-foreground">A domain no site or tunnel here has yet.</p>
        {domain && <DnsRecords hostname={domain} />}
      </div>

      <div className="flex flex-wrap justify-end gap-2">
        <Button
          type="button"
          variant="outline"
          disabled={busy}
          onClick={() =>
            run(async () => {
              await denyTunnel.mutateAsync(request.code);
              return '/tunnels';
            }, `${request.name} turned down`)
          }
        >
          Deny
        </Button>
        <Button type="submit" disabled={!domain || busy}>
          {approveTunnel.isPending ? 'Authorizing…' : 'Authorize'}
        </Button>
      </div>
    </form>
  );
}

/**
 * Where the request came from, set beside where the admin is: a request from
 * their own network is very likely the agent they just started.
 */
function Provenance({ request, fromYou }: { request: TunnelRequest; fromYou: boolean | null }) {
  const when = timeAgo(request.created_at);

  if (fromYou && request.remote) {
    return (
      <p className="flex items-center gap-1.5 text-xs text-muted-foreground">
        <Check className="size-3.5 shrink-0 text-success" aria-hidden />
        Asked {when} from your network ({request.remote}).
      </p>
    );
  }
  if (fromYou === false && request.remote) {
    return (
      <p className="text-xs text-muted-foreground">
        Asked {when} from {request.remote}, which is a different network from the one you're on.
      </p>
    );
  }
  return (
    <p className="text-xs text-muted-foreground">
      Asked {when}
      {request.remote && ` from ${request.remote}`}.
    </p>
  );
}

export default TunnelAuthorizePage;
