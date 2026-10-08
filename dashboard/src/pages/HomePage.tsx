import { useSeoMeta } from '@unhead/react';
import { ArrowLeftRight, ArrowUpRight, ChevronRight, Globe, Signpost, Upload } from 'lucide-react';
import type { ReactNode } from 'react';

import { LoginArea } from '@/components/auth/LoginArea';
import { Logo } from '@/components/Logo';
import { Perforation } from '@/components/Perforation';
import { SiteCover } from '@/components/SiteCover';
import { SiteIcon } from '@/components/SiteIcon';
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar';
import { buttonVariants } from '@/components/ui/button-variants';
import { Skeleton } from '@/components/ui/skeleton';
import { useAuthor } from '@/hooks/useAuthor';
import { useInstance, usePublicSites } from '@/hooks/useNpanel';
import { useServer } from '@/hooks/useServer';
import type { SiteCard } from '@/lib/api-types';
import { decodePubkey, shorten } from '@/lib/format';
import { profileUrl, SOURCE_URL } from '@/lib/links';
import { cn } from '@/lib/utils';

/**
 * What a signed-out visitor sees.
 *
 * Most arrived by following a link to a site this gateway serves, so the page
 * says whose gateway it is, what it's for, and how to run one of their own —
 * and gets out of the way of staff who came to sign in.
 */
export function HomePage() {
  useSeoMeta({
    title: 'npanel',
    description: "Serve your team's nsites from your own domains.",
  });

  return (
    <div className="min-h-screen overflow-x-clip">
      <header className="material sticky top-0 z-30 bg-material">
        <div className="mx-auto flex h-14 max-w-5xl items-center justify-between px-4 sm:px-6">
          <span className="flex items-center gap-2">
            <Logo className="size-7" />
            <span className="text-[15px] font-semibold tracking-tight">npanel</span>
          </span>
          <SwitchServer />
        </div>
      </header>

      <main className="relative isolate mx-auto max-w-5xl space-y-24 px-4 pt-16 pb-16 sm:px-6 sm:pt-24">
        <Perforation className="-inset-x-[50vw] -top-14 h-[36rem] [mask-image:radial-gradient(ellipse_50%_70%_at_50%_30%,black,transparent)]" />
        <Hero />
        <ServedSites />
        <HowItWorks />
        <SelfHost />
      </main>

      <footer className="mx-auto max-w-5xl border-t px-4 py-6 text-xs text-muted-foreground sm:px-6">
        npanel is free software.
      </footer>
    </div>
  );
}

/** When the dashboard was opened apart from its server, the way back to choosing another. */
function SwitchServer() {
  const { origin, standalone, disconnect } = useServer();
  if (!standalone) return null;

  return (
    <button
      type="button"
      onClick={disconnect}
      className="flex min-w-0 items-center gap-1.5 rounded-lg px-2 py-1 text-sm text-muted-foreground transition-colors hover:bg-foreground/[0.06] hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring/40 focus-visible:outline-hidden"
    >
      <span className="truncate">{new URL(origin).host}</span>
      <ArrowLeftRight className="size-3.5 shrink-0" aria-hidden />
      <span className="sr-only">Switch server</span>
    </button>
  );
}

function Hero() {
  return (
    <section className="space-y-8 text-center">
      <Logo className="mx-auto size-24 drop-shadow-xl" />

      <div className="space-y-4">
        <h1 className="text-4xl font-bold tracking-tight text-balance sm:text-6xl">
          Your team's sites, on your own domains.
        </h1>
        <p className="mx-auto max-w-xl text-lg text-balance text-muted-foreground sm:text-xl">
          Publish an nsite to Nostr. Point a domain here. It's live.
        </p>
      </div>

      <div className="flex flex-wrap items-center justify-center gap-x-6 gap-y-3">
        <LoginArea />
        <a href="#self-host" className="inline-flex items-center text-primary hover:opacity-80">
          Run your own
          <ChevronRight className="size-4" aria-hidden />
        </a>
      </div>

      <Operator />
    </section>
  );
}

/** Whose gateway this is — the one thing a visitor from a served site needs to know. */
function Operator() {
  const instance = useInstance();
  const { origin } = useServer();
  const host = instance.data?.dashboard_host ?? new URL(origin).hostname;

  if (instance.isLoading) return <Skeleton className="mx-auto h-9 w-64 rounded-full" />;

  return (
    <div className="mx-auto inline-flex max-w-full items-center gap-2 rounded-full bg-card py-1.5 pr-4 pl-1.5 text-sm surface">
      {instance.data?.contact_npub ? (
        <OperatorName npub={instance.data.contact_npub} host={host} />
      ) : (
        <span className="truncate pl-2.5 text-muted-foreground">{host}</span>
      )}
    </div>
  );
}

function OperatorName({ npub, host }: { npub: string; host: string }) {
  const author = useAuthor(decodePubkey(npub));
  const name = author.data?.metadata?.name;
  const picture = author.data?.metadata?.picture;

  return (
    <>
      <Avatar className="size-6 shrink-0">
        {picture && <AvatarImage src={picture} alt="" />}
        <AvatarFallback className="text-[10px]">{(name ?? npub.slice(5)).slice(0, 2)}</AvatarFallback>
      </Avatar>
      <span className="min-w-0 truncate text-muted-foreground">
        {host} is run by{' '}
        <a
          href={profileUrl(npub)}
          target="_blank"
          rel="noreferrer noopener"
          className="font-medium text-foreground hover:text-primary"
        >
          {name ?? shorten(npub)}
        </a>
      </span>
    </>
  );
}

