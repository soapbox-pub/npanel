import assert from "node:assert/strict";
import { resolve } from "node:path";
import { test } from "node:test";
import { Config } from "./config.ts";

function config(vars: Record<string, string>): Config {
  const map = new Map(Object.entries(vars));
  return new Config({ get: (key) => map.get(key) });
}

test("port defaults to 3000 and rejects garbage", () => {
  assert.equal(config({}).port, 3000);
  assert.equal(config({ PORT: "8080" }).port, 8080);
  assert.throws(() => config({ PORT: "nope" }).port, /PORT/);
  assert.throws(() => config({ PORT: "0" }).port, /PORT/);
  assert.throws(() => config({ PORT: "70000" }).port, /PORT/);
});

test("dashboardHost is required and normalized", () => {
  assert.throws(() => config({}).dashboardHost, /DASHBOARD_HOST/);
  assert.equal(config({ DASHBOARD_HOST: "Panel.Example.com" }).dashboardHost, "panel.example.com");
  assert.equal(config({ DASHBOARD_HOST: "panel.example.com:3000" }).dashboardHost, "panel.example.com");
  assert.throws(() => config({ DASHBOARD_HOST: "https://panel.example.com" }).dashboardHost, /bare hostname/);
});

test("logLevel defaults to info and validates", () => {
  assert.equal(config({}).logLevel, "info");
  assert.equal(config({ LOG_LEVEL: "DEBUG" }).logLevel, "debug");
  assert.throws(() => config({ LOG_LEVEL: "verbose" }).logLevel, /LOG_LEVEL/);
});

test("dataDir derives the database and blob paths", () => {
  const c = config({ DATA_DIR: "/srv/npanel" });
  assert.equal(c.dataDir, "/srv/npanel");
  assert.equal(c.databasePath, "/srv/npanel/npanel.sqlite3");
  assert.equal(c.blobsDir, "/srv/npanel/blobs");
  assert.equal(config({}).dataDir, resolve("./data"));
});
