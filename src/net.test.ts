import assert from "node:assert/strict";
import { test } from "node:test";
import { isPublicAddress } from "./net.ts";

test("private and reserved IPv4 addresses are refused", () => {
  for (const address of [
    "0.0.0.0",
    "10.1.2.3",
    "127.0.0.1",
    "100.64.0.1",
    "169.254.169.254",
    "172.16.0.1",
    "192.168.1.1",
    "224.0.0.1",
    "255.255.255.255",
  ]) {
    assert.equal(isPublicAddress(address), false, address);
  }
});

test("public addresses of either family are allowed", () => {
  for (const address of ["93.184.215.14", "1.1.1.1", "2606:4700::1111", "2a00:1450:4001:82a::200e"]) {
    assert.equal(isPublicAddress(address), true, address);
  }
});

test("IPv6 loopback, local, and reserved ranges are refused", () => {
  for (const address of [
    "::",
    "::1",
    "0:0:0:0:0:0:0:1",
    "fc00::1",
    "fd12:3456::1",
    "fe80::1%eth0",
    "fec0::1",
    "ff02::1",
    "2001:db8::1",
    "100::1",
    "2001:0:4136:e378::1",
    "64:ff9b:1::1",
  ]) {
    assert.equal(isPublicAddress(address), false, address);
  }
});

test("an IPv6 address is only as public as the IPv4 address inside it", () => {
  // IPv4-mapped, in both notations.
  assert.equal(isPublicAddress("::ffff:127.0.0.1"), false);
  assert.equal(isPublicAddress("::ffff:7f00:1"), false);
  assert.equal(isPublicAddress("::ffff:93.184.215.14"), true);
  // The deprecated IPv4-compatible form is refused whatever it holds.
  assert.equal(isPublicAddress("::127.0.0.1"), false);
  assert.equal(isPublicAddress("::7f00:1"), false);
  // NAT64 reaches the IPv4 address it embeds.
  assert.equal(isPublicAddress("64:ff9b::a9fe:a9fe"), false);
  assert.equal(isPublicAddress("64:ff9b::169.254.169.254"), false);
  assert.equal(isPublicAddress("64:ff9b::5db8:d70e"), true);
  // As does 6to4.
  assert.equal(isPublicAddress("2002:c0a8:0101::1"), false);
  assert.equal(isPublicAddress("2002:5db8:d70e::1"), true);
});

test("anything that isn't an address is refused", () => {
  for (const address of ["", "example.com", "1.2.3", "1::2::3"]) {
    assert.equal(isPublicAddress(address), false, address);
  }
});
