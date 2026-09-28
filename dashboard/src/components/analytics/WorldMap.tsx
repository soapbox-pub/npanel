import { geoNaturalEarth1, geoPath } from 'd3-geo';
import type { Feature, Geometry } from 'geojson';
import { useMemo, useState } from 'react';
import { feature } from 'topojson-client';
import type { GeometryCollection, Topology } from 'topojson-specification';
import world from 'visionscarto-world-atlas/world/110m.json';

import { Skeleton } from '@/components/ui/skeleton';
import { useStats } from '@/hooks/useAnalytics';
import alpha3 from '@/lib/analytics/alpha3.json';
import { errorText, flag, formatMetric } from '@/lib/analytics/format';
import { statsQuery } from '@/lib/analytics/query';
import { REPORTS } from '@/lib/analytics/reports';
import { type DashboardState, withFilter } from '@/lib/analytics/state';

const WIDTH = 480;
const HEIGHT = 250;

type Country = Feature<Geometry, { name: string; a3: string }>;

/** Every country's outline, projected once for the lifetime of the page. */
const COUNTRIES: { a3: string; name: string; path: string }[] = (() => {
  const topology = world as unknown as Topology<{ countries: GeometryCollection<{ name: string; a3: string }> }>;
  const collection = feature(topology, topology.objects.countries);
  const features = collection.features as Country[];
  const projection = geoNaturalEarth1().fitSize([WIDTH, HEIGHT], collection);
  const path = geoPath(projection);

  return features
    .filter((country) => country.properties.a3 !== 'ATA')
    .map((country) => ({ a3: country.properties.a3, name: country.properties.name, path: path(country) ?? '' }));
})();

const ALPHA2_BY_ALPHA3 = new Map(Object.entries(alpha3 as Record<string, string>).map(([a2, a3]) => [a3, a2]));

/**
 * Where visitors are, as a map: shaded by how many came from each country,
 * and a click filters by it, like a row of the Countries list.
 */
export default function WorldMap({
  hostname,
  state,
  onChange,
}: {
  hostname: string;
  state: DashboardState;
  onChange(state: DashboardState): void;
}) {
  const [hovered, setHovered] = useState<string | null>(null);

  const stats = useStats(
    hostname,
    statsQuery(state, {
      metrics: ['visitors'],
      dimensions: ['visit:country_name', 'visit:country'],
      filters: REPORTS.countries.alwaysOn,
      compare: false,
    }),
    { live: state.period === 'realtime' },
  );

  const byCountry = useMemo(() => {
    const map = new Map<string, { code: string; name: string; visitors: number }>();
    for (const row of stats.data?.results ?? []) {
      const code = String(row.dimensions[1]);
      map.set(code, { code, name: String(row.dimensions[0]), visitors: row.metrics[0] ?? 0 });
    }
    return map;
  }, [stats.data]);

  if (stats.isLoading) return <Skeleton className="h-64 w-full" />;
  if (stats.isError) return <p className="py-8 text-center text-sm text-destructive">{errorText(stats.error)}</p>;

  const max = Math.max(...[...byCountry.values()].map((country) => country.visitors), 1);
  const hover = hovered ? byCountry.get(ALPHA2_BY_ALPHA3.get(hovered) ?? '') : undefined;
  const hoverName = hovered ? (COUNTRIES.find((country) => country.a3 === hovered)?.name ?? '') : '';

  return (
    <div className="space-y-2">
      <svg viewBox={`0 0 ${WIDTH} ${HEIGHT}`} className="w-full" role="img" aria-label="Visitors by country">
        {COUNTRIES.map((country) => {
          const data = byCountry.get(ALPHA2_BY_ALPHA3.get(country.a3) ?? '');
          const share = data ? 0.15 + 0.85 * (data.visitors / max) : 0;

          return (
            <path
              key={country.a3}
              d={country.path}
              className={data ? 'cursor-pointer fill-primary stroke-background' : 'fill-muted stroke-background'}
              style={data ? { fillOpacity: share } : undefined}
              strokeWidth={0.5}
              onMouseEnter={() => setHovered(country.a3)}
              onMouseLeave={() => setHovered(null)}
              onClick={() =>
                data &&
                onChange(
                  withFilter(state, { operator: 'is', key: 'country', clauses: [data.code] }, { [data.code]: data.name }),
                )
              }
            />
          );
        })}
      </svg>
      <p className="h-5 text-center text-sm text-muted-foreground">
        {hovered &&
          (hover
            ? `${flag(hover.code)} ${hover.name}: ${formatMetric('visitors', hover.visitors)} visitors`
            : `${hoverName}: no visitors`)}
      </p>
    </div>
  );
}
