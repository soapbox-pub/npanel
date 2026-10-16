import { useSeoMeta } from '@unhead/react';
import { ArrowUpRight } from 'lucide-react';
import { type ReactNode, useState } from 'react';
import { useNavigate, useParams } from 'react-router-dom';

import { CertificateRow } from '@/components/CertificateRow';
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
import { useDeleteTunnel, useTunnel, useUpdateTunnel } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';
import type { Tunnel, TunnelPatch } from '@/lib/api-types';
import { timeAgo } from '@/lib/format';
import { TILE_COLORS } from '@/lib/tile-colors';
import { tunnelStatus } from '@/lib/tunnel-status';
import { cn } from '@/lib/utils';

const BACK = { to: '/tunnels', label: 'Tunnels' };

/**
 * One tunnel: whether its agent is connected and from where, what the agent
 * forwards to, and what an admin can change about it. Admin only.
 */
export function TunnelDetailPage() {
  const hostname = useParams().hostname;
  const tunnel = useTunnel(hostname);

  useSeoMeta({ title: tunnel.data ? `${tunnel.data.hostname} — npanel` : 'npanel' });

  if (tunnel.isLoading) {
    return (
      <div className="space-y-6">
        <Skeleton className="h-16 w-72 rounded-xl" />
        <Skeleton className="h-40 rounded-2xl" />
      </div>
    );
  }

  if (tunnel.isError || !tunnel.data) {
    return (
      <div className="space-y-6">
        <PageHeader back={BACK} title="Tunnel not found" />
        <p className="text-muted-foreground">
          {tunnel.error instanceof Error ? tunnel.error.message : 'There is no tunnel by that name.'}
        </p>
      </div>
    );
  }

  return <Detail tunnel={tunnel.data} />;
}

function Detail({ tunnel }: { tunnel: Tunnel }) {
  const updateTunnel = useUpdateTunnel();
  const navigate = useNavigate();
  const { toast } = useToast();
  const status = tunnelStatus(tunnel);

  /** Change the tunnel, and say whether it worked. True when it did. */
  const update = async (patch: TunnelPatch, success: string) => {
    try {
      const { tunnel: updated } = await updateTunnel.mutateAsync({ hostname: tunnel.hostname, patch });
      toast({ title: success });
      // A new domain is a new address for this page.
      if (updated.hostname !== tunnel.hostname) {
        navigate(`/tunnels/${encodeURIComponent(updated.hostname)}`, { replace: true });
      }
      return true;
    } catch (error) {
      toast({
        title: "That didn't work",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
      return false;
    }
  };

  return (
    <div className="space-y-8">
      <PageHeader
        back={BACK}
        title={tunnel.hostname}
        subtitle={
          <span className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
            <span className="flex items-center gap-1.5">
              <span className={cn('size-2 shrink-0 rounded-full', TILE_COLORS[status.color])} aria-hidden />
              {status.label}
            </span>
            <span aria-hidden>·</span>
            <span className="truncate">{tunnel.name}</span>
          </span>
        }
        actions={
          <Button asChild className="rounded-full">
            <a href={`https://${tunnel.hostname}`} target="_blank" rel="noreferrer noopener">
              Visit
              <ArrowUpRight aria-hidden />
            </a>
          </Button>
        }
      />

      <Connections tunnel={tunnel} detail={status.detail} />

      <Section title="Agent">
        <Row label="Forwards to" description="Set where npanel tunnel runs, and changed there.">
          <code className="font-mono text-sm">{tunnel.target}</code>
        </Row>
        <EditableField
          id="tunnel-name"
          label="Name"
          value={tunnel.name}
          busy={updateTunnel.isPending}
          onSave={(name) => update({ name }, 'Name changed')}
        />
      </Section>

      <Section title="Serving">
        <ToggleRow
          id="tunnel-enabled"
          label="On"
          description="Off, the domain answers nothing and the agent waits to be turned back on."
          checked={tunnel.enabled}
          disabled={updateTunnel.isPending}
          onCheckedChange={(enabled) => update({ enabled }, enabled ? 'Tunnel turned on' : 'Tunnel turned off')}
        />
        <EditableField
          id="tunnel-hostname"
          label="Domain"
          value={tunnel.hostname}
          busy={updateTunnel.isPending}
          normalize={(value) => value.trim().toLowerCase()}
          onSave={(hostname) => update({ hostname }, 'Domain changed')}
          below={(draft) => <DnsRecords hostname={draft} />}
        />
      </Section>

      {!!tunnel.certificates?.length && (
        <Section title="Certificate">
          {tunnel.certificates.map((certificate) => (
            <CertificateRow key={certificate.hostname} certificate={certificate} />
          ))}
        </Section>
      )}

      <RemoveTunnel hostname={tunnel.hostname} />
    </div>
  );
}

/** Whether the agent is connected, and each connection it holds. */
function Connections({ tunnel, detail }: { tunnel: Tunnel; detail?: string }) {
  if (!tunnel.connections.length) {
    return (
      <Section title="Connections">
        <Row label={tunnel.enabled ? 'None' : 'None while off'} description={detail} />
      </Section>
    );
  }

  return (
    <Section title="Connections">
      {tunnel.connections.map((connection, index) => (
        <Row
          key={index}
          label={connection.remote ? <span className="font-mono">{connection.remote}</span> : 'Address unknown'}
          description={[connection.agent, `connected ${timeAgo(connection.connected_at)}`].filter(Boolean).join(' · ')}
        />
      ))}
    </Section>
  );
}

/**
 * A field that saves on its own, offering its Save button only once it has
 * been changed. `below` is shown under it while there is a change to save.
 */
function EditableField({
  id,
  label,
  value,
  busy,
  onSave,
  normalize = (value) => value.trim(),
  below,
}: {
  id: string;
  label: string;
  value: string;
  busy: boolean;
  onSave(value: string): Promise<boolean>;
  normalize?(value: string): string;
  below?(draft: string): ReactNode;
}) {
  const [draft, setDraft] = useState(value);
  const changed = normalize(draft) !== value && normalize(draft) !== '';

  return (
    <FieldRow label={label} htmlFor={id}>
      <form
        className="space-y-3"
        onSubmit={async (event) => {
          event.preventDefault();
          if (!(await onSave(normalize(draft)))) setDraft(value);
        }}
      >
        <div className="flex gap-2">
          <Input
            id={id}
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            autoComplete="off"
            spellCheck={false}
          />
          {changed && (
            <Button type="submit" disabled={busy}>
              Save
            </Button>
          )}
        </div>
        {changed && below?.(normalize(draft))}
      </form>
    </FieldRow>
  );
}

function RemoveTunnel({ hostname }: { hostname: string }) {
  const deleteTunnel = useDeleteTunnel();
  const navigate = useNavigate();
  const { toast } = useToast();

  const remove = async () => {
    try {
      await deleteTunnel.mutateAsync(hostname);
      toast({ title: `${hostname} removed` });
      navigate('/tunnels');
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
        <button
          type="button"
          className="flex h-12 w-full items-center justify-center gap-2 rounded-xl bg-card text-sm text-destructive surface transition-colors hover:bg-destructive/5 focus-visible:ring-[3px] focus-visible:ring-ring/40 focus-visible:outline-hidden"
        >
          Remove Tunnel…
        </button>
      </AlertDialogTrigger>
      <AlertDialogContent size="sm">
        <AlertDialogHeader>
          <AlertDialogTitle>Remove {hostname}?</AlertDialogTitle>
          <AlertDialogDescription>
            It stops being served right away, and its agent's key stops working here. Running the agent again asks
            to be approved anew.
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

export default TunnelDetailPage;
