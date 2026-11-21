/** Unit tests for the core: set discovery, the save-verdict reading, the
 * bounded log, and the guards of an unstarted RustelProcess. The parts that
 * need a real rustel child are in integration.test.ts. */

import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { BoundedLog } from "../src/core/bounded.ts";
import { firstScoreOfSet, verdictFrom, writeScoreFile, RustelProcess } from "../src/core/rustel.ts";

function tempSet(): string {
  return mkdtempSync(join(tmpdir(), "pi-rustel-test-"));
}

test("the first score of a set is the first scene in studio order", () => {
  const set = tempSet();
  try {
    writeFileSync(join(set, "drop.strudel"), "// drop");
    writeFileSync(join(set, "Beta.strudel"), "// beta");
    writeFileSync(join(set, "alpha.strudel"), "// alpha");
    writeFileSync(join(set, "notes.txt"), "not a scene");
    mkdirSync(join(set, "nested.strudel")); // a folder, not a scene file
    assert.equal(firstScoreOfSet(set), join(set, "alpha.strudel"));
  } finally {
    rmSync(set, { recursive: true, force: true });
  }
});

test("a set without scenes, a missing folder and a file are refused", () => {
  const set = tempSet();
  try {
    assert.throws(() => firstScoreOfSet(set), /holds no \.strudel score/);
    assert.throws(() => firstScoreOfSet(join(set, "absent")), /does not exist/);
    assert.throws(() => firstScoreOfSet(join(set, "notes.txt") + "x"), /does not exist/);
    writeFileSync(join(set, "notes.txt"), "x");
    assert.throws(() => firstScoreOfSet(join(set, "notes.txt")), /not a folder/);
  } finally {
    rmSync(set, { recursive: true, force: true });
  }
});

test("a save that raises the index installs", () => {
  const verdict = verdictFrom(
    { kind: "score_active", at: 1, index: 3, source: 's("bd")\n' },
    2,
  );
  assert.ok(verdict !== null && verdict.outcome === "installed");
  assert.equal(verdict.index, 3);
  assert.equal(verdict.source, 's("bd")\n');
});

test("the score the set opened on is not an update verdict", () => {
  assert.equal(
    verdictFrom({ kind: "score_active", at: 0, index: 1, source: "" }, 1),
    null,
  );
});

test("the update-failed narration reads as a refusal with its reason", () => {
  const verdict = verdictFrom(
    {
      kind: "narration",
      text: "Update failed for /tmp/set/score.strudel: javascript: Expected `)`",
    },
    1,
  );
  assert.ok(verdict !== null && verdict.outcome === "rejected");
  assert.equal(verdict.reason, "javascript: Expected `)`");
});

test("the update-failed narration without a reason still refuses", () => {
  const verdict = verdictFrom(
    { kind: "narration", text: "Update failed for /tmp/set/score.strudel" },
    1,
  );
  assert.ok(verdict !== null && verdict.outcome === "rejected");
  assert.equal(verdict.reason, "the score was rejected");
});

test("other narration and errors say nothing about a save", () => {
  const lastIndex = 1;
  assert.equal(
    verdictFrom(
      { kind: "narration", text: "Updated /tmp/set/score.strudel" },
      lastIndex,
    ),
    null,
  );
  assert.equal(
    verdictFrom(
      {
        kind: "live_error",
        errorKind: "audio",
        message: "the device left",
        recoverable: false,
      },
      lastIndex,
    ),
    null,
  );
});

test("a rejected reload carries the engine's reason", () => {
  const verdict = verdictFrom(
    {
      kind: "reload",
      target: "score",
      status: "rejected",
      path: "score.strudel",
      generationBefore: 2,
      generationAfter: 2,
      errorKind: "evaluation",
      message: "javascript: Expected `)`",
    },
    2,
  );
  assert.ok(verdict !== null && verdict.outcome === "rejected");
  assert.equal(verdict.reason, "javascript: Expected `)`");
});

test("writing a save puts the exact source in the watched file", () => {
  const set = tempSet();
  try {
    const score = join(set, "score.strudel");
    writeFileSync(score, "old\n");
    writeScoreFile(score, 's("bd").slow(2)\n');
    assert.equal(readFileSync(score, "utf8"), 's("bd").slow(2)\n');
  } finally {
    rmSync(set, { recursive: true, force: true });
  }
});

test("the bounded log keeps the newest entries", () => {
  const log = new BoundedLog<number>(3);
  for (const value of [1, 2, 3, 4, 5]) {
    log.push(value);
  }
  assert.equal(log.length, 3);
  assert.deepEqual(log.snapshot(), [3, 4, 5]);
  log.clear();
  assert.equal(log.length, 0);
  assert.throws(() => new BoundedLog<number>(0), /at least 1/);
});

test("an unstarted RustelProcess reports nothing running and refuses updates and stops", async () => {
  const set = new RustelProcess();
  assert.equal(set.running, false);
  const status = set.status();
  assert.equal(status.running, false);
  assert.equal(status.active, null);
  assert.equal(status.events.length, 0);
  await assert.rejects(() => set.update('s("bd")\n'), /no set is running/);
  assert.deepEqual(await set.stop(), {
    outcome: "not-running",
    code: null,
    signal: null,
  });
});
