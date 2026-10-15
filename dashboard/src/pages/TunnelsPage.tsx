import { useSeoMeta } from '@unhead/react';
import { Cable, ExternalLink } from 'lucide-react';
import { useState } from 'react';
import { Link } from 'react-router-dom';

import { CertificateRow } from '@/components/CertificateRow';
import { CopyButton } from '@/components/CopyButton';
import { DnsRecords } from '@/components/DnsRecords';
import { FieldRow, Row, Section, ToggleRow } from '@/components/Grouped';
import { PageHeader } from '@/components/PageHeader';
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from '@/components/ui/alert-dialog';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Skeleton } from '@/components/ui/skeleton';
import { useDeleteTunnel, useTunnels, useUpdateTunnel } from '@/hooks/useNpanel';
import { useServer } from '@/hooks/useServer';
import { useToast } from '@/hooks/useToast';
import type { Tunnel, TunnelRequest } from '@/lib/api-types';
import { timeAgo } from '@/lib/format';
import { cn } from '@/lib/utils';

/**
 * Hostnames served from other machines, by agents running `npanel tunnel`:
 * whether each is connected, what it forwards to, and the switches an admin
 * holds over it. Admin only.
 */
export function TunnelsPage() {
  useSeoMeta({ title: 'Tunnels — npanel' });

  const tunnels = useTunnels();
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
          {tunnels.data?.tunnels.length ? (
            tunnels.data.tunnels.map((tunnel) => <TunnelSection key={tunnel.hostname} tunnel={tunnel} />)
          ) : (
            <Empty />
          )}
          {!!tunnels.data?.tunnels.length && <HowTo />}
        </>
      )}
    </div>
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

function TunnelSection({ tunnel }: { tunnel: Tunnel }) {
  const updateTunnel = useUpdateTunnel();
  const { toast } = useToast();
  const [hostname, setHostname] = useState(tunnel.hostname);
  const renaming = hostname.trim().toLowerCase() !== tunnel.hostname;

  const update = async (patch: Parameters<typeof updateTunnel.mutateAsync>[0]['patch'], success: string) => {
    try {
      await updateTunnel.mutateAsync({ hostname: tunnel.hostname, patch });
      toast({ title: success });
    } catch (error) {
      toast({
        title: "That didn't work",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  return (
    <Section
      title={
        <a
          href={`https://${tunnel.hostname}`}
          target="_blank"
          rel="noreferrer"
          className="inline-flex items-center gap-1 hover:text-primary"
        >
          {tunnel.hostname}
          <ExternalLink className="size-3" aria-hidden />
        </a>
      }
    >
      <Status tunnel={tunnel} />
      <Row label="Forwards to" description={`On ${tunnel.name}. Set on that machine, not here.`}>
        <code className="font-mono text-sm">{tunnel.target}</code>
      </Row>
      <ToggleRow
        id={`enabled-${tunnel.hostname}`}
        label="On"
        description="Off, the domain answers nothing and the agent waits to be turned back on."
        checked={tunnel.enabled}
        disabled={updateTunnel.isPending}
        onCheckedChange={(enabled) => update({ enabled }, enabled ? 'Tunnel turned on' : 'Tunnel turned off')}
      />
      <FieldRow label="Domain" htmlFor={`hostname-${tunnel.hostname}`}>
        <form
          className="space-y-3"
          onSubmit={(event) => {
            event.preventDefault();
            void update({ hostname }, 'Domain changed');
          }}
        >
          <div className="flex gap-2">
            <Input
              id={`hostname-${tunnel.hostname}`}
              value={hostname}
              onChange={(event) => setHostname(event.target.value)}
              autoComplete="off"
              spellCheck={false}
            />
            {renaming && (
              <Button type="submit" disabled={!hostname.trim() || updateTunnel.isPending}>
                Save
              </Button>
            )}
          </div>
          {renaming && hostname.trim() && <DnsRecords hostname={hostname} />}
        </form>
      </FieldRow>
      {tunnel.certificates?.map((certificate) => (
        <CertificateRow key={certificate.hostname} certificate={certificate} />
      ))}
      <Row label="Remove" description="The agent's key stops working here. Running it again asks to be approved anew.">
        <RemoveTunnel hostname={tunnel.hostname} />
      </Row>
    </Section>
  );
}

function Status({ tunnel }: { tunnel: Tunnel }) {
  const open = tunnel.connections.length;
  const remotes = [...new Set(tunnel.connections.map((connection) => connection.remote).filter(Boolean))];
  const agent = tunnel.connections[0]?.agent;

  let label: string;
  let description: string;
  if (!tunnel.enabled) {
    label = 'Off';
    description = 'Turned off here.';
  } else if (open) {
    label = 'Connected';
    description = [
      `${open} ${open === 1 ? 'connection' : 'connections'}`,
      remotes.length && `from ${remotes.join(', ')}`,
      agent,
    ]
      .filter(Boolean)
      .join(' · ');
  } else {
    label = 'Offline';
    description = tunnel.connected_at
      ? `Last connected ${timeAgo(tunnel.connected_at)}. Visitors get an error until the agent is running again.`
      : "The agent hasn't connected yet.";
  }

  return (
    <Row
      label={
        <span className="flex items-center gap-1.5">
          <span
            className={cn(
              'size-2 shrink-0 rounded-full',
              !tunnel.enabled ? 'bg-muted-foreground/50' : open ? 'bg-success' : 'bg-destructive',
            )}
            aria-hidden
          />
          {label}
        </span>
      }
      description={description}
    />
  );
}

function RemoveTunnel({ hostname }: { hostname: string }) {
  const deleteTunnel = useDeleteTunnel();
  const { toast } = useToast();

  const remove = async () => {
    try {
      await deleteTunnel.mutateAsync(hostname);
      toast({ title: `${hostname} removed` });
    } catch (error) {
      toast({
        title: "Couldn't remove that tunnel",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  return (
    <AlertDialog>
      <AlertDialogTrigger asChild>
        <Button variant="ghost" size="sm" className="text-destructive hover:bg-destructive/10">
          Remove…
        </Button>
      </AlertDialogTrigger>
      <AlertDialogContent size="sm">
        <AlertDialogHeader>
          <AlertDialogTitle>Remove {hostname}?</AlertDialogTitle>
          <AlertDialogDescription>
            It stops being served right away, and its agent is disconnected for good.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction variant="destructive" onClick={remove}>
            Remove
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

export default TunnelsPage;
