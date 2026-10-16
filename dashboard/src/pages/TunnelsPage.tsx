import { useSeoMeta } from '@unhead/react';
import { Cable } from 'lucide-react';
import { Link } from 'react-router-dom';

import { CopyButton } from '@/components/CopyButton';
import { LinkRow, Row, RowIcon, Section } from '@/components/Grouped';
import { PageHeader } from '@/components/PageHeader';
import { Button } from '@/components/ui/button';
import { Skeleton } from '@/components/ui/skeleton';
import { useTunnels } from '@/hooks/useNpanel';
import { useServer } from '@/hooks/useServer';
import type { Tunnel, TunnelRequest } from '@/lib/api-types';
import { timeAgo } from '@/lib/format';
import { tunnelStatus } from '@/lib/tunnel-status';

/**
 * Hostnames served from other machines, by agents running `npanel tunnel`:
 * a row each, which opens the tunnel's own page, and the agents waiting to be
 * approved above them. Admin only.
 */
export function TunnelsPage() {
  useSeoMeta({ title: 'Tunnels — npanel' });

  const tunnels = useTunnels();
  const list = tunnels.data?.tunnels ?? [];
  const requests = tunnels.data?.requests ?? [];

  return (
    <div className="space-y-8">
      <PageHeader title="Tunnels" />

      {tunnels.isLoading ? (
        <Skeleton className="h-48 w-full rounded-xl" />
      ) : tunnels.isError ? (
        <p className="rounded-xl bg-destructive/10 p-4 text-sm text-destructive">
          {tunnels.error instanceof Error ? tunnels.error.message : "Couldn't load tunnels. Reload the page to try again."}
        </p>
      ) : (
        <>
          {requests.length > 0 && <Waiting requests={requests} />}
          {list.length ? (
            <>
              <Section>
                {list.map((tunnel) => (
                  <TunnelRow key={tunnel.hostname} tunnel={tunnel} />
                ))}
              </Section>
              <HowTo />
            </>
          ) : (
            <Empty />
          )}
        </>
      )}
    </div>
  );
}

function TunnelRow({ tunnel }: { tunnel: Tunnel }) {
  const status = tunnelStatus(tunnel);

  return (
    <LinkRow
      to={`/tunnels/${encodeURIComponent(tunnel.hostname)}`}
      icon={
        <RowIcon color={status.color}>
          <Cable />
        </RowIcon>
      }
      label={tunnel.hostname}
      value={`${tunnel.name} · ${status.label}`}
    />
  );
}

/** The command that starts a tunnel to this server. */
function useCommand() {
  const { origin } = useServer();
  return `npanel tunnel ${new URL(origin).host} 8080`;
}

function Command() {
  const command = useCommand();

  return (
    <div className="flex items-center gap-1 rounded-xl bg-muted py-2 pr-2 pl-3 text-left">
      <code className="min-w-0 flex-1 truncate font-mono text-xs">{command}</code>
      <CopyButton value={command} label="Copy the command" />
    </div>
  );
}

function Empty() {
  return (
    <div className="flex flex-col items-center gap-4 rounded-2xl border-2 border-dashed px-8 py-16 text-center">
      <span className="flex size-14 items-center justify-center rounded-2xl bg-primary/10 text-primary">
        <Cable className="size-7" aria-hidden />
      </span>
      <div className="space-y-1">
        <p className="text-lg font-semibold">No tunnels yet</p>
        <p className="max-w-sm text-sm text-muted-foreground">
          Serve a domain from a web server on another machine. Run this there, with the port it listens on, and
          approve it here:
        </p>
      </div>
      <div className="w-full max-w-sm">
        <Command />
      </div>
    </div>
  );
}

function HowTo() {
  return (
    <Section title="Add a tunnel" footer="Run it on the machine with the web server, with the port that server listens on.">
      <div className="p-3">
        <Command />
      </div>
    </Section>
  );
}

/** Agents that have asked to be let in, each waiting on an admin. */
function Waiting({ requests }: { requests: TunnelRequest[] }) {
  return (
    <Section title="Waiting for approval">
      {requests.map((request) => (
        <Row
          key={request.code}
          label={
            <>
              {request.name} <span className="font-mono text-muted-foreground">{request.code}</span>
            </>
          }
          description={[request.target, request.remote && `from ${request.remote}`, `asked ${timeAgo(request.created_at)}`]
            .filter(Boolean)
            .join(' · ')}
        >
          <Button asChild size="sm">
            <Link to={`/tunnels/authorize?code=${encodeURIComponent(request.code)}`}>Review</Link>
          </Button>
        </Row>
      ))}
    </Section>
  );
}

export default TunnelsPage;
