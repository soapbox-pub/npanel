import DeviceDetector from "node-device-detector";

/** What a visit is recorded as coming from. Empty strings mean unknown. */
export interface Device {
  browser: string;
  browserVersion: string;
  os: string;
  osVersion: string;
  /** `Desktop`, `Mobile` or `Tablet`, from the device type, or empty. */
  device: string;
}

/**
 * User agents parsed and kept. Most traffic is a handful of browsers, so a
 * small cache answers nearly every event, and parsing is what costs.
 */
const MAX_CACHED = 10_000;

/** Longer user agents are cut here before parsing. None is legitimately this long. */
const MAX_USER_AGENT = 1024;

/** Device types Plausible reports as each screen size. */
const MOBILE = new Set(["smartphone", "feature phone", "portable media player", "phablet", "wearable", "camera"]);
const TABLET = new Set(["car browser", "tablet"]);
const DESKTOP = new Set(["tv", "console", "desktop"]);

/**
 * Browser names Plausible folds together, so a site's mobile and desktop
 * visitors on the same browser are one row rather than two.
 */
const BROWSER_ALIASES: Record<string, string> = {
  "Mobile Safari": "Safari",
  "Chrome Mobile": "Chrome",
  "Chrome Mobile iOS": "Chrome",
  "Firefox Mobile": "Firefox",
  "Firefox Mobile iOS": "Firefox",
  "Opera Mobile": "Opera",
  "Opera Mini": "Opera",
  "Opera Mini iOS": "Opera",
  "Yandex Browser Lite": "Yandex Browser",
  "Chrome Webview": "Mobile App",
};

/**
 * Reads browsers, operating systems and device types out of user agents, the
 * way Plausible does.
 *
 * `node-device-detector` is a port of Matomo's Device Detector, the same
 * database Plausible's UAInspector reads, so browsers and systems come out
 * under the names a Plausible dashboard would show. Plausible's own
 * adjustments are applied on top: mobile variants folded into their browser,
 * versions cut to major.minor, and the device type reduced to three screen
 * sizes.
 */
export class UserAgents {
  private detector: DeviceDetector | undefined;
  private cache = new Map<string, Device | null>();

  /** What `userAgent` is, or null for a bot, which is never counted. */
  parse(userAgent: string): Device | null {
    const ua = userAgent.slice(0, MAX_USER_AGENT);

    const cached = this.cache.get(ua);
    if (cached !== undefined) {
      // Re-insert on a hit, so eviction below drops the least recently used.
      this.cache.delete(ua);
      this.cache.set(ua, cached);
      return cached;
    }

    const result = this.detect(ua);

    if (this.cache.size >= MAX_CACHED) {
      const oldest = this.cache.keys().next().value;
      if (oldest !== undefined) this.cache.delete(oldest);
    }
    this.cache.set(ua, result);

    return result;
  }

  private detect(ua: string): Device | null {
    // Built on first use: it compiles a few thousand patterns, and a gateway
    // with every site's analytics off should never pay for that.
    this.detector ??= new DeviceDetector({ clientIndexes: true, deviceIndexes: true, deviceAliasCode: false });

    if (this.detector.parseBot(ua)?.name) return null;

    const { client, os, device } = this.detector.detect(ua);
    // Headless Chrome announces itself, and is only ever automation.
    if (client.name === "Headless Chrome") return null;

    const mobileApp = client.type === "mobile app";

    return {
      browser: mobileApp ? "Mobile App" : (BROWSER_ALIASES[client.name] ?? client.name ?? ""),
      browserVersion: mobileApp ? "" : majorMinor(client.version),
      os: os.name ?? "",
      osVersion: majorMinor(os.version),
      device: screenSize(device.type),
    };
  }
}

function screenSize(type: string): string {
  if (MOBILE.has(type)) return "Mobile";
  if (TABLET.has(type)) return "Tablet";
  if (DESKTOP.has(type)) return "Desktop";
  return "";
}

function majorMinor(version: string | undefined): string {
  return version ? version.split(".").slice(0, 2).join(".") : "";
}
