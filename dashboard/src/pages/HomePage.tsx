import { formatNsiteAddress, ROOT_SITE_KIND } from '@shared/nsite-address';
import { useSeoMeta } from '@unhead/react';
import { ExternalLink, Lock } from 'lucide-react';
import { type ReactNode, useState } from 'react';

import { LoginArea } from '@/components/auth/LoginArea';
import { Logo } from '@/components/Logo';
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar';
import { Badge } from '@/components/ui/badge';
import { buttonVariants } from '@/components/ui/button-variants';
import { Card } from '@/components/ui/card';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Skeleton } from '@/components/ui/skeleton';
import { useAuthor } from '@/hooks/useAuthor';
import { useInstance, usePublicSites } from '@/hooks/useNpanel';
import { decodePubkey, shorten } from '@/lib/format';
import { profileUrl, SOURCE_URL } from '@/lib/links';
import { cn } from '@/lib/utils';

/**
 * The NIP-19 specification's example key, used everywhere the demo needs a
 * plausible npub. Nobody's real profile is fetched for it, so the walkthrough
 * can't end up displaying a stranger's name and picture.
 */
const EXAMPLE_NPUB = 'npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6';
const EXAMPLE_ADDRESS = formatNsiteAddress({
  pubkey: '3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d',
  kind: ROOT_SITE_KIND,
  identifier: '',
});

/** Reserved for documentation by RFC 5737 and RFC 3849, so they point nowhere. */
const EXAMPLE_A = '203.0.113.10';
const EXAMPLE_AAAA = '2001:db8::10';

const DEFAULT_DOMAIN = 'docs.example.com';

/**
 * What a signed-out visitor sees.
 *
 * Most of them arrived by following a link to a site this gateway serves and
 * have never heard of npanel, so the page has to do three things in order:
 * admit that it is *an* instance and say who runs it, walk through what the
 * software actually does, and point at the source so they can run their own.
 * It demonstrates rather than describes — the walkthrough is the real UI, and
 * the domain in it is theirs to change.
 */
export function HomePage() {
  useSeoMeta({
    title: 'npanel — a self-hosted nsite gateway',
    description:
      'Point a domain at npanel, map it to an nsite, and your team serves that site from its own domain.',
  });

  const [hostname, setHostname] = useState(DEFAULT_DOMAIN);
  const domain = hostname.trim().toLowerCase() || DEFAULT_DOMAIN;

  return (
    <main className="grid-canvas flex-1">
      <div className="mx-auto max-w-5xl space-y-12 px-4 py-8 md:px-8 md:py-12">
        <InstanceBanner />
        <ServedSites />
        <Walkthrough domain={domain} hostname={hostname} onHostname={setHostname} />
        <SelfHost />
      </div>
    </main>
  );
}

/** Whose gateway this is — the thing a stranger most needs to know. */
function InstanceBanner() {
  const instance = useInstance();

  return (
    <Card className="gap-0 overflow-hidden border-t-2 border-t-primary py-0">
      <div className="flex flex-col gap-6 p-6 sm:flex-row sm:items-center sm:justify-between md:p-8">
        <div className="min-w-0 space-y-3">
          <p className="font-mono text-[11px] font-semibold uppercase tracking-widest text-muted-foreground">
            This is an npanel instance
          </p>
          <p className="truncate font-mono text-xl font-semibold sm:text-2xl">
            {instance.data?.dashboard_host ?? window.location.hostname}
          </p>

          {instance.isLoading ? (
            <Skeleton className="h-10 w-48" />
          ) : instance.data?.contact_npub ? (
            <Operator npub={instance.data.contact_npub} />
          ) : (
            <p className="text-sm text-muted-foreground">Nobody here has said who runs it.</p>
          )}

          <p className="text-sm text-muted-foreground">
            Not your gateway?{' '}
            <a
              href="#self-host"
              className="font-medium text-foreground underline-offset-4 hover:text-primary hover:underline"
            >
              Run your own.
            </a>
          </p>
        </div>

        <div className="shrink-0 space-y-2 sm:text-right">
          <p className="text-sm text-muted-foreground">Staff and team members</p>
          <LoginArea />
        </div>
      </div>
    </Card>
  );
}

