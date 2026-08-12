import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { createDashboardServer } from "./dashboard.ts";
import { Logger } from "./log.ts";

const dirs: string[] = [];

after(async () => {
  for (const dir of dirs) await rm(dir, { recursive: true, force: true });
});

const INDEX = [
  "<!doctype html>",
  '<meta property="og:image" content="/og.png">',
  '<meta property="og:image:alt" content="npanel">',
  '<meta property="og:url" content="/">',
  '<meta property="og:title" content="npanel">',
].join("\n");

/** A dashboard directory holding `index.html` and one asset. */
async function setup(dashboardHost = "panel.example.com") {
  const dir = await mkdtemp(join(tmpdir(), "npanel-dashboard-"));
  dirs.push(dir);

  await writeFile(join(dir, "index.html"), INDEX);
  await writeFile(join(dir, "og.png"), "not really a png");

  return createDashboardServer({ dir, dashboardHost, log: new Logger("error") });
}

test("link preview URLs are made absolute", async () => {
  const serve = await setup();
  const html = await (await serve(new Request("https://panel.example.com/"))).text();

  assert.match(html, /<meta property="og:image" content="https:\/\/panel\.example\.com\/og\.png">/);
  assert.match(html, /<meta property="og:url" content="https:\/\/panel\.example\.com\/">/);
});

test("meta tags that aren't URLs are left alone", async () => {
  const serve = await setup();
  const html = await (await serve(new Request("https://panel.example.com/"))).text();

  assert.match(html, /<meta property="og:image:alt" content="npanel">/);
  assert.match(html, /<meta property="og:title" content="npanel">/);
});

test("a loopback dashboard host previews over http", async () => {
  const serve = await setup("localhost");
  const html = await (await serve(new Request("http://localhost/"))).text();

  assert.match(html, /content="http:\/\/localhost\/og\.png"/);
});

test("content-length matches the rewritten body", async () => {
  const serve = await setup();
  const response = await serve(new Request("https://panel.example.com/"));
  const body = await response.arrayBuffer();

  assert.equal(response.headers.get("content-length"), String(body.byteLength));
  assert.notEqual(body.byteLength, INDEX.length, "the body should have grown");
});

test("unknown paths fall back to index.html", async () => {
  const serve = await setup();
  const response = await serve(new Request("https://panel.example.com/settings"));

  assert.equal(response.headers.get("content-type"), "text/html; charset=utf-8");
  assert.match(await response.text(), /og:image/);
});

test("non-HTML files are served untouched", async () => {
  const serve = await setup();
  const response = await serve(new Request("https://panel.example.com/og.png"));

  assert.equal(await response.text(), "not really a png");
});

test("HEAD returns the rewritten length without a body", async () => {
  const serve = await setup();
  const response = await serve(new Request("https://panel.example.com/", { method: "HEAD" }));

  assert.equal(await response.text(), "");
  assert.equal(response.headers.get("content-length"), String(INDEX.length + 2 * "https://panel.example.com".length));
});

test("paths cannot escape the dashboard directory", async () => {
  const serve = await setup();
  const response = await serve(new Request("https://panel.example.com/../../etc/passwd"));

  // Falls back to the SPA shell rather than reading outside the directory.
  assert.match(await response.text(), /og:image/);
});