/**
 * The sites this gateway serves, when its admins have chosen to say. Nothing
 * at all otherwise: a gateway that keeps its list private should look exactly
 * like one with nothing on it.
 */
function ServedSites() {
  const sites = usePublicSites();
  if (!sites.data?.length) return null;

  return (
    <section className="space-y-6">
      <h2 className="text-center text-2xl font-bold tracking-tight sm:text-3xl">Served from here</h2>
      <ul className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
        {sites.data.map((site) => (
          <li key={site.hostname}>
            <SiteCardLink site={site} />
          </li>
        ))}
      </ul>
    </section>
  );
}

/** One site as a link preview, from the metadata and icon of its own landing page. */
function SiteCardLink({ site }: { site: SiteCard }) {
  return (
    <a
      href={`https://${site.hostname}`}
      target="_blank"
      rel="noreferrer noopener"
      className="group flex h-full flex-col overflow-hidden rounded-2xl bg-card surface transition-shadow hover:surface-lift focus-visible:ring-[3px] focus-visible:ring-ring/40 focus-visible:outline-hidden"
    >
      <SiteCover hostname={site.hostname} preview={site} />

      <div className="space-y-3 p-4">
        <div className="flex items-center gap-3">
          <SiteIcon hostname={site.hostname} src={site.icon} className="size-9" />
          <div className="min-w-0 flex-1">
            <p className="truncate font-semibold">{site.title ?? site.hostname}</p>
            <p className="flex items-center gap-1 text-xs text-primary">
              <span className="truncate">{site.hostname}</span>
              <ArrowUpRight className="size-3 shrink-0" aria-hidden />
            </p>
          </div>
        </div>
        {site.description && <p className="line-clamp-2 text-sm text-muted-foreground">{site.description}</p>}
      </div>
    </a>
  );
}

function HowItWorks() {
  return (
    <section className="grid grid-cols-1 gap-4 sm:grid-cols-3">
      <Step icon={<Upload />} tile="bg-linear-to-b from-[#64d2ff] to-[#0a84ff]" title="Publish">
        Push a site to Nostr with any nsite tool.
      </Step>
      <Step icon={<Signpost />} tile="bg-linear-to-b from-[#ffd60a] to-[#ff9f0a]" title="Point">
        Add one DNS record for your domain.
      </Step>
      <Step icon={<Globe />} tile="bg-linear-to-b from-[#30d158] to-[#00a86b]" title="Serve">
        Served over HTTPS from this server's disk, even when relays go quiet.
      </Step>
    </section>
  );
}

function Step({ icon, tile, title, children }: { icon: ReactNode; tile: string; title: string; children: ReactNode }) {
  return (
    <div className="space-y-3 rounded-2xl bg-card p-6 surface">
      <span
        aria-hidden
        className={cn('flex size-11 items-center justify-center rounded-xl text-white shadow-sm [&_svg]:size-5', tile)}
      >
        {icon}
      </span>
      <h3 className="text-lg font-semibold">{title}</h3>
      <p className="text-muted-foreground">{children}</p>
    </div>
  );
}

function SelfHost() {
  return (
    <section id="self-host" className="scroll-mt-20">
      <div className="grid grid-cols-1 items-center gap-10 rounded-3xl bg-card p-6 surface sm:p-10 md:grid-cols-2">
        <div className="space-y-4">
          <h2 className="text-3xl font-bold tracking-tight">Run your own.</h2>
          <p className="text-lg text-muted-foreground">
            One process, one SQLite file, one folder of files. No account with anyone.
          </p>
          <a href={SOURCE_URL} target="_blank" rel="noreferrer noopener" className={cn(buttonVariants(), 'rounded-full px-5')}>
            View source
            <ArrowUpRight aria-hidden />
          </a>
        </div>

        <Terminal />
      </div>
    </section>
  );
}

/** A Terminal window, traffic lights and all. */
function Terminal() {
  return (
    <div className="overflow-hidden rounded-xl bg-[#1e1e1e] font-mono text-[13px] leading-relaxed text-[#e5e5e5] shadow-xl ring-1 ring-black/20">
      <div className="flex items-center gap-2 bg-[#2d2d2d] px-3 py-2" aria-hidden>
        <span className="size-3 rounded-full bg-[#ff5f57]" />
        <span className="size-3 rounded-full bg-[#febc2e]" />
        <span className="size-3 rounded-full bg-[#28c840]" />
      </div>
      <div className="space-y-0.5 overflow-x-auto p-4 whitespace-nowrap">
        <p>
          <span className="text-[#28c840]">$</span> cargo build --release
        </p>
        <p>
          <span className="text-[#28c840]">$</span> (cd dashboard && bun install && bun run build)
        </p>
        <p>
          <span className="text-[#28c840]">$</span> {`echo 'dashboard_host = "panel.example.com"' > npanel.toml`}
        </p>
        <p>
          <span className="text-[#28c840]">$</span> ./target/release/npanel -c npanel.toml
        </p>
        <p className="text-[#8e8e93]">listening on 127.0.0.1:3000</p>
      </div>
    </div>
  );
}

export default HomePage;