function Operator({ npub }: { npub: string }) {
  const author = useAuthor(decodePubkey(npub));
  const name = author.data?.metadata?.name;
  const picture = author.data?.metadata?.picture;

  return (
    <a
      href={profileUrl(npub)}
      target="_blank"
      rel="noreferrer noopener"
      className="group inline-flex max-w-full items-center gap-3 rounded-md focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-hidden"
    >
      <Avatar className="size-10 shrink-0">
        {picture && <AvatarImage src={picture} alt="" />}
        <AvatarFallback>{(name ?? npub.slice(5)).slice(0, 2)}</AvatarFallback>
      </Avatar>
      <span className="min-w-0">
        <span className="block text-xs text-muted-foreground">Operated by</span>
        <span className="block truncate font-medium underline-offset-4 group-hover:text-primary group-hover:underline">
          {name ?? shorten(npub)}
        </span>
      </span>
    </a>
  );
}

/**
 * The sites this gateway serves, when its admins have chosen to say.
 *
 * Renders nothing at all otherwise — an empty "no sites listed" panel would
 * only advertise that there is something being kept quiet, and a gateway that
 * doesn't publish its list should look exactly like one with nothing on it.
 */
function ServedSites() {
  const sites = usePublicSites();

  if (!sites.data?.length) return null;

  return (
    <Card className="gap-0 overflow-hidden py-0">
      <div className="flex items-center justify-between gap-3 border-b bg-muted/50 px-6 py-2.5">
        <h2 className="font-mono text-[11px] font-semibold uppercase tracking-widest text-muted-foreground">
          Served from here
        </h2>
        <span className="font-mono text-xs text-muted-foreground">{sites.data.length}</span>
      </div>
      <ul className="grid gap-2 px-6 py-4 sm:grid-cols-2 lg:grid-cols-3">
        {sites.data.map((site) => (
          <li key={site.hostname} className="flex min-w-0 items-center gap-1.5">
            <a
              href={`https://${site.hostname}`}
              className="truncate font-mono text-sm hover:text-primary hover:underline"
            >
              {site.hostname}
            </a>
            <ExternalLink className="size-3 shrink-0 text-muted-foreground" aria-hidden />
          </li>
        ))}
      </ul>
    </Card>
  );
}

interface WalkthroughProps {
  /** The hostname as the demo should render it, never empty. */
  domain: string;
  /** The raw field value, which may be mid-edit. */
  hostname: string;
  onHostname(value: string): void;
}

/** npanel's whole job, in the order it happens. */
function Walkthrough({ domain, hostname, onHostname }: WalkthroughProps) {
  const instance = useInstance();
  const gateway = instance.data?.dashboard_host ?? window.location.hostname;

  return (
    <section className="space-y-6">
      <div className="max-w-2xl space-y-3">
        <h1 className="text-3xl font-semibold tracking-tight sm:text-4xl">
          Your team's sites, on your own domains.
        </h1>
        <p className="text-lg text-muted-foreground">
          Team members publish nsites to Nostr. Staff point a domain at the gateway and map it.
          Nobody touches a web server, and nothing is uploaded twice.
        </p>
      </div>

      <div className="grid gap-4 md:grid-cols-2">
        <Step index="01" who="Team member" title="Publish an nsite">
          <Terminal />
        </Step>

        <Step index="02" who="Staff" title="Point the domain here">
          <Zone domain={domain} gateway={gateway} />
        </Step>

        <Step index="03" who="Staff" title="Add the mapping">
          <Mapping hostname={hostname} onHostname={onHostname} />
        </Step>

        <Step index="04" who="Anyone" title="Served from that domain">
          <Browser domain={domain} />
        </Step>
      </div>
    </section>
  );
}

