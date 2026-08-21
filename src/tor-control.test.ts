import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, describe, it } from "node:test";
import { type FakeTor, protocolInfo, startFakeTor } from "./test/fake-tor.ts";
import { TorControl, TorControlError } from "./tor-control.ts";

const ONION = "250-ServiceID=vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd\r\n";
const KEY = "250-PrivateKey=ED25519-V3:c2VjcmV0\r\n";

describe("TorControl", () => {
  const running: FakeTor[] = [];

  after(async () => {
    await Promise.all(running.map((tor) => tor.close()));
  });

  async function connect(respond: (command: string) => string, password = ""): Promise<[TorControl, FakeTor]> {
    const tor = await startFakeTor(respond);
    running.push(tor);
    const control = await TorControl.connect({ url: `tcp://127.0.0.1:${tor.port}`, password });
    return [control, tor];
  }

  it("authenticates with no credentials when Tor asks for none", async () => {
    const [control, tor] = await connect((command) =>
      command === "PROTOCOLINFO 1" ? protocolInfo("METHODS=NULL") : "250 OK\r\n",
    );

    assert.deepEqual(tor.commands, ["PROTOCOLINFO 1", "AUTHENTICATE"]);
    control.close();
  });

  it("authenticates with the cookie file Tor names", async () => {
    const dir = await mkdtemp(join(tmpdir(), "npanel-tor-"));
    const cookie = join(dir, "control_auth_cookie");
    await writeFile(cookie, new Uint8Array([0x00, 0x0f, 0xff]));

    const [control, tor] = await connect((command) =>
      command === "PROTOCOLINFO 1" ? protocolInfo(`METHODS=COOKIE,SAFECOOKIE COOKIEFILE="${cookie}"`) : "250 OK\r\n",
    );

    assert.equal(tor.commands[1], "AUTHENTICATE 000fff");
    control.close();
  });

  it("authenticates with a password, quoted the way Tor spells strings", async () => {
    const [control, tor] = await connect(
      (command) => (command === "PROTOCOLINFO 1" ? protocolInfo("METHODS=HASHEDPASSWORD") : "250 OK\r\n"),
      'pa"ss\\word',
    );

    assert.equal(tor.commands[1], 'AUTHENTICATE "pa\\"ss\\\\word"');
    control.close();
  });

  it("says what to do when Tor wants a password and none is set", async () => {
    const tor = await startFakeTor((command) =>
      command === "PROTOCOLINFO 1" ? protocolInfo("METHODS=HASHEDPASSWORD") : "250 OK\r\n",
    );
    running.push(tor);

    await assert.rejects(TorControl.connect({ url: `tcp://127.0.0.1:${tor.port}`, password: "" }), /Settings/);
  });

  it("explains an unreadable cookie file in terms of the group that owns it", async () => {
    const tor = await startFakeTor((command) =>
      command === "PROTOCOLINFO 1" ? protocolInfo('METHODS=COOKIE COOKIEFILE="/nonexistent/cookie"') : "250 OK\r\n",
    );
    running.push(tor);

    await assert.rejects(TorControl.connect({ url: `tcp://127.0.0.1:${tor.port}`, password: "" }), /debian-tor/);
  });

  it("creates an onion service and returns the address with its key", async () => {
    const [control, tor] = await connect((command) => {
      if (command === "PROTOCOLINFO 1") return protocolInfo("METHODS=NULL");
      if (command.startsWith("ADD_ONION")) return `${ONION}${KEY}250 OK\r\n`;
      return "250 OK\r\n";
    });

    const key = await control.addOnion(41234);

    assert.equal(tor.commands[2], "ADD_ONION NEW:ED25519-V3 Port=80,127.0.0.1:41234");
    assert.equal(key.address, "vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd.onion");
    assert.equal(key.secretKey, "ED25519-V3:c2VjcmV0");
    control.close();
  });

  it("republishes a stored key rather than asking for a new one", async () => {
    const existing = {
      address: "vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd.onion",
      secretKey: "ED25519-V3:c2VjcmV0",
    };

    const [control, tor] = await connect((command) => {
      if (command === "PROTOCOLINFO 1") return protocolInfo("METHODS=NULL");
      if (command.startsWith("ADD_ONION")) return `${ONION}250 OK\r\n`;
      return "250 OK\r\n";
    });

    const key = await control.addOnion(41234, existing);

    assert.equal(tor.commands[2], "ADD_ONION ED25519-V3:c2VjcmV0 Port=80,127.0.0.1:41234");
    assert.deepEqual(key, existing);
    control.close();
  });

  it("treats an address collision as the service already being up", async () => {
    const existing = {
      address: "vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd.onion",
      secretKey: "ED25519-V3:c2VjcmV0",
    };

    const [control] = await connect((command) => {
      if (command === "PROTOCOLINFO 1") return protocolInfo("METHODS=NULL");
      if (command.startsWith("ADD_ONION")) return "550 Onion address collision\r\n";
      return "250 OK\r\n";
    });

    assert.deepEqual(await control.addOnion(41234, existing), existing);
    control.close();
  });

  it("does not mistake an asynchronous event for a reply", async () => {
    const [control] = await connect((command) => {
      if (command === "PROTOCOLINFO 1") return protocolInfo("METHODS=NULL");
      // Tor is free to interleave 6xx event lines with a reply, and a reader
      // that counted lines instead of codes would answer with this one.
      if (command.startsWith("ADD_ONION")) {
        return `650 HS_DESC UPLOAD somewhere\r\n${ONION}${KEY}250 OK\r\n`;
      }
      return "250 OK\r\n";
    });

    const key = await control.addOnion(41234);

    assert.equal(key.address, "vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd.onion");
    control.close();
  });

  it("rejects a refused command with the code Tor gave", async () => {
    const [control] = await connect((command) => {
      if (command === "PROTOCOLINFO 1") return protocolInfo("METHODS=NULL");
      if (command.startsWith("DEL_ONION")) return "552 Unknown service id\r\n";
      return "250 OK\r\n";
    });

    await assert.rejects(control.delOnion("nope.onion"), (error: unknown) => {
      assert.ok(error instanceof TorControlError);
      assert.equal(error.code, 552);
      return true;
    });

    control.close();
  });

  it("reports the connection closing, so callers can reconnect", async () => {
    const [control, tor] = await connect((command) =>
      command === "PROTOCOLINFO 1" ? protocolInfo("METHODS=NULL") : "250 OK\r\n",
    );

    const closed = new Promise<Error>((resolve) => control.once("close", resolve));
    tor.drop();

    assert.match((await closed).message, /clos/i);
  });
});
