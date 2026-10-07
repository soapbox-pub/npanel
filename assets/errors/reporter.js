/*
 * npanel's error reporter: what a site with error reporting on loads first
 * thing in every page. It catches what the page throws and doesn't catch, and
 * sends it to the gateway on the site's own origin as a Sentry envelope, with
 * the trail of clicks, navigations, requests and console output that led up
 * to it.
 *
 * Kept small and dependency-free because every page waits for it. Stack
 * traces are parsed here, with the line formats Sentry's browser SDK reads,
 * so the gateway gets structured frames it can map back through the site's
 * own source maps.
 *
 * Called with { endpoint, release } by the line npanel appends.
 */
(function (config) {
  "use strict";

  var MAX_EVENTS = 10;
  var MAX_BREADCRUMBS = 30;
  var MAX_FRAMES = 50;
  var MAX_CAUSES = 5;
  var MAX_STRING = 1000;
  // `keepalive` requests are capped at 64 KiB, so a report has to fit.
  var MAX_BODY = 60000;

  var root = typeof window !== "undefined" ? window : self;
  var send = root.fetch;
  var sent = 0;
  var seen = {};
  var breadcrumbs = [];

  function now() {
    return Date.now() / 1000;
  }

  function truncate(value, max) {
    var text = String(value);
    return text.length > max ? text.slice(0, max) + "\u2026" : text;
  }

  function describe(value) {
    if (typeof value === "string") return value;
    if (value instanceof Error) return value.name + ": " + value.message;
    try {
      var json = JSON.stringify(value);
      return json === undefined ? String(value) : json;
    } catch (e) {
      return String(value);
    }
  }

  function crumb(entry) {
    entry.timestamp = now();
    breadcrumbs.push(entry);
    if (breadcrumbs.length > MAX_BREADCRUMBS) breadcrumbs.shift();
  }

  // V8: `    at name (https://x/a.js:1:2)` or `    at https://x/a.js:1:2`.
  var CHROME = /^\s*at (?:(.+?) \()?((?:[a-z][a-z0-9+.-]*:|\/)[^\s)]*?)(?::(\d+))?(?::(\d+))?\)?\s*$/i;
  // The location inside `at eval (eval at f (https://x/a.js:1:2), <anonymous>:1:1)`.
  var CHROME_EVAL = /^\s*at (?:(.+?) \()?eval at .*?\(((?:[a-z][a-z0-9+.-]*:|\/)[^\s)]*?)(?::(\d+))?(?::(\d+))?\)/i;
  // SpiderMonkey and JavaScriptCore: `name@https://x/a.js:1:2`.
  var GECKO = /^\s*(.*?)@((?:[a-z][a-z0-9+.-]*:)?\/[^\s]*?)(?::(\d+))?(?::(\d+))?\s*$/i;

  function number(value) {
    return value ? parseInt(value, 10) : undefined;
  }

  // One line of a stack trace as a frame, or null for a line that isn't
  // one: the message, `[native code]`, an engine's internals.
  function frame(line) {
    var match = CHROME_EVAL.exec(line) || CHROME.exec(line);
    if (match) {
      var name = match[1] ? match[1].replace(/^async /, "").replace(/ \[as \S+\]$/, "") : undefined;
      return { abs_path: match[2], function: name || undefined, lineno: number(match[3]), colno: number(match[4]) };
    }
    match = GECKO.exec(line);
    if (match) {
      return { abs_path: match[2], function: match[1] || undefined, lineno: number(match[3]), colno: number(match[4]) };
    }
    return null;
  }

  // Oldest call first, as Sentry orders them, and none of the reporter's own.
  function frames(stack) {
    var out = [];
    var lines = String(stack || "").split("\n");
    for (var i = 0; i < lines.length && out.length < MAX_FRAMES; i++) {
      var parsed = frame(lines[i]);
      if (parsed && parsed.abs_path.indexOf("/_npanel/") < 0) out.push(parsed);
    }
    return out.reverse();
  }

  // An error and its causes, the innermost cause first, as Sentry lists them.
  function exceptions(error, mechanism) {
    var values = [];
    var current = error;
    while (current !== undefined && current !== null && values.length <= MAX_CAUSES) {
      if (current instanceof Error || typeof current.stack === "string") {
        values.unshift({
          type: current.name || "Error",
          value: truncate(current.message || "", MAX_STRING),
          stacktrace: { frames: frames(current.stack) },
          mechanism: { type: mechanism, handled: false },
        });
      } else {
        var what = mechanism === "onunhandledrejection" ? "promise rejection" : "exception";
        values.unshift({
          type: "Error",
          value: truncate("Non-Error " + what + " captured with value: " + describe(current), MAX_STRING),
          mechanism: { type: mechanism, handled: false },
        });
      }
      current = current.cause;
    }
    return values;
  }

  function eventId() {
    var bytes = new Uint8Array(16);
    if (root.crypto && root.crypto.getRandomValues) {
      root.crypto.getRandomValues(bytes);
    } else {
      for (var i = 0; i < 16; i++) bytes[i] = Math.floor(Math.random() * 256);
    }
    var hex = "";
    for (var j = 0; j < 16; j++) hex += (bytes[j] < 16 ? "0" : "") + bytes[j].toString(16);
    return hex;
  }

  function report(values) {
    if (!send || sent >= MAX_EVENTS || !values.length) return;
    var main = values[values.length - 1];
    var stack = main.stacktrace ? main.stacktrace.frames : [];
    var top = stack[stack.length - 1];
    var key = main.type + ":" + main.value + ":" + (top ? top.abs_path + ":" + top.lineno + ":" + top.colno : "");
    if (seen[key]) return;
    seen[key] = true;
    sent++;

    var id = eventId();
    var event = {
      event_id: id,
      timestamp: now(),
      platform: "javascript",
      level: "error",
      release: config.release,
      sdk: { name: "npanel.errors", version: "1" },
      request: { url: String(root.location && root.location.href) },
      exception: { values: values },
      breadcrumbs: { values: breadcrumbs.slice() },
    };
    var header = JSON.stringify({ event_id: id, sent_at: new Date().toISOString() }) + "\n" + JSON.stringify({ type: "event" }) + "\n";
    var body = header + JSON.stringify(event);
    // The oldest breadcrumbs go first, half at a time, until it fits.
    while (body.length > MAX_BODY && event.breadcrumbs.values.length) {
      var crumbs = event.breadcrumbs.values;
      event.breadcrumbs.values = crumbs.length > 1 ? crumbs.slice(Math.ceil(crumbs.length / 2)) : [];
      body = header + JSON.stringify(event);
    }
    if (body.length > MAX_BODY) return;
    try {
      send.call(root, config.endpoint, { method: "POST", body: body, keepalive: true }).catch(function () {});
    } catch (e) {
      // Nowhere to report a failure to report.
    }
  }

  root.addEventListener("error", function (event) {
    // Resource load failures reach window listeners too, without a message.
    if (!event || (!event.error && !event.message)) return;
    if (event.error) {
      report(exceptions(event.error, "onerror"));
      return;
    }
    // What a cross-origin script threw, with everything about it withheld.
    if (/^(Uncaught )?Script error\.?$/.test(event.message)) return;
    report([
      {
        type: "Error",
        value: truncate(event.message, MAX_STRING),
        stacktrace: event.filename ? { frames: [{ abs_path: event.filename, lineno: event.lineno || undefined, colno: event.colno || undefined }] } : undefined,
        mechanism: { type: "onerror", handled: false },
      },
    ]);
  });

  root.addEventListener("unhandledrejection", function (event) {
    report(exceptions(event && event.reason, "onunhandledrejection"));
  });

  if (root.console) {
    ["error", "warn"].forEach(function (level) {
      var original = root.console[level];
      if (typeof original !== "function") return;
      root.console[level] = function () {
        var parts = [];
        for (var i = 0; i < arguments.length; i++) parts.push(describe(arguments[i]));
        crumb({ category: "console", level: level === "warn" ? "warning" : "error", message: truncate(parts.join(" "), MAX_STRING) });
        return original.apply(this, arguments);
      };
    });
  }

  function selector(element) {
    var parts = [];
    for (var node = element; node && node.tagName && parts.length < 3; node = node.parentElement) {
      var part = node.tagName.toLowerCase();
      if (node.id) part += "#" + node.id;
      var classes = typeof node.className === "string" ? node.className.trim().split(/\s+/).slice(0, 2) : [];
      for (var i = 0; i < classes.length; i++) if (classes[i]) part += "." + classes[i];
      var label = node.getAttribute && node.getAttribute("aria-label");
      if (label) part += '[aria-label="' + truncate(label, 40) + '"]';
      parts.unshift(part);
    }
    return parts.join(" > ");
  }

  if (root.document && root.document.addEventListener) {
    root.document.addEventListener(
      "click",
      function (event) {
        try {
          crumb({ category: "ui.click", message: selector(event.target) });
        } catch (e) {
          // A click on something odd is not worth an error of its own.
        }
      },
      true
    );
  }

  function where() {
    var location = root.location;
    return location ? location.pathname + location.search + location.hash : "";
  }

  if (root.history) {
    var last = where();
    var navigated = function () {
      var to = where();
      if (to !== last) crumb({ category: "navigation", data: { from: last, to: to } });
      last = to;
    };
    ["pushState", "replaceState"].forEach(function (method) {
      var original = root.history[method];
      if (typeof original !== "function") return;
      root.history[method] = function () {
        var result = original.apply(this, arguments);
        navigated();
        return result;
      };
    });
    root.addEventListener("popstate", navigated);
    root.addEventListener("hashchange", navigated);
  }

  if (send) {
    root.fetch = function (input, init) {
      var method = (init && init.method) || (input && input.method) || "GET";
      var url = typeof input === "string" ? input : input && input.url ? input.url : String(input);
      var data = { method: String(method).toUpperCase(), url: truncate(url, 300) };
      return send.apply(this, arguments).then(
        function (response) {
          data.status_code = response.status;
          crumb({ category: "fetch", level: response.status >= 400 ? "error" : "info", data: data });
          return response;
        },
        function (error) {
          crumb({ category: "fetch", level: "error", data: data, message: describe(error) });
          throw error;
        }
      );
    };
  }
})