function Step({
  index,
  who,
  title,
  children,
}: {
  index: string;
  who: string;
  title: string;
  children: ReactNode;
}) {
  return (
    <Card className="gap-0 overflow-hidden py-0">
      <div className="flex items-center gap-3 border-b bg-muted/50 px-4 py-2.5">
        <span className="font-mono text-[11px] font-semibold tracking-widest text-primary">
          {index}
        </span>
        <h2 className="min-w-0 flex-1 truncate text-sm font-semibold">{title}</h2>
        <span className="shrink-0 rounded-full border px-2 py-0.5 font-mono text-[10px] uppercase tracking-widest text-muted-foreground">
          {who}
        </span>
      </div>
      <div className="p-4">{children}</div>
    </Card>
  );
}

function Terminal() {
  return (
    <div className="space-y-2">
      <div className="rounded-lg bg-sidebar p-3 font-mono text-xs leading-relaxed text-sidebar-foreground">
        <div className="flex gap-1.5 pb-2">
          <span className="size-2 rounded-full bg-white/20" />
          <span className="size-2 rounded-full bg-white/20" />
          <span className="size-2 rounded-full bg-white/20" />
        </div>
        <p className="truncate">
          <span className="text-brand">~/docs $</span> nsite upload ./dist
        </p>
        <p className="truncate text-sidebar-foreground/70">24 files → 2 Blossom servers</p>
        <p className="truncate text-sidebar-foreground/70">manifest signed, sent to 3 relays</p>
        <p className="truncate">
          <span className="text-brand">→</span> {shorten(EXAMPLE_NPUB)}
        </p>
      </div>
      <p className="text-xs text-muted-foreground">
        Any nsite publishing tool does this. The site now exists on Nostr, addressed by its author's
        key — no server, no domain, nothing to point at yet.
      </p>
    </div>
  );
}

function Zone({ domain, gateway }: { domain: string; gateway: string }) {
  return (
    <div className="space-y-2">
      <div className="space-y-2 rounded-lg border bg-muted/40 p-3 font-mono text-xs">
        <p className="truncate font-medium">{domain}</p>

        <dl className="space-y-1">
          <Record type="A" value={EXAMPLE_A} />
          <Record type="AAAA" value={EXAMPLE_AAAA} />
        </dl>

        <div className="flex items-center gap-2 text-[10px] uppercase tracking-widest text-muted-foreground">
          <span className="h-px flex-1 bg-border" />
          or
          <span className="h-px flex-1 bg-border" />
        </div>

        <dl>
          <Record type="CNAME" value={gateway} />
        </dl>
      </div>
      <p className="text-xs text-muted-foreground">
        One record at your DNS provider, and it's the only step npanel can't do for you.
      </p>
    </div>
  );
}

function Record({ type, value }: { type: string; value: string }) {
  return (
    <div className="flex items-baseline gap-3">
      <dt className="w-14 shrink-0 text-muted-foreground">{type}</dt>
      <dd className="min-w-0 truncate">{value}</dd>
    </div>
  );
}

function Mapping({ hostname, onHostname }: { hostname: string; onHostname(value: string): void }) {
  return (
    <div className="space-y-3">
      <div className="space-y-1.5">
        <Label htmlFor="demo-hostname" className="text-xs">
          Domain
        </Label>
        <Input
          id="demo-hostname"
          value={hostname}
          onChange={(event) => onHostname(event.target.value)}
          placeholder={DEFAULT_DOMAIN}
          aria-describedby="demo-hostname-hint"
          autoComplete="off"
          spellCheck={false}
          className="font-mono text-sm"
        />
        <p id="demo-hostname-hint" className="text-xs text-muted-foreground">
          Put your own domain in — it follows through the rest of this page.
        </p>
      </div>

      <div className="space-y-1.5">
        <Label htmlFor="demo-site" className="text-xs">
          nsite
        </Label>
        <Input id="demo-site" value={EXAMPLE_NPUB} readOnly className="font-mono text-sm" />

        <div className="space-y-1 rounded-md border bg-muted/40 p-2.5 text-xs">
          <div className="flex gap-2">
            <span className="w-12 shrink-0 text-muted-foreground">Author</span>
            <span className="min-w-0 truncate font-mono">{shorten(EXAMPLE_NPUB)}</span>
          </div>
          <div className="flex gap-2">
            <span className="w-12 shrink-0 text-muted-foreground">nsite</span>
            <span>Root site</span>
          </div>
          <p className="break-all font-mono text-[11px] text-muted-foreground">{EXAMPLE_ADDRESS}</p>
        </div>
      </div>

      {/* A picture of the button, not the button: there is nothing to submit. */}
      <div aria-hidden className={cn(buttonVariants(), 'pointer-events-none w-full')}>
        Add site
      </div>
    </div>
  );
}

