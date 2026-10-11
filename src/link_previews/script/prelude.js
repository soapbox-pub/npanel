// The web a preview script runs in: as much of what a browser gives a script
// drawing on an OffscreenCanvas as a preview needs, and nothing more.
//
// A module whose default export npanel calls once per run, before the site's
// own modules load, with the natives npanel provides, the seed `Math.random`
// starts from, and the time `Date` stands still at. It returns the hooks npanel
// calls the handler with.
//
// Anything a script calls that isn't here throws, rather than drawing a
// picture wrong.
export default function (native, seed, now) {
  const define = (name, value) => Object.defineProperty(globalThis, name, { value, writable: true, configurable: true, enumerable: false });
  const unsupported = (what) => {
    throw new TypeError(`${what} isn't supported by npanel's preview runtime.`);
  };

  // --- Determinism: the same events give the same preview. ----------------

  let state = seed >>> 0 || 1;
  Math.random = () => {
    state ^= state << 13;
    state >>>= 0;
    state ^= state >>> 17;
    state ^= state << 5;
    state >>>= 0;
    return state / 4294967296;
  };
  const RealDate = Date;
  class FrozenDate extends RealDate {
    constructor(...args) {
      super(...(args.length ? args : [now]));
    }
    static now() {
      return now;
    }
  }
  define("Date", FrozenDate);

  // --- Timers. --------------------------------------------------------------

  let nextTimer = 1;
  const timers = new Set();
  define("setTimeout", (fn, ms = 0, ...args) => {
    const id = nextTimer++;
    timers.add(id);
    native.sleep(Math.max(0, Number(ms) || 0)).then(() => {
      if (timers.delete(id)) fn(...args);
    });
    return id;
  });
  define("clearTimeout", (id) => void timers.delete(id));
  if (typeof globalThis.queueMicrotask !== "function") define("queueMicrotask", (fn) => void Promise.resolve().then(fn));

  // --- Logging, into npanel's log. -------------------------------------------

  const show = (value) => {
    if (typeof value === "string") return value;
    if (value instanceof Error) return `${value.name}: ${value.message}`;
    try {
      return JSON.stringify(value) ?? String(value);
    } catch {
      return String(value);
    }
  };
  const log = (level) => (...args) => native.log(level, args.map(show).join(" "));
  define("console", Object.freeze({ log: log("info"), info: log("info"), debug: log("debug"), warn: log("warn"), error: log("error") }));

  // --- Encoding. --------------------------------------------------------------

  const bytesOf = (value) => {
    if (value instanceof Uint8Array) return value;
    if (value instanceof ArrayBuffer) return new Uint8Array(value);
    if (ArrayBuffer.isView(value)) return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    return null;
  };
  // Natives take a Uint8Array that is the whole of its buffer.
  const plain = (bytes) => (bytes.byteOffset === 0 && bytes.byteLength === bytes.buffer.byteLength && bytes.constructor === Uint8Array ? bytes : new Uint8Array(bytes));

  class TextEncoder {
    get encoding() {
      return "utf-8";
    }
    encode(text = "") {
      return native.utf8Encode(String(text));
    }
  }
  class TextDecoder {
    get encoding() {
      return "utf-8";
    }
    decode(input) {
      if (input === undefined) return "";
      const bytes = bytesOf(input);
      if (!bytes) throw new TypeError("TextDecoder.decode takes an ArrayBuffer or a view of one.");
      return native.utf8Decode(plain(bytes));
    }
  }
  define("TextEncoder", TextEncoder);
  define("TextDecoder", TextDecoder);
  define("btoa", (text) => native.btoa(String(text)));
  define("atob", (text) => native.atob(String(text)));

  // --- URL. ------------------------------------------------------------------

  // application/x-www-form-urlencoded, as a browser reads and writes it: a
  // stray `%` is kept rather than thrown on.
  const isHex = (b) => (b >= 48 && b <= 57) || (b >= 65 && b <= 70) || (b >= 97 && b <= 102);
  const formDecode = (text) => {
    const bytes = native.utf8Encode(text.replace(/\+/g, " "));
    const out = new Uint8Array(bytes.length);
    let n = 0;
    for (let i = 0; i < bytes.length; i++) {
      if (bytes[i] === 37 && isHex(bytes[i + 1]) && isHex(bytes[i + 2])) {
        out[n++] = parseInt(String.fromCharCode(bytes[i + 1], bytes[i + 2]), 16);
        i += 2;
      } else {
        out[n++] = bytes[i];
      }
    }
    return native.utf8Decode(out.slice(0, n));
  };
  const formParse = (query) => {
    const pairs = [];
    for (const part of query.replace(/^\?/, "").split("&")) {
      if (!part) continue;
      const at = part.indexOf("=");
      pairs.push(at < 0 ? [formDecode(part), ""] : [formDecode(part.slice(0, at)), formDecode(part.slice(at + 1))]);
    }
    return pairs;
  };
  const formEncode = (text) =>
    encodeURIComponent(text)
      .replace(/[!'()~]/g, (c) => `%${c.charCodeAt(0).toString(16).toUpperCase()}`)
      .replace(/%20/g, "+");

  // How a URL and its searchParams keep each other current, out of a
  // script's reach.
  let attachParams, writeQuery;

  class URLSearchParams {
    #pairs = [];
    #url = null;
    static {
      attachParams = (params, url, query) => {
        params.#url = url;
        params.#pairs = formParse(query);
      };
    }
    constructor(init = "") {
      if (typeof init === "string") {
        this.#pairs = formParse(init);
      } else if (init && typeof init[Symbol.iterator] === "function") {
        for (const [k, v] of init) this.#pairs.push([String(k), String(v)]);
      } else if (init && typeof init === "object") {
        for (const k of Object.keys(init)) this.#pairs.push([k, String(init[k])]);
      }
    }
    #changed() {
      if (this.#url) writeQuery(this.#url, this.toString());
    }
    get size() {
      return this.#pairs.length;
    }
    get(name) {
      return this.#pairs.find(([k]) => k === String(name))?.[1] ?? null;
    }
    getAll(name) {
      return this.#pairs.filter(([k]) => k === String(name)).map(([, v]) => v);
    }
    has(name, value) {
      return this.#pairs.some(([k, v]) => k === String(name) && (value === undefined || v === String(value)));
    }
    append(name, value) {
      this.#pairs.push([String(name), String(value)]);
      this.#changed();
    }
    set(name, value) {
      [name, value] = [String(name), String(value)];
      const at = this.#pairs.findIndex(([k]) => k === name);
      if (at < 0) {
        this.#pairs.push([name, value]);
      } else {
        this.#pairs[at] = [name, value];
        this.#pairs = this.#pairs.filter(([k], i) => i <= at || k !== name);
      }
      this.#changed();
    }
    delete(name, value) {
      this.#pairs = this.#pairs.filter(([k, v]) => !(k === String(name) && (value === undefined || v === String(value))));
      this.#changed();
    }
    sort() {
      // By UTF-16 code units, keeping pairs with the same name in order.
      this.#pairs.sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
      this.#changed();
    }
    forEach(fn, thisArg) {
      for (const [k, v] of this.#pairs) fn.call(thisArg, v, k, this);
    }
    *entries() {
      yield* this.#pairs.map((pair) => [...pair]);
    }
    *keys() {
      for (const [k] of this.#pairs) yield k;
    }
    *values() {
      for (const [, v] of this.#pairs) yield v;
    }
    [Symbol.iterator]() {
      return this.entries();
    }
    toString() {
      return this.#pairs.map(([k, v]) => `${formEncode(k)}=${formEncode(v)}`).join("&");
    }
  }

  class URL {
    #parts;
    #params = new URLSearchParams();
    static {
      writeQuery = (url, query) => {
        url.#parts = native.urlSet(url.href, "search", query);
      };
    }
    constructor(input, base) {
      this.#parts = base === undefined ? native.urlParse(String(input)) : native.urlParse(String(input), String(base));
      attachParams(this.#params, this, this.#parts.search);
    }
    static canParse(input, base) {
      try {
        new URL(input, base);
        return true;
      } catch {
        return false;
      }
    }
    // Setting a part takes the URL a browser would make of it: only a bad
    // `href` throws, and anything else that doesn't fit is ignored.
    #set(name, value) {
      this.#parts = native.urlSet(this.href, name, String(value));
      if (name === "href" || name === "search") attachParams(this.#params, this, this.#parts.search);
    }
    get href() {
      return this.#parts.href;
    }
    set href(value) {
      this.#set("href", value);
    }
    get origin() {
      return this.#parts.origin;
    }
    get protocol() {
      return this.#parts.protocol;
    }
    set protocol(value) {
      this.#set("protocol", value);
    }
    get username() {
      return this.#parts.username;
    }
    set username(value) {
      this.#set("username", value);
    }
    get password() {
      return this.#parts.password;
    }
    set password(value) {
      this.#set("password", value);
    }
    get host() {
      return this.#parts.host;
    }
    set host(value) {
      this.#set("host", value);
    }
    get hostname() {
      return this.#parts.hostname;
    }
    set hostname(value) {
      this.#set("hostname", value);
    }
    get port() {
      return this.#parts.port;
    }
    set port(value) {
      this.#set("port", value);
    }
    get pathname() {
      return this.#parts.pathname;
    }
    set pathname(value) {
      this.#set("pathname", value);
    }
    get search() {
      return this.#parts.search;
    }
    set search(value) {
      this.#set("search", value);
    }
    get hash() {
      return this.#parts.hash;
    }
    set hash(value) {
      this.#set("hash", value);
    }
    get searchParams() {
      return this.#params;
    }
    toString() {
      return this.href;
    }
    toJSON() {
      return this.href;
    }
  }
  define("URL", URL);
  define("URLSearchParams", URLSearchParams);

  // --- Abort signals. --------------------------------------------------------

  class AbortSignal {
    #aborted = false;
    #reason = undefined;
    #listeners = new Set();
    onabort = null;
    get aborted() {
      return this.#aborted;
    }
    get reason() {
      return this.#reason;
    }
    throwIfAborted() {
      if (this.#aborted) throw this.#reason;
    }
    addEventListener(type, fn) {
      if (type === "abort") this.#listeners.add(fn);
    }
    removeEventListener(type, fn) {
      if (type === "abort") this.#listeners.delete(fn);
    }
    static abort(reason) {
      const controller = new AbortController();
      controller.abort(reason);
      return controller.signal;
    }
    static timeout(ms) {
      const controller = new AbortController();
      setTimeout(() => controller.abort(new DOMException("The operation timed out.", "TimeoutError")), ms);
      return controller.signal;
    }
    static any(signals) {
      const controller = new AbortController();
      for (const signal of signals) {
        if (signal.aborted) {
          controller.abort(signal.reason);
          break;
        }
        signal.addEventListener("abort", () => controller.abort(signal.reason));
      }
      return controller.signal;
    }
    static _abort(signal, reason) {
      if (signal.#aborted) return;
      signal.#aborted = true;
      signal.#reason = reason ?? new DOMException("The operation was aborted.", "AbortError");
      const event = { type: "abort", target: signal };
      signal.onabort?.(event);
      for (const fn of signal.#listeners) fn(event);
    }
  }
  const abort = AbortSignal._abort;
  delete AbortSignal._abort;
  class AbortController {
    #signal = new AbortSignal();
    get signal() {
      return this.#signal;
    }
    abort(reason) {
      abort(this.#signal, reason);
    }
  }
  class DOMException extends Error {
    constructor(message = "", name = "Error") {
      super(message);
      this.name = name;
    }
  }
  define("AbortSignal", AbortSignal);
  define("AbortController", AbortController);
  if (typeof globalThis.DOMException !== "function") define("DOMException", DOMException);

  // --- Blob, Headers, Request, Response, fetch. ------------------------------

  const BYTES = Symbol("bytes");
  class Blob {
    constructor(parts = [], options = {}) {
      const chunks = [];
      for (const part of parts) {
        if (part instanceof Blob) chunks.push(part[BYTES]);
        else chunks.push(bytesOf(part) ?? native.utf8Encode(String(part)));
      }
      const size = chunks.reduce((n, c) => n + c.byteLength, 0);
      const bytes = new Uint8Array(size);
      let at = 0;
      for (const chunk of chunks) {
        bytes.set(chunk, at);
        at += chunk.byteLength;
      }
      Object.defineProperty(this, BYTES, { value: bytes });
      this.type = String(options.type ?? "").toLowerCase();
    }
    get size() {
      return this[BYTES].byteLength;
    }
    async arrayBuffer() {
      return this[BYTES].slice().buffer;
    }
    async bytes() {
      return this[BYTES].slice();
    }
    async text() {
      return native.utf8Decode(plain(this[BYTES]));
    }
    slice(start = 0, end = this.size, type = "") {
      return new Blob([this[BYTES].slice(start, end)], { type });
    }
  }
  define("Blob", Blob);

  class Headers {
    #map = new Map();
    constructor(init) {
      if (init instanceof Headers) init = [...init];
      if (init && typeof init[Symbol.iterator] === "function") for (const [k, v] of init) this.append(k, v);
      else if (init && typeof init === "object") for (const k of Object.keys(init)) this.append(k, init[k]);
    }
    append(name, value) {
      const key = String(name).toLowerCase();
      const old = this.#map.get(key);
      this.#map.set(key, old === undefined ? String(value) : `${old}, ${value}`);
    }
    set(name, value) {
      this.#map.set(String(name).toLowerCase(), String(value));
    }
    delete(name) {
      this.#map.delete(String(name).toLowerCase());
    }
    get(name) {
      return this.#map.get(String(name).toLowerCase()) ?? null;
    }
    has(name) {
      return this.#map.has(String(name).toLowerCase());
    }
    forEach(fn, thisArg) {
      for (const [k, v] of this.entries()) fn.call(thisArg, v, k, this);
    }
    *entries() {
      yield* [...this.#map].sort(([a], [b]) => (a < b ? -1 : 1));
    }
    *keys() {
      for (const [k] of this.entries()) yield k;
    }
    *values() {
      for (const [, v] of this.entries()) yield v;
    }
    [Symbol.iterator]() {
      return this.entries();
    }
  }
  define("Headers", Headers);

  // The handler's request headers: whichever it reads, the preview is cached
  // by. Reading them all is reading every one. A request copied from it —
  // `clone()`, `new Request(request)` — copies them without reading them,
  // and what the copy reads counts the same.
  const sealed = new WeakSet();
  const COPY = Symbol("copy");
  class ReadHeaders extends Headers {
    #read;
    constructor(init, read) {
      super(init);
      this.#read = read;
      sealed.add(this);
    }
    get(name) {
      this.#read.add(String(name).toLowerCase());
      return super.get(name);
    }
    has(name) {
      this.#read.add(String(name).toLowerCase());
      return super.has(name);
    }
    *entries() {
      for (const name of super.keys()) this.#read.add(name);
      yield* super.entries();
    }
    append(name, value) {
      if (sealed.has(this)) unsupported("Changing the request's headers");
      super.append(name, value);
    }
    set() {
      unsupported("Changing the request's headers");
    }
    delete() {
      unsupported("Changing the request's headers");
    }
    [COPY]() {
      return new ReadHeaders([...super.entries()], this.#read);
    }
  }

  const body = async (source) => {
    if (source == null) return new Uint8Array(0);
    if (source instanceof Blob) return source[BYTES].slice();
    return bytesOf(source)?.slice() ?? native.utf8Encode(String(source));
  };
  class Response {
    #bytes;
    #used = false;
    constructor(source = null, init = {}) {
      this.status = init.status ?? 200;
      this.statusText = init.statusText ?? "";
      this.headers = new Headers(init.headers);
      this.url = init.url ?? "";
      this.redirected = false;
      this.#bytes = body(source);
      if (source instanceof Blob && source.type && !this.headers.has("content-type")) this.headers.set("content-type", source.type);
    }
    get ok() {
      return this.status >= 200 && this.status < 300;
    }
    get bodyUsed() {
      return this.#used;
    }
    async bytes() {
      if (this.#used) throw new TypeError("The body has already been read.");
      this.#used = true;
      return await this.#bytes;
    }
    async arrayBuffer() {
      return (await this.bytes()).buffer;
    }
    async text() {
      return native.utf8Decode(plain(await this.bytes()));
    }
    async json() {
      return JSON.parse(await this.text());
    }
    async blob() {
      return new Blob([await this.bytes()], { type: this.headers.get("content-type") ?? "" });
    }
  }
  define("Response", Response);

  // A request as a preview has them: a GET, so without a body, as a
  // browser's GET is.
  class Request {
    constructor(input, init = {}) {
      const from = input instanceof Request ? input : null;
      this.url = from ? from.url : new URL(String(input)).href;
      this.method = String(init.method ?? from?.method ?? "GET").toUpperCase();
      if (init.body != null && (this.method === "GET" || this.method === "HEAD")) throw new TypeError("A GET or HEAD request can't have a body.");
      if (init.body != null) unsupported("A request body");
      if (init.headers instanceof Headers) this.headers = init.headers;
      else if (init.headers === undefined && sealed.has(from?.headers)) this.headers = from.headers[COPY]();
      else this.headers = new Headers(init.headers ?? from?.headers);
      this.signal = init.signal ?? from?.signal ?? new AbortController().signal;
    }
    get body() {
      return null;
    }
    get bodyUsed() {
      return false;
    }
    async bytes() {
      return new Uint8Array(0);
    }
    async arrayBuffer() {
      return new ArrayBuffer(0);
    }
    async text() {
      return "";
    }
    async json() {
      return JSON.parse("");
    }
    async blob() {
      return new Blob([]);
    }
    clone() {
      return new Request(this);
    }
  }
  define("Request", Request);

  define("fetch", async (input, init = {}) => {
    const request = input instanceof Request ? input : null;
    const url = request ? request.url : String(input);
    const method = String(init.method ?? request?.method ?? "GET").toUpperCase();
    if (method !== "GET") throw new TypeError("A preview can only fetch with GET.");
    (init.signal ?? request?.signal)?.throwIfAborted();
    const { status, url: final, headers, body } = await native.fetch(url);
    return new Response(body, { status, headers, url: final });
  });

  // --- Images and canvases. ----------------------------------------------------

  const ID = Symbol("surface");
  const RESET = Symbol("reset");

  class ImageData {
    constructor(a, b, c) {
      if (typeof a === "number") {
        this.width = a;
        this.height = b;
        this.data = new Uint8ClampedArray(a * b * 4);
      } else {
        this.data = a;
        this.width = b;
        this.height = c ?? a.length / 4 / b;
        if (this.data.length !== this.width * this.height * 4) throw new RangeError("The data doesn't match the width and height.");
      }
      this.colorSpace = "srgb";
    }
  }
  define("ImageData", ImageData);

  class ImageBitmap {
    constructor(id) {
      Object.defineProperty(this, ID, { value: id, writable: true });
      const [width, height] = native.surfaceSize(id);
      this.width = width;
      this.height = height;
    }
    close() {
      if (this[ID] === null) return;
      native.surfaceFree(this[ID]);
      this[ID] = null;
      this.width = 0;
      this.height = 0;
    }
  }
  define("ImageBitmap", ImageBitmap);

  const surfaceOf = (source) => {
    if (!(source instanceof ImageBitmap || source instanceof OffscreenCanvas) || source[ID] === null) {
      throw new TypeError("Only an ImageBitmap or an OffscreenCanvas can be drawn.");
    }
    return source[ID];
  };

  define("createImageBitmap", async (source, ...crop) => {
    if (crop.length) unsupported("Cropping in createImageBitmap");
    if (source instanceof Blob) return new ImageBitmap(native.decode(plain(source[BYTES])));
    if (source instanceof ImageData) {
      const id = native.canvasNew(source.width, source.height);
      native.putImageData(id, plain(new Uint8Array(source.data.buffer, source.data.byteOffset, source.data.byteLength)), 0, 0, source.width, source.height);
      return new ImageBitmap(id);
    }
    return new ImageBitmap(native.surfaceCopy(surfaceOf(source)));
  });

  class CanvasGradient {
    #style;
    constructor(kind, points) {
      this.#style = { kind, points, stops: [] };
    }
    addColorStop(offset, color) {
      if (!(offset >= 0 && offset <= 1)) throw new RangeError("A colour stop's offset must be between 0 and 1.");
      if (!native.isColor(String(color))) throw new SyntaxError(`${color} isn't a colour.`);
      this.#style.stops.push([Number(offset), String(color)]);
      this.#style.stops.sort((a, b) => a[0] - b[0]);
    }
    static _style(gradient, matrix) {
      const { kind, points, stops } = gradient.#style;
      return { [kind]: { points, stops, matrix } };
    }
  }
  const gradientStyle = CanvasGradient._style;
  delete CanvasGradient._style;
  define("CanvasGradient", CanvasGradient);

  // [a, b, c, d, e, f], as DOMMatrix names them.
  const multiply = (m, n) => [
    m[0] * n[0] + m[2] * n[1],
    m[1] * n[0] + m[3] * n[1],
    m[0] * n[2] + m[2] * n[3],
    m[1] * n[2] + m[3] * n[3],
    m[0] * n[4] + m[2] * n[5] + m[4],
    m[1] * n[4] + m[3] * n[5] + m[5],
  ];
  const apply = (m, x, y) => [m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]];
  const IDENTITY = [1, 0, 0, 1, 0, 0];
  const finite = (...values) => values.every(Number.isFinite);

  // A path's commands, its points transformed as they're added: 0 move,
  // 1 line, 2 cubic, 3 quad, 4 close.
  class Recorder {
    constructor() {
      this.commands = [];
      this.current = null;
      this.start = null;
    }
    move(m, x, y) {
      if (!finite(x, y)) return;
      const [px, py] = apply(m, x, y);
      this.commands.push(0, px, py);
      this.current = this.start = [x, y];
    }
    line(m, x, y) {
      if (!finite(x, y)) return;
      if (!this.current) return this.move(m, x, y);
      this.commands.push(1, ...apply(m, x, y));
      this.current = [x, y];
    }
    cubic(m, x1, y1, x2, y2, x, y) {
      if (!finite(x1, y1, x2, y2, x, y)) return;
      if (!this.current) this.move(m, x1, y1);
      this.commands.push(2, ...apply(m, x1, y1), ...apply(m, x2, y2), ...apply(m, x, y));
      this.current = [x, y];
    }
    quad(m, x1, y1, x, y) {
      if (!finite(x1, y1, x, y)) return;
      if (!this.current) this.move(m, x1, y1);
      this.commands.push(3, ...apply(m, x1, y1), ...apply(m, x, y));
      this.current = [x, y];
    }
    close() {
      if (!this.current) return;
      this.commands.push(4);
      this.current = this.start;
    }
    // An ellipse's arc as cubic curves, a quarter turn at most each.
    ellipse(m, x, y, rx, ry, rotation, start, end, ccw) {
      if (!finite(x, y, rx, ry, rotation, start, end)) return;
      if (rx < 0 || ry < 0) throw new RangeError("A radius can't be negative.");
      const tau = Math.PI * 2;
      let sweep;
      if (!ccw) sweep = end - start >= tau ? tau : (((end - start) % tau) + tau) % tau;
      else sweep = start - end >= tau ? -tau : -((((start - end) % tau) + tau) % tau);
      const cos = Math.cos(rotation);
      const sin = Math.sin(rotation);
      const at = (t) => {
        const px = rx * Math.cos(t);
        const py = ry * Math.sin(t);
        return [x + px * cos - py * sin, y + px * sin + py * cos];
      };
      const tangent = (t) => {
        const dx = -rx * Math.sin(t);
        const dy = ry * Math.cos(t);
        return [dx * cos - dy * sin, dx * sin + dy * cos];
      };
      const [sx, sy] = at(start);
      if (this.current) this.line(m, sx, sy);
      else this.move(m, sx, sy);
      const segments = Math.max(1, Math.ceil(Math.abs(sweep) / (Math.PI / 2) - 1e-9));
      const step = sweep / segments;
      const k = (4 / 3) * Math.tan(step / 4);
      let t = start;
      for (let i = 0; i < segments; i++) {
        const t2 = t + step;
        const [x0, y0] = at(t);
        const [x3, y3] = at(t2);
        const [dx0, dy0] = tangent(t);
        const [dx3, dy3] = tangent(t2);
        this.cubic(m, x0 + k * dx0, y0 + k * dy0, x3 - k * dx3, y3 - k * dy3, x3, y3);
        t = t2;
      }
    }
    arcTo(m, x1, y1, x2, y2, r) {
      if (!finite(x1, y1, x2, y2, r)) return;
      if (r < 0) throw new RangeError("A radius can't be negative.");
      if (!this.current) this.move(m, x1, y1);
      const [x0, y0] = this.current;
      const [ax, ay, bx, by] = [x0 - x1, y0 - y1, x2 - x1, y2 - y1];
      const la = Math.hypot(ax, ay);
      const lb = Math.hypot(bx, by);
      const cross = ax * by - ay * bx;
      if (r === 0 || la === 0 || lb === 0 || Math.abs(cross) < 1e-9) return this.line(m, x1, y1);
      const angle = Math.acos(Math.max(-1, Math.min(1, (ax * bx + ay * by) / (la * lb))));
      const d = r / Math.tan(angle / 2);
      const [tx0, ty0] = [x1 + (ax / la) * d, y1 + (ay / la) * d];
      const [tx1, ty1] = [x1 + (bx / lb) * d, y1 + (by / lb) * d];
      const side = cross > 0 ? 1 : -1;
      const [cx, cy] = [tx0 - (side * -ay / la) * r, ty0 - (side * ax / la) * r];
      const a0 = Math.atan2(ty0 - cy, tx0 - cx);
      const a1 = Math.atan2(ty1 - cy, tx1 - cx);
      this.line(m, tx0, ty0);
      this.ellipse(m, cx, cy, r, r, 0, a0, a1, cross > 0);
    }
    rect(m, x, y, w, h) {
      if (!finite(x, y, w, h)) return;
      this.move(m, x, y);
      this.line(m, x + w, y);
      this.line(m, x + w, y + h);
      this.line(m, x, y + h);
      this.close();
      this.move(m, x, y);
    }
    roundRect(m, x, y, w, h, radii = 0) {
      if (!finite(x, y, w, h)) return;
      const list = (Array.isArray(radii) ? radii : [radii]).map((r) => (typeof r === "object" ? r.x : Number(r)));
      if (list.length < 1 || list.length > 4 || list.some((r) => !(r >= 0))) throw new RangeError("roundRect takes one to four radii, none negative.");
      const [tl, tr, br, bl] = [
        list[0],
        list[list.length > 1 ? 1 : 0],
        list[list.length > 2 ? 2 : 0],
        list[list.length === 4 ? 3 : list.length > 1 ? 1 : 0],
      ];
      const scale = Math.min(1, Math.abs(w) / Math.max(tl + tr, bl + br, 1e-9), Math.abs(h) / Math.max(tl + bl, tr + br, 1e-9));
      const [a, b, c, d] = [tl * scale, tr * scale, br * scale, bl * scale];
      const half = Math.PI / 2;
      this.move(m, x + a, y);
      this.line(m, x + w - b, y);
      if (b) this.ellipse(m, x + w - b, y + b, b, b, 0, -half, 0, false);
      this.line(m, x + w, y + h - c);
      if (c) this.ellipse(m, x + w - c, y + h - c, c, c, 0, 0, half, false);
      this.line(m, x + d, y + h);
      if (d) this.ellipse(m, x + d, y + h - d, d, d, 0, half, Math.PI, false);
      this.line(m, x, y + a);
      if (a) this.ellipse(m, x + a, y + a, a, a, 0, Math.PI, Math.PI + half, false);
      this.close();
      this.move(m, x, y);
    }
  }

  // A Path2D keeps its points as given, and is transformed when it's used.
  const pathOps = new WeakMap();
  const opsOf = (path) => pathOps.get(path);
  const record = (name) =>
    function (...args) {
      opsOf(this).push([name, args]);
    };
  class Path2D {
    constructor(path) {
      if (path instanceof Path2D) pathOps.set(this, [...opsOf(path)]);
      else if (path !== undefined) unsupported("A Path2D from SVG path data");
      else pathOps.set(this, []);
    }
    arc(x, y, r, start, end, ccw = false) {
      opsOf(this).push(["ellipse", [x, y, r, r, 0, start, end, ccw]]);
    }
    addPath(path) {
      opsOf(this).push(...opsOf(path));
    }
  }
  Object.assign(Path2D.prototype, {
    moveTo: record("move"),
    lineTo: record("line"),
    bezierCurveTo: record("cubic"),
    quadraticCurveTo: record("quad"),
    closePath: record("close"),
    ellipse: record("ellipse"),
    arcTo: record("arcTo"),
    rect: record("rect"),
    roundRect: record("roundRect"),
  });
  const pathCommands = (path, m) => {
    const recorder = new Recorder();
    for (const [name, args] of opsOf(path)) recorder[name](m, ...args);
    return recorder.commands;
  };
  define("Path2D", Path2D);

  const parseFont = (font) => {
    const match = /^\s*((?:[\w-]+\s+)*?)(\d*\.?\d+)(px|pt|em|rem)(?:\s*\/\s*\S+)?\s+(.+?)\s*$/i.exec(font);
    if (!match) return null;
    let style = "normal";
    let weight = "normal";
    for (const word of match[1].trim().split(/\s+/).filter(Boolean)) {
      const lower = word.toLowerCase();
      if (lower === "italic" || lower === "oblique") style = lower;
      else if (lower === "bold" || lower === "bolder") weight = "700";
      else if (lower === "lighter") weight = "300";
      else if (/^[1-9]00$/.test(lower)) weight = lower;
    }
    const size = Number(match[2]) * { px: 1, pt: 4 / 3, em: 16, rem: 16 }[match[3].toLowerCase()];
    return { family: match[4], size, weight, style };
  };

  const OPS = new Set([
    "source-over", "source-in", "source-out", "source-atop", "destination-over", "destination-in", "destination-out", "destination-atop",
    "lighter", "copy", "xor", "multiply", "screen", "overlay", "darken", "lighten", "color-dodge", "color-burn", "hard-light", "soft-light",
    "difference", "exclusion", "hue", "saturation", "color", "luminosity",
  ]);

  const initial = () => ({
    matrix: IDENTITY,
    fillStyle: "#000000",
    strokeStyle: "#000000",
    globalAlpha: 1,
    globalCompositeOperation: "source-over",
    lineWidth: 1,
    lineCap: "butt",
    lineJoin: "miter",
    miterLimit: 10,
    dash: [],
    lineDashOffset: 0,
    font: "10px sans-serif",
    textAlign: "start",
    textBaseline: "alphabetic",
    imageSmoothingEnabled: true,
    imageSmoothingQuality: "low",
    clips: [],
  });

  class OffscreenCanvasRenderingContext2D {
    #canvas;
    #s = initial();
    #stack = [];
    #path = new Recorder();
    constructor(canvas) {
      this.#canvas = canvas;
      Object.preventExtensions(this);
    }
    get canvas() {
      return this.#canvas;
    }
    get #id() {
      return surfaceOf(this.#canvas);
    }
    [RESET]() {
      this.#s = initial();
      this.#stack = [];
      this.#path = new Recorder();
    }

    // State.
    save() {
      this.#stack.push({ ...this.#s, dash: [...this.#s.dash], clips: [...this.#s.clips] });
    }
    restore() {
      if (this.#stack.length) this.#s = this.#stack.pop();
    }
    reset() {
      this[RESET]();
      native.fill(this.#id, [0, 0, 0, 1, 1e7, 0, 1, 1e7, 1e7, 1, 0, 1e7, 4], "nonzero", JSON.stringify({ color: "transparent" }), JSON.stringify({ alpha: 1, op: "copy" }));
    }
    #state() {
      const s = this.#s;
      return JSON.stringify({ alpha: s.globalAlpha, op: s.globalCompositeOperation, clips: s.clips, smoothing: s.imageSmoothingEnabled, quality: s.imageSmoothingQuality });
    }
    #style(style) {
      return JSON.stringify(style instanceof CanvasGradient ? gradientStyle(style, this.#s.matrix) : { color: style });
    }

    get globalAlpha() {
      return this.#s.globalAlpha;
    }
    set globalAlpha(v) {
      if (Number.isFinite(+v) && v >= 0 && v <= 1) this.#s.globalAlpha = +v;
    }
    get globalCompositeOperation() {
      return this.#s.globalCompositeOperation;
    }
    set globalCompositeOperation(v) {
      if (OPS.has(v)) this.#s.globalCompositeOperation = v;
    }
    get fillStyle() {
      return this.#s.fillStyle;
    }
    set fillStyle(v) {
      if (v instanceof CanvasGradient || native.isColor(String(v))) this.#s.fillStyle = v instanceof CanvasGradient ? v : String(v);
      else if (v && typeof v === "object") unsupported("A pattern as a fill");
    }
    get strokeStyle() {
      return this.#s.strokeStyle;
    }
    set strokeStyle(v) {
      if (v instanceof CanvasGradient || native.isColor(String(v))) this.#s.strokeStyle = v instanceof CanvasGradient ? v : String(v);
      else if (v && typeof v === "object") unsupported("A pattern as a stroke");
    }
    get lineWidth() {
      return this.#s.lineWidth;
    }
    set lineWidth(v) {
      if (v > 0 && Number.isFinite(+v)) this.#s.lineWidth = +v;
    }
    get lineCap() {
      return this.#s.lineCap;
    }
    set lineCap(v) {
      if (["butt", "round", "square"].includes(v)) this.#s.lineCap = v;
    }
    get lineJoin() {
      return this.#s.lineJoin;
    }
    set lineJoin(v) {
      if (["miter", "round", "bevel"].includes(v)) this.#s.lineJoin = v;
    }
    get miterLimit() {
      return this.#s.miterLimit;
    }
    set miterLimit(v) {
      if (v > 0 && Number.isFinite(+v)) this.#s.miterLimit = +v;
    }
    get lineDashOffset() {
      return this.#s.lineDashOffset;
    }
    set lineDashOffset(v) {
      if (Number.isFinite(+v)) this.#s.lineDashOffset = +v;
    }
    setLineDash(segments) {
      const list = [...segments].map(Number);
      if (list.some((n) => !(n >= 0) || !Number.isFinite(n))) return;
      this.#s.dash = list.length % 2 ? [...list, ...list] : list;
    }
    getLineDash() {
      return [...this.#s.dash];
    }
    get font() {
      return this.#s.font;
    }
    set font(v) {
      if (parseFont(String(v))) this.#s.font = String(v);
    }
    get textAlign() {
      return this.#s.textAlign;
    }
    set textAlign(v) {
      if (["start", "end", "left", "right", "center"].includes(v)) this.#s.textAlign = v;
    }
    get textBaseline() {
      return this.#s.textBaseline;
    }
    set textBaseline(v) {
      if (["top", "hanging", "middle", "alphabetic", "ideographic", "bottom"].includes(v)) this.#s.textBaseline = v;
    }
    get direction() {
      return "ltr";
    }
    set direction(v) {
      if (v !== "ltr" && v !== "inherit") unsupported("Right-to-left canvas text");
    }
    get imageSmoothingEnabled() {
      return this.#s.imageSmoothingEnabled;
    }
    set imageSmoothingEnabled(v) {
      this.#s.imageSmoothingEnabled = Boolean(v);
    }
    get imageSmoothingQuality() {
      return this.#s.imageSmoothingQuality;
    }
    set imageSmoothingQuality(v) {
      if (["low", "medium", "high"].includes(v)) this.#s.imageSmoothingQuality = v;
    }
    get shadowBlur() {
      return 0;
    }
    set shadowBlur(v) {
      if (+v !== 0) unsupported("A canvas shadow");
    }
    get shadowColor() {
      return "rgba(0, 0, 0, 0)";
    }
    set shadowColor(v) {
      if (v !== "transparent" && v !== "rgba(0, 0, 0, 0)") unsupported("A canvas shadow");
    }
    get shadowOffsetX() {
      return 0;
    }
    set shadowOffsetX(v) {
      if (+v !== 0) unsupported("A canvas shadow");
    }
    get shadowOffsetY() {
      return 0;
    }
    set shadowOffsetY(v) {
      if (+v !== 0) unsupported("A canvas shadow");
    }
    get filter() {
      return "none";
    }
    set filter(v) {
      if (v !== "none") unsupported("A canvas filter");
    }

    // Transforms.
    #multiply(n) {
      if (n.every(Number.isFinite)) this.#s.matrix = multiply(this.#s.matrix, n);
    }
    translate(x, y) {
      this.#multiply([1, 0, 0, 1, x, y]);
    }
    scale(x, y) {
      this.#multiply([x, 0, 0, y, 0, 0]);
    }
    rotate(angle) {
      const [c, s] = [Math.cos(angle), Math.sin(angle)];
      this.#multiply([c, s, -s, c, 0, 0]);
    }
    transform(a, b, c, d, e, f) {
      this.#multiply([a, b, c, d, e, f]);
    }
    setTransform(a, b, c, d, e, f) {
      if (a === undefined) return this.resetTransform();
      const m = typeof a === "object" ? [a.a ?? 1, a.b ?? 0, a.c ?? 0, a.d ?? 1, a.e ?? 0, a.f ?? 0] : [a, b, c, d, e, f];
      if (m.every(Number.isFinite)) this.#s.matrix = m;
    }
    resetTransform() {
      this.#s.matrix = IDENTITY;
    }
    getTransform() {
      const [a, b, c, d, e, f] = this.#s.matrix;
      return { a, b, c, d, e, f, m11: a, m12: b, m21: c, m22: d, m41: e, m42: f, is2D: true };
    }

    // The path.
    beginPath() {
      this.#path = new Recorder();
    }
    moveTo(x, y) {
      this.#path.move(this.#s.matrix, x, y);
    }
    lineTo(x, y) {
      this.#path.line(this.#s.matrix, x, y);
    }
    bezierCurveTo(x1, y1, x2, y2, x, y) {
      this.#path.cubic(this.#s.matrix, x1, y1, x2, y2, x, y);
    }
    quadraticCurveTo(x1, y1, x, y) {
      this.#path.quad(this.#s.matrix, x1, y1, x, y);
    }
    closePath() {
      this.#path.close();
    }
    arc(x, y, r, start, end, ccw = false) {
      this.#path.ellipse(this.#s.matrix, x, y, r, r, 0, start, end, ccw);
    }
    ellipse(x, y, rx, ry, rotation, start, end, ccw = false) {
      this.#path.ellipse(this.#s.matrix, x, y, rx, ry, rotation, start, end, ccw);
    }
    arcTo(x1, y1, x2, y2, r) {
      this.#path.arcTo(this.#s.matrix, x1, y1, x2, y2, r);
    }
    rect(x, y, w, h) {
      this.#path.rect(this.#s.matrix, x, y, w, h);
    }
    roundRect(x, y, w, h, radii) {
      this.#path.roundRect(this.#s.matrix, x, y, w, h, radii);
    }
    #commands(path) {
      return path instanceof Path2D ? pathCommands(path, this.#s.matrix) : this.#path.commands;
    }
    #rule(path, rule) {
      return (path instanceof Path2D ? rule : path) === "evenodd" ? "evenodd" : "nonzero";
    }

    // Drawing.
    fill(path, rule) {
      native.fill(this.#id, this.#commands(path), this.#rule(path, rule), this.#style(this.#s.fillStyle), this.#state());
    }
    stroke(path) {
      const m = this.#s.matrix;
      const scale = Math.sqrt(Math.abs(m[0] * m[3] - m[1] * m[2]));
      const line = {
        width: this.#s.lineWidth * scale,
        cap: this.#s.lineCap,
        join: this.#s.lineJoin,
        miter: this.#s.miterLimit,
        dash: this.#s.dash.map((n) => n * scale),
        offset: this.#s.lineDashOffset * scale,
      };
      native.stroke(this.#id, this.#commands(path), this.#style(this.#s.strokeStyle), JSON.stringify(line), this.#state());
    }
    clip(path, rule) {
      this.#s.clips = [...this.#s.clips, { path: [...this.#commands(path)], rule: this.#rule(path, rule) }];
    }
    fillRect(x, y, w, h) {
      const recorder = new Recorder();
      recorder.rect(this.#s.matrix, x, y, w, h);
      native.fill(this.#id, recorder.commands, "nonzero", this.#style(this.#s.fillStyle), this.#state());
    }
    strokeRect(x, y, w, h) {
      const saved = this.#path;
      this.#path = new Recorder();
      this.#path.rect(this.#s.matrix, x, y, w, h);
      this.stroke();
      this.#path = saved;
    }
    clearRect(x, y, w, h) {
      const recorder = new Recorder();
      recorder.rect(this.#s.matrix, x, y, w, h);
      const state = JSON.stringify({ alpha: 1, op: "destination-out", clips: this.#s.clips });
      native.fill(this.#id, recorder.commands, "nonzero", JSON.stringify({ color: "#000" }), state);
    }
    drawImage(image, ...args) {
      const source = surfaceOf(image);
      const [width, height] = native.surfaceSize(source);
      let rects;
      if (args.length === 2) rects = [0, 0, width, height, args[0], args[1], width, height];
      else if (args.length === 4) rects = [0, 0, width, height, ...args];
      else if (args.length === 8) rects = args;
      else throw new TypeError("drawImage takes 3, 5 or 9 arguments.");
      if (!rects.every(Number.isFinite)) return;
      native.drawImage(this.#id, source, rects, this.#s.matrix, this.#state());
    }
    createImageData(width, height) {
      return typeof width === "object" ? new ImageData(width.width, width.height) : new ImageData(width, height);
    }
    getImageData(x, y, width, height) {
      const bytes = native.getImageData(this.#id, x, y, width, height);
      return new ImageData(new Uint8ClampedArray(bytes.buffer), width, height);
    }
    putImageData(data, x, y, ...dirty) {
      if (dirty.length) unsupported("A dirty rectangle in putImageData");
      native.putImageData(this.#id, plain(new Uint8Array(data.data.buffer, data.data.byteOffset, data.data.byteLength)), x, y, data.width, data.height);
    }
    createLinearGradient(x0, y0, x1, y1) {
      return new CanvasGradient("linear", [x0, y0, x1, y1]);
    }
    createRadialGradient(x0, y0, r0, x1, y1, r1) {
      if (r0 < 0 || r1 < 0) throw new RangeError("A radius can't be negative.");
      return new CanvasGradient("radial", [x0, y0, r0, x1, y1, r1]);
    }
    createConicGradient() {
      unsupported("A conic gradient");
    }
    createPattern() {
      unsupported("A pattern");
    }

    // Text.
    #text(text, x, y, maxWidth, stroke) {
      const font = parseFont(this.#s.font);
      const m = this.#s.matrix;
      const spec = {
        text: String(text),
        x,
        y,
        font,
        align: this.#s.textAlign,
        baseline: this.#s.textBaseline,
        matrix: m,
        max_width: maxWidth ?? null,
        stroke: stroke ? this.#s.lineWidth : null,
      };
      native.drawText(this.#id, JSON.stringify(spec), this.#style(stroke ? this.#s.strokeStyle : this.#s.fillStyle), this.#state());
    }
    fillText(text, x, y, maxWidth) {
      this.#text(text, x, y, maxWidth, false);
    }
    strokeText(text, x, y, maxWidth) {
      this.#text(text, x, y, maxWidth, true);
    }
    measureText(text) {
      const font = parseFont(this.#s.font);
      const [width, left, right, ascent, descent] = native.measureText(String(text), JSON.stringify(font));
      return {
        width,
        actualBoundingBoxLeft: left,
        actualBoundingBoxRight: right,
        actualBoundingBoxAscent: ascent,
        actualBoundingBoxDescent: descent,
        fontBoundingBoxAscent: font.size * 0.8,
        fontBoundingBoxDescent: font.size * 0.2,
      };
    }
  }
  define("OffscreenCanvasRenderingContext2D", OffscreenCanvasRenderingContext2D);

  class OffscreenCanvas {
    #context = null;
    constructor(width, height) {
      Object.defineProperty(this, ID, { value: native.canvasNew(width >>> 0, height >>> 0), writable: true });
    }
    get width() {
      return native.surfaceSize(this[ID])[0];
    }
    set width(v) {
      this.#resize(v >>> 0, this.height);
    }
    get height() {
      return native.surfaceSize(this[ID])[1];
    }
    set height(v) {
      this.#resize(this.width, v >>> 0);
    }
    #resize(width, height) {
      native.surfaceFree(this[ID]);
      this[ID] = native.canvasNew(width, height);
      if (this.#context) this.#context[RESET]();
    }
    getContext(type) {
      if (type !== "2d") return null;
      return (this.#context ??= new OffscreenCanvasRenderingContext2D(this));
    }
    async convertToBlob(options = {}) {
      // The pixels as they are now, encoded once the handler has had the
      // chance to return: encoding is the slowest part of drawing, and a
      // handler returning the promise shouldn't keep the page waiting on it.
      const snapshot = native.surfaceCopy(this[ID]);
      try {
        await native.sleep(0);
        const { bytes, type } = native.encode(snapshot, String(options.type ?? "image/png"), typeof options.quality === "number" ? options.quality : -1);
        return new Blob([bytes], { type });
      } finally {
        native.surfaceFree(snapshot);
      }
    }
    transferToImageBitmap() {
      const bitmap = new ImageBitmap(native.surfaceCopy(this[ID]));
      this.#resize(this.width, this.height);
      return bitmap;
    }
  }
  define("OffscreenCanvas", OffscreenCanvas);

  // --- Nostr, as Nostrify's NRelay. -------------------------------------------

  let subscriptions = 0;
  const nostr = Object.freeze({
    async query(filters, opts = {}) {
      opts.signal?.throwIfAborted();
      if (!Array.isArray(filters)) throw new TypeError("query takes an array of filters.");
      return JSON.parse(await native.query(JSON.stringify(filters)));
    },
    async *req(filters, opts = {}) {
      const id = String(++subscriptions);
      for (const event of await nostr.query(filters, opts)) yield ["EVENT", id, event];
      // Everything stored has been sent, and a preview subscribes to nothing
      // more.
      yield ["EOSE", id];
    },
    async event() {
      throw new Error("npanel doesn't publish events.");
    },
    async close() {},
  });

  // --- The hooks npanel calls. ------------------------------------------------

  const toBytes = async (image) => {
    if (image == null) return null;
    if (image instanceof Blob) return plain(image[BYTES]);
    const bytes = bytesOf(image);
    if (!bytes) throw new TypeError("An image must be a URL, a Uint8Array, an ArrayBuffer or a Blob, or a promise of one.");
    return plain(bytes);
  };
  const text = (value) => (value == null ? null : String(value));

  return Object.freeze({
    async call(handler, url, method, headers, deadline) {
      if (!handler || typeof handler.preview !== "function") {
        throw new TypeError("/.well-known/npanel/preview.js must export default { routes, preview(request, ctx) }.");
      }
      const read = new Set();
      // The request's signal is the run's deadline, as a server's is the
      // client going away.
      const signal = AbortSignal.timeout(deadline);
      const request = new Request(url, { method, headers: new ReadHeaders(headers, read), signal });
      const result = await handler.preview(request, Object.freeze({ nostr, signal }));
      if (result == null) return { none: true, headers: [...read] };
      if (typeof result !== "object") throw new TypeError("preview() must return an object, or null to leave the page as published.");
      const image = result.image;
      const out = { none: false, title: text(result.title), description: text(result.description), body: text(result.body), url: null, bytes: null, pending: null };
      if (typeof image === "string" || image instanceof URL) out.url = String(image);
      else if (image && typeof image.then === "function") out.pending = Promise.resolve(image).then(toBytes);
      else out.bytes = await toBytes(image);
      const published = result.published instanceof Date ? result.published.getTime() / 1000 : result.published;
      out.extras = JSON.stringify({
        type: text(result.type),
        twitter: text(result.twitter),
        video: result.video ?? null,
        audio: result.audio ?? null,
        published: typeof published === "number" ? published : null,
        canonical: result.canonical == null ? null : String(result.canonical),
        jsonLd: result.jsonLd ?? null,
      });
      out.headers = [...read];
      return out;
    },
  });
}
