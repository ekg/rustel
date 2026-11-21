/** Unit tests for the stderr event parser. The JSON fixtures are lines a real
 * `rustel play --watch --score-events` run wrote, and the reload fixture is
 * the one crates/cli/src/main.rs tests its own renderer with. */

import assert from "node:assert/strict";
import test from "node:test";
import {
  decodeScoreSource,
  parseEventLine,
} from "../src/core/events.ts";

test("a score_active line decodes its base64 source", () => {
  const event = parseEventLine(
    '{"score_active":{"t":2.075,"index":2,"lines":1,"source":"cygiYmQiKS5zbG93KDIpCg=="}}',
  );
  assert.ok(event !== null && event.kind === "score_active");
  assert.equal(event.at, 2.075);
  assert.equal(event.index, 2);
  assert.equal(event.lines, 1);
  assert.equal(event.source, 's("bd").slow(2)\n');
  assert.equal(event.decodeError, undefined);
});

test("the opening score_active of a real run parses", () => {
  const event = parseEventLine(
    '{"score_active":{"t":0.0,"index":1,"lines":1,"source":"cygiYmQiKQo="}}',
  );
  assert.ok(event !== null && event.kind === "score_active");
  assert.equal(event.index, 1);
  assert.equal(event.source, 's("bd")\n');
});

test("a replay score_active carries the total", () => {
  const event = parseEventLine(
    '{"score_active":{"t":1.5,"index":3,"of":12,"lines":2,"source":"YQo="}}',
  );
  assert.ok(event !== null && event.kind === "score_active");
  assert.equal(event.of, 12);
  assert.equal(event.source, "a\n");
});

test("an invalid base64 source is reported, not misread", () => {
  const event = parseEventLine(
    '{"score_active":{"t":0.0,"index":1,"source":"not*base64*"}}',
  );
  assert.ok(event !== null && event.kind === "score_active");
  assert.equal(event.source, "");
  assert.match(event.decodeError ?? "", /invalid base64/);
  assert.equal(decodeScoreSource("aGk=").source, "hi");
});

test("a live_error object parses with its kind and recoverability", () => {
  const event = parseEventLine(
    '{"live_error":{"kind":"evaluation","message":"javascript: Expected `)`","recoverable":true,"stream_id":3}}',
  );
  assert.ok(event !== null && event.kind === "live_error");
  assert.equal(event.errorKind, "evaluation");
  assert.equal(event.message, "javascript: Expected `)`");
  assert.equal(event.recoverable, true);
  assert.equal(event.streamId, 3);
});

test("a rejected reload parses with its generations and reason", () => {
  const event = parseEventLine(
    '{"reload":{"target":"score","stream_id":0,"path":"score.strudel","status":"rejected","generation_before":2,"generation_after":2,"error_kind":"evaluation","message":"javascript: Expected `)`","device":{}}}',
  );
  assert.ok(event !== null && event.kind === "reload");
  assert.equal(event.status, "rejected");
  assert.equal(event.generationBefore, 2);
  assert.equal(event.generationAfter, 2);
  assert.equal(event.errorKind, "evaluation");
  assert.equal(event.message, "javascript: Expected `)`");
});

test("a started live object carries the engine generation", () => {
  const event = parseEventLine(
    '{"live":{"status":"started","generation":7,"path":"score.strudel","device":"silent"}}',
  );
  assert.ok(event !== null && event.kind === "live");
  assert.equal(event.status, "started");
  assert.equal(event.generation, 7);
});

test("narration stays narration beside the JSON lines", () => {
  const narration = [
    "Audio out silent · silent · 48000 Hz · 256 frames · 5.3 ms",
    "Updated /tmp/set/score.strudel",
    "Update failed for /tmp/set/score.strudel: javascript: Expected a semicolon",
    "Playback stopped, letting the tail ring out - Ctrl-C again to quit now",
  ];
  for (const line of narration) {
    const event = parseEventLine(line);
    assert.ok(event !== null && event.kind === "narration", line);
    assert.equal(event.text, line);
  }
});

test("a broken JSON-looking line is kept as narration", () => {
  const event = parseEventLine('{"score_active":');
  assert.ok(event !== null && event.kind === "narration");
  assert.equal(event.text, '{"score_active":');
});

test("unknown JSON is kept as narration text, not dropped", () => {
  const event = parseEventLine('{"engine_pressure":{"status":"busy"}}');
  assert.ok(event !== null && event.kind === "narration");
  assert.match(event.text, /engine_pressure/);
});

test("blank lines are nothing", () => {
  assert.equal(parseEventLine(""), null);
  assert.equal(parseEventLine("   "), null);
});