function Browser({ domain }: { domain: string }) {
  return (
    <div className="space-y-2">
      <div className="overflow-hidden rounded-lg border shadow-sm">
        <div className="flex items-center gap-2 border-b bg-muted/60 px-3 py-2">
          <span className="flex shrink-0 gap-1">
            <span className="size-2 rounded-full bg-border" />
            <span className="size-2 rounded-full bg-border" />
            <span className="size-2 rounded-full bg-border" />
          </span>
          <span className="flex min-w-0 flex-1 items-center gap-1.5 rounded-full bg-background px-2.5 py-1 font-mono text-[11px]">
            <Lock className="size-3 shrink-0 text-muted-foreground" aria-hidden />
            <span className="truncate">https://{domain}</span>
          </span>
        </div>

        <div className="bg-background p-4" aria-hidden>
          <div className="h-1 w-full rounded bg-linear-to-r from-brand to-brand-glow" />
          <div className="mt-3 space-y-2">
            <div className="h-3 w-2/5 rounded bg-foreground/70" />
            <div className="h-2 w-full rounded bg-muted-foreground/25" />
            <div className="h-2 w-11/12 rounded bg-muted-foreground/25" />
            <div className="h-2 w-3/5 rounded bg-muted-foreground/25" />
          </div>
          <div className="mt-4 grid grid-cols-3 gap-2">
            <div className="h-10 rounded bg-muted" />
            <div className="h-10 rounded bg-muted" />
            <div className="h-10 rounded bg-muted" />
          </div>
        </div>

        <div className="flex items-center gap-2 border-t bg-muted/40 px-3 py-2">
          <Badge className="shrink-0">Live</Badge>
          <span className="min-w-0 truncate font-mono text-[11px] text-muted-foreground">
            24 files · served from disk
          </span>
        </div>
      </div>
      <p className="text-xs text-muted-foreground">
        A certificate is issued on the first request. Files come off this server's disk, so the site
        stays up when the relays and Blossom servers it came from don't.
      </p>
    </div>
  );
}

/** The point of the page: this is software, and it is meant to be yours. */
function SelfHost() {
  return (
    <section id="self-host" className="scroll-mt-20">
      <Card className="gap-0 overflow-hidden border-t-2 border-t-primary py-0">
        <div className="grid gap-8 p-6 md:grid-cols-2 md:p-8">
          <div className="space-y-4">
            <div className="flex items-center gap-3">
              <Logo className="size-10" />
              <div>
                <p className="text-lg font-semibold tracking-tight">Run your own npanel</p>
                <p className="font-mono text-[10px] uppercase tracking-widest text-muted-foreground">
                  AGPL-3.0 · self-hosted
                </p>
              </div>
            </div>

            <p className="text-muted-foreground">
              One process, one SQLite file, one directory of blobs. No tenant, no account, and
              nobody else's server standing between your team and its domains — which is rather the
              point of publishing to Nostr in the first place.
            </p>

            <a
              href={SOURCE_URL}
              target="_blank"
              rel="noreferrer noopener"
              className={cn(buttonVariants(), 'w-full sm:w-auto')}
            >
              Source code
            </a>
          </div>

          <div className="rounded-lg bg-sidebar p-4 font-mono text-xs leading-relaxed text-sidebar-foreground">
            <p className="truncate">
              <span className="text-brand">$</span> bun install
            </p>
            <p className="truncate">
              <span className="text-brand">$</span> bun run dashboard:build
            </p>
            <p className="truncate">
              <span className="text-brand">$</span> DASHBOARD_HOST=panel.example.com bun start
            </p>
            <p className="truncate pt-1 text-sidebar-foreground/70">listening on 127.0.0.1:3000</p>
          </div>
        </div>
      </Card>
    </section>
  );
}

export default HomePage;
