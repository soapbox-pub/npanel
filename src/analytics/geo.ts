import { createWriteStream } from "node:fs";
import { mkdir, rename, rm } from "node:fs/promises";
import { isIP } from "node:net";
import { join } from "node:path";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { createGunzip } from "node:zlib";
import { type Logger, errFields } from "../log.ts";
import { MmdbFile } from "./mmdb.ts";

/** Where a visitor was, as far as their address says. Empty strings mean unknown. */
export interface Location {
  /** ISO 3166-1 alpha-2. */
  country: string;
  /** English name of the first-level subdivision — a state, a province. */
  region: string;
  /** English name of the city. */
  city: string;
}

/** The fields of a City database record that a location is made of. */
interface CityRecord {
  country?: { iso_code?: string };
  subdivisions?: { names?: { en?: string } }[];
  city?: { names?: { en?: string } };
}

const UNKNOWN: Location = { country: "", region: "", city: "" };

/**
 * Codes that name no country: unknown, and Tor, which DB-IP files as a place.
 * Plausible records all three as no country at all.
 */
const NOT_COUNTRIES = new Set(["ZZ", "XX", "T1"]);

const FILE = "dbip-city-lite.mmdb";

/** How long to wait between checks for a newer database. */
const CHECK_EVERY = 24 * 60 * 60 * 1000;

/**
 * Turns an address into a country, region and city, with DB-IP's free City
 * Lite database.
 *
 * DB-IP publishes a new one at the start of each month under CC BY 4.0, which
 * asks for attribution — the dashboard gives it wherever locations are shown.
 * It is downloaded into `DATA_DIR/geo` the first time any site has analytics
 * on, and replaced once a month; until the first download lands, visits are
 * counted without a location rather than waited on.
 *
 * The file is a bit over a hundred megabytes and a lookup reads a few
 * kilobytes of it, so it is read a page at a time (`mmdb.ts`) rather than held
 * in memory. The address itself is used for the lookup and nothing else.
 */
export class Geo {
  private reader: MmdbFile | undefined;
  private checking: Promise<void> | undefined;
  private lastCheck = 0;
  private timer: ReturnType<typeof setInterval> | undefined;

  constructor(
    private readonly dir: string,
    private readonly log: Logger,
  ) {}

  /** Load whatever database is already on disk. */
  async load(): Promise<void> {
    try {
      const reader = new MmdbFile(join(this.dir, FILE));
      // An open descriptor would go on reading the file a download just renamed over.
      this.reader?.close();
      this.reader = reader;
      this.log.info("geo_loaded", { built: reader.metadata.buildEpoch.toISOString() });
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") {
        this.log.warn("geo_load_failed", errFields(error));
      }
    }
  }

  /**
   * Keep the database current, from now on. Idempotent, so it can be called
   * whenever a site turns analytics on.
   */
  keepUpdated(): void {
    if (this.timer) return;

    void this.update();
    this.timer = setInterval(() => void this.update(), CHECK_EVERY);
    this.timer.unref?.();
  }

  stop(): void {
    clearInterval(this.timer);
    this.timer = undefined;
    this.reader?.close();
    this.reader = undefined;
  }

  lookup(ip: string): Location {
    if (!this.reader || !isIP(ip)) return UNKNOWN;

    let record: CityRecord | null;
    try {
      record = this.reader.get(ip) as CityRecord | null;
    } catch {
      return UNKNOWN;
    }

    const country = record?.country?.iso_code ?? "";
    if (!record || NOT_COUNTRIES.has(country)) return UNKNOWN;

    return {
      country,
      region: record.subdivisions?.[0]?.names?.en ?? "",
      city: record.city?.names?.en ?? "",
    };
  }

  /** Download this month's database if the one on disk is older. */
  private async update(): Promise<void> {
    if (this.checking || Date.now() - this.lastCheck < CHECK_EVERY / 2) return;
    this.lastCheck = Date.now();

    this.checking = this.download().finally(() => {
      this.checking = undefined;
    });
    await this.checking;
  }

  private async download(): Promise<void> {
    const path = join(this.dir, FILE);
    const month = new Date().toISOString().slice(0, 7);
    // Judged by when DB-IP built it rather than when it was downloaded, so a
    // download of last month's early in this one still gets replaced.
    const have = this.reader?.metadata.buildEpoch.toISOString().slice(0, 7);

    await mkdir(this.dir, { recursive: true });
    const tmp = join(this.dir, `${FILE}.tmp`);

    // This month's may not be published yet in its first days; last month's is
    // then the newest there is, and still better than none.
    for (const candidate of [month, previousMonth(month)]) {
      if (have && have >= candidate) return;

      const url = `https://download.db-ip.com/free/dbip-city-lite-${candidate}.mmdb.gz`;
      try {
        const response = await fetch(url, { signal: AbortSignal.timeout(10 * 60 * 1000) });
        if (response.status === 404) continue;
        if (!response.ok || !response.body) throw new Error(`HTTP ${response.status}`);

        await pipeline(
          Readable.fromWeb(response.body as import("node:stream/web").ReadableStream),
          createGunzip(),
          createWriteStream(tmp),
        );
        await rename(tmp, path);

        this.log.info("geo_downloaded", { month: candidate });
        await this.load();
        return;
      } catch (error) {
        await rm(tmp, { force: true });
        this.log.warn("geo_download_failed", { url, ...errFields(error) });
        return;
      }
    }
  }
}

function previousMonth(month: string): string {
  const [year, number] = month.split("-").map(Number);
  const date = new Date(Date.UTC(year, number - 2, 1));
  return date.toISOString().slice(0, 7);
}
