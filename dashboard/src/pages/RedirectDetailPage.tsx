import { useSeoMeta } from '@unhead/react';
import { ArrowUpRight } from 'lucide-react';
import { useNavigate, useParams } from 'react-router-dom';

import { CertificateRow } from '@/components/CertificateRow';
import { DnsRecords } from '@/components/DnsRecords';
import { EditableField } from '@/components/EditableField';
import { Section, ToggleRow } from '@/components/Grouped';
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
import { Skeleton } from '@/components/ui/skeleton';
import { useDeleteRedirect, useRedirect, useUpdateRedirect } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';
import type { Redirect, RedirectPatch } from '@/lib/api-types';
import { redirectSummary } from '@/lib/redirect-summary';

const BACK = { to: '/redirects', label: 'Redirects' };

/** One redirect: where it sends visitors, and how. Admin only. */
export function RedirectDetailPage() {
  const hostname = useParams().hostname;
  const redirect = useRedirect(hostname);

  useSeoMeta({ title: redirect.data ? `${redirect.data.hostname} — npanel` : 'npanel' });

  if (redirect.isLoading) {
    return (
      <div className="space-y-6">
        <Skeleton className="h-16 w-72 rounded-xl" />
        <Skeleton className="h-40 rounded-2xl" />
      </div>
    );
  }

  if (redirect.isError || !redirect.data) {
    return (
      <div className="space-y-6">
        <PageHeader back={BACK} title="Redirect not found" />
        <p className="text-muted-foreground">
          {redirect.error instanceof Error ? redirect.error.message : 'There is no redirect by that name.'}
        </p>
      </div>
    );
  }

  return <Detail redirect={redirect.data} />;
}

function Detail({ redirect }: { redirect: Redirect }) {
  const updateRedirect = useUpdateRedirect();
  const navigate = useNavigate();
  const { toast } = useToast();

  /** Change the redirect, and say whether it worked. True when it did. */
  const update = async (patch: RedirectPatch, success: string) => {
    try {
      const { redirect: updated } = await updateRedirect.mutateAsync({ hostname: redirect.hostname, patch });
      toast({ title: success });
      // A new domain is a new address for this page.
      if (updated.hostname !== redirect.hostname) {
        navigate(`/redirects/${encodeURIComponent(updated.hostname)}`, { replace: true });
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
        title={redirect.hostname}
        subtitle={<span className="truncate">{redirect.enabled ? `To ${redirectSummary(redirect)}` : 'Off'}</span>}
        actions={
          <Button asChild className="rounded-full">
            <a href={`https://${redirect.hostname}`} target="_blank" rel="noreferrer noopener">
              Visit
              <ArrowUpRight aria-hidden />
            </a>
          </Button>
        }
      />

      <Section title="Redirect">
        {/* Keyed by the stored target, which the server spells its own way:
            the field starts over from that rather than offering to save what
            was typed. */}
        <EditableField
          key={redirect.target}
          id="redirect-target"
          label="To"
          value={redirect.target}
          placeholder="https://example.com"
          busy={updateRedirect.isPending}
          onSave={(target) => update({ target }, 'Address changed')}
        />
        <ToggleRow
          id="redirect-keep-path"
          label="Keep the path"
          description="On, /about goes to /about at the new address. Off, every visit lands on the address itself."
          checked={redirect.keep_path}
          disabled={updateRedirect.isPending}
          onCheckedChange={(keep_path) => update({ keep_path }, keep_path ? 'Keeping the path' : 'Not keeping the path')}
        />
        <ToggleRow
          id="redirect-permanent"
          label="Permanent"
          description="On, a 301: search engines move to the new address. Off, a 302, for a move that won't last."
          checked={redirect.permanent}
          disabled={updateRedirect.isPending}
          onCheckedChange={(permanent) => update({ permanent }, permanent ? 'Redirect is permanent' : 'Redirect is temporary')}
        />
      </Section>

      <Section title="Serving">
        <ToggleRow
          id="redirect-enabled"
          label="On"
          description="Off, the domain answers nothing."
          checked={redirect.enabled}
          disabled={updateRedirect.isPending}
          onCheckedChange={(enabled) => update({ enabled }, enabled ? 'Redirect turned on' : 'Redirect turned off')}
        />
        <EditableField
          id="redirect-hostname"
          label="Domain"
          value={redirect.hostname}
          busy={updateRedirect.isPending}
          normalize={(value) => value.trim().toLowerCase()}
          onSave={(hostname) => update({ hostname }, 'Domain changed')}
          below={(draft) => <DnsRecords hostname={draft} />}
        />
      </Section>

      {!!redirect.certificates?.length && (
        <Section title="Certificate">
          {redirect.certificates.map((certificate) => (
            <CertificateRow key={certificate.hostname} certificate={certificate} />
          ))}
        </Section>
      )}

      <RemoveRedirect hostname={redirect.hostname} />
    </div>
  );
}

function RemoveRedirect({ hostname }: { hostname: string }) {
  const deleteRedirect = useDeleteRedirect();
  const navigate = useNavigate();
  const { toast } = useToast();

  const remove = async () => {
    try {
      await deleteRedirect.mutateAsync(hostname);
      toast({ title: `${hostname} removed` });
      navigate('/redirects');
    } catch (error) {
      toast({
        title: "Couldn't remove that redirect",
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
          Remove Redirect…
        </button>
      </AlertDialogTrigger>
      <AlertDialogContent size="sm">
        <AlertDialogHeader>
          <AlertDialogTitle>Remove {hostname}?</AlertDialogTitle>
          <AlertDialogDescription>
            It stops answering right away, and the domain is free for a site or a tunnel.
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

export default RedirectDetailPage;
