/** Integration test against the real installed rustel binary: one set,
 * started, updated, refused, stopped. No audio is attempted - the set runs
 * with `--output silent`, so the machine needs no audio device. The test
 * skips when no rustel binary can be found. */

import assert from "node:assert/strict";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join, sep } from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { RustelProcess } from "../src/core/rustel.ts";
import factory from "../src/index.ts";

/** The first candidate that answers `--version`: $RUSTEL_BIN, `rustel` from
 * PATH, then the cargo install root this machine uses. */
function resolveBinary(): string | null {
  const candidates = [
    process.env.RUSTEL_BIN,
    "rustel",
    join(homedir(), ".cargo", "bin", "rustel"),
  ].filter((candidate): candidate is string => candidate !== undefined && candidate !== "");
  for (const candidate of candidates) {
    if (candidate.includes(sep)) {
      if (existsSync(candidate)) {
        return candidate;
      }
      continue;
    }
    const probe = spawnSync(candidate, ["--version"], { encoding: "utf8" });
    if (probe.error === undefined) {
      return candidate;
    }
  }
  return null;
}

const BINARY = resolveBinary();
const skipWhenAbsent = BINARY === null ? "no rustel binary found" : false;

test(
  "a set plays, installs a save, refuses a broken save and stops",
  { skip: skipWhenAbsent },
  async () => {
    const setPath = mkdtempSync(join(tmpdir(), "pi-rustel-integration-"));
    const scorePath = join(setPath, "score.strudel");
    writeFileSync(scorePath, 's("bd")\n');
    
    const live = new RustelProcess();
    try {
      await live.start(setPath, { binary: BINARY as string, output: "silent" });

      // The set opened on its first scene and announced it.
      let status = live.status();
      assert.equal(status.running, true);
      assert.equal(status.scorePath, scorePath);
      assert.equal(status.active?.index, 1);
      assert.equal(status.active?.source, 's("bd")\n');

      // A good save installs: the index rises and the source decodes.
      const installed = await live.update('s("bd").slow(2)\n');
      assert.ok(installed.outcome === "installed", JSON.stringify(installed));
      assert.equal(installed.index, 2);
      assert.equal(installed.source, 's("bd").slow(2)\n');
      status = live.status();
      assert.equal(status.active?.index, 2);
      assert.equal(status.active?.source, 's("bd").slow(2)\n');

      // A broken save is refused by the lint gate: the last good score
      // keeps playing and the reason names the fault.
      const refused = await live.update("this is (broken\n");
      assert.ok(refused.outcome === "rejected", JSON.stringify(refused));
      assert.match(refused.reason, /javascript/);
      status = live.status();
      assert.equal(status.active?.index, 2, "the refused save must not bump the index");
      assert.equal(status.active?.source, 's("bd").slow(2)\n');
      const errors = status.recentErrors;
      assert.ok(errors.length > 0, "the refusal is kept as a recent error");
      assert.match(errors[errors.length - 1].message, /javascript/);

      // The events the set produced are held, bounded, for status.
      const events = live.status().events;
      assert.ok(events.length > 0);
      assert.ok(events.length <= 64);

      // One SIGINT stops the set cleanly.
      const stop = await live.stop();
      assert.ok(
        stop.outcome === "stopped" || stop.outcome === "quit",
        `the set should stop on SIGINT, not ${stop.outcome}`,
      );
      assert.equal(live.running, false);
      assert.equal(live.status().running, false);
    } finally {
      await live.stop();
      rmSync(setPath, { recursive: true, force: true });
    }
  },
);

test(
  "a stopped set may be started again",
  { skip: skipWhenAbsent },
  async () => {
    const setPath = mkdtempSync(join(tmpdir(), "pi-rustel-integration-"));
    writeFileSync(join(setPath, "score.strudel"), 's("bd")\n');
    const live = new RustelProcess();
    try {
      await live.start(setPath, { binary: BINARY as string, output: "silent" });
      await live.stop();
      await live.start(setPath, { binary: BINARY as string, output: "silent" });
      const status = live.status();
      assert.equal(status.running, true);
      assert.equal(status.active?.index, 1, "a new set opens on save 1");
      await live.stop();
    } finally {
      await live.stop();
      rmSync(setPath, { recursive: true, force: true });
    }
  },
);

test(
  "a timed-out update leaves no stale waiter behind",
  { skip: skipWhenAbsent },
  async () => {
    const setPath = mkdtempSync(join(tmpdir(), "pi-rustel-integration-"));
    writeFileSync(join(setPath, "score.strudel"), 's("bd")\n');
    const live = new RustelProcess();
    try {
      await live.start(setPath, { binary: BINARY as string, output: "silent" });
      const unknown = await live.update('s("bd").slow(2)\n', 1);
      assert.ok(unknown.outcome === "unknown", JSON.stringify(unknown));
      // The cleanup is not observable through the public surface (a settled
      // promise is harmless to re-finish), so the regression test looks at
      // the waiters the process itself keeps: the timed-out one must go, or
      // every timed-out update would leak a closure that observe then
      // visits on every later stream event.
      const waiters = (live as unknown as { waiters: unknown[] }).waiters;
      assert.equal(waiters.length, 0, "the timed-out waiter must be removed");
      // The set still reads the next verdict.
      const installed = await live.update('s("bd").slow(2)\n');
      assert.ok(installed.outcome === "installed", JSON.stringify(installed));
      await live.stop();
    } finally {
      await live.stop();
      rmSync(setPath, { recursive: true, force: true });
    }
  },
);

/** The smallest ExtensionAPI the tool layer needs: capture the registered
 * tools so a test can call them the way pi would. */
function stubPi() {
  const tools = new Map<
    string,
    {
      execute: (
        toolCallId: string,
        params: Record<string, unknown>,
        signal: AbortSignal | undefined,
        onUpdate: unknown,
        ctx: unknown,
      ) => Promise<{
        isError?: boolean;
        content: Array<{ type: string; text: string }>;
      }>;
    }
  >();
  return {
    tools,
    registerTool: (definition: { name: string }) =>
      tools.set(definition.name, definition as never),
    on: () => {},
    sendMessage: () => {},
  };
}

test(
  "a start that returns after its signal aborted stops the set",
  { skip: skipWhenAbsent },
  async () => {
    const setPath = mkdtempSync(join(tmpdir(), "pi-rustel-integration-"));
    writeFileSync(join(setPath, "score.strudel"), 's("bd")\n');
    const pi = stubPi();
    factory(pi as unknown as never);
    const start = pi.tools.get("rustel_start");
    const status = pi.tools.get("rustel_status");
    const stop = pi.tools.get("rustel_stop");
    assert.ok(start !== undefined && status !== undefined && stop !== undefined);
    const controller = new AbortController();
    controller.abort();
    try {
      const refused = await start.execute(
        "id",
        { set: setPath, binary: BINARY, output: "silent" },
        controller.signal,
        undefined,
        { cwd: process.cwd() },
      );
      assert.equal(refused.isError, true);
      assert.match(refused.content[0].text, /the session ended while the set was starting/);
      // The aborted start must not install the set: shutdown has already
      // run by now, so nothing else would stop the child.
      const reported = await status.execute("id", {}, undefined, undefined, {
        cwd: process.cwd(),
      });
      assert.match(reported.content[0].text, /no set was ever started/);
      await stop.execute("id", {}, undefined, undefined, { cwd: process.cwd() });
    } finally {
      await stop.execute("id", {}, undefined, undefined, { cwd: process.cwd() });
      rmSync(setPath, { recursive: true, force: true });
    }
  },
);
