/**
 * The rustel live event stream, parsed into typed events.
 *
 * `rustel play <score> --watch --score-events` writes two kinds of line to
 * the child's stderr, sharing one stream:
 *
 * - JSON objects. `score_active` is always a JSON line (crates/cli/src/
 *   replay.rs, `announce_active_score`), and every other live event is JSON
 *   too when the run speaks JSON (`--json`, `-vvv`, `--ui-events`;
 *   crates/cli/src/live.rs, `LiveOutput::render`).
 * - Human narration, the sentences `human_live_event` writes in
 *   crates/cli/src/main.rs: "Updated <path>", "Update failed for <path>: …",
 *   "Playback stopped, letting the tail ring out …".
 *
 * This module mirrors those emitters so the parser is grounded in what
 * rustel actually prints. Narration never starts with `{`, so a line is
 * narration exactly when it does not parse as a JSON object.
 */

/** The score that is sounding right now, announced as base64 JSON. */
export type ScoreActiveEvent = {
  kind: "score_active";
  /** Seconds into the set when the save became the sounding score. */
  at: number;
  /** Which save this is; the score the set opened on is 1. */
  index: number;
  /** How many saves exist in total; only a replay tape knows. */
  of?: number;
  /** How many non-empty lines the score has, as rustel counted them. */
  lines?: number;
  /** The score text, decoded from the base64 in the JSON line. */
  source: string;
  /** Why `source` could not be decoded, when it could not. */
  decodeError?: string;
};

/** A `live_error` object: something that went wrong inside the set. */
export type LiveErrorEvent = {
  kind: "live_error";
  /** What went wrong: "evaluation", "audio", "panic", "sample-failed", … */
  errorKind: string;
  message: string;
  /** Whether the set plays on after the error. */
  recoverable: boolean;
  streamId?: number;
};

/** A `reload` object: the verdict on one save of the watched score. */
export type ReloadEvent = {
  kind: "reload";
  target: "score" | "prebake";
  status: "installed" | "rejected";
  path: string;
  generationBefore: number;
  generationAfter: number;
  errorKind?: string;
  message?: string;
};

/** A `live` object: the set starting, stopping or recycling its output. */
export type LiveEvent = {
  kind: "live";
  status: "started" | "stopped" | "recycled";
  /** The engine generation the event speaks of, when the line carries it. */
  generation?: number;
  /** Whether a signal, not the score, ended the set. */
  interrupted?: boolean;
  /** "success", "interrupted", an error kind, … */
  outcomeKind?: string;
};

/** A human narration line from the same stderr the JSON lines use. */
export type NarrationEvent = {
  kind: "narration";
  text: string;
};

export type RustelEvent =
  | ScoreActiveEvent
  | LiveErrorEvent
  | ReloadEvent
  | LiveEvent
  | NarrationEvent;

/**
 * Decode the base64 `source` of a `score_active` line.
 *
 * rustel's `decode_base64` (crates/runtime/src/session_log.rs) skips padding
 * and whitespace and rejects every character outside the standard alphabet;
 * mirror that so a corrupted line is reported, not misread as score text.
 */
export function decodeScoreSource(
  encoded: string,
): { source: string } | { decodeError: string } {
  const packed = encoded.replace(/[\s=]/g, "");
  if (/[^A-Za-z0-9+/]/.test(packed)) {
    return { decodeError: "invalid base64 character in the score source" };
  }
  return { source: Buffer.from(packed, "base64").toString("utf8") };
}

function string(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

function number(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function fields(value: unknown): Record<string, unknown> | null {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return null;
  }
  return value as Record<string, unknown>;
}

function parseScoreActive(value: Record<string, unknown>): ScoreActiveEvent {
  const decoded = decodeScoreSource(string(value.source) ?? "");
  const event: ScoreActiveEvent = {
    kind: "score_active",
    at: number(value.t) ?? 0,
    index: number(value.index) ?? 0,
    source: "source" in decoded ? decoded.source : "",
  };
  const of = number(value.of);
  if (of !== undefined) {
    event.of = of;
  }
  const lines = number(value.lines);
  if (lines !== undefined) {
    event.lines = lines;
  }
  if ("decodeError" in decoded) {
    event.decodeError = decoded.decodeError;
  }
  return event;
}

function parseLiveError(value: Record<string, unknown>): LiveErrorEvent {
  const event: LiveErrorEvent = {
    kind: "live_error",
    errorKind: string(value.kind) ?? "unknown",
    message: string(value.message) ?? "",
    recoverable: value.recoverable !== false,
  };
  const streamId = number(value.stream_id);
  if (streamId !== undefined) {
    event.streamId = streamId;
  }
  return event;
}

function parseReload(value: Record<string, unknown>): ReloadEvent {
  const event: ReloadEvent = {
    kind: "reload",
    target: string(value.target) === "prebake" ? "prebake" : "score",
    status: string(value.status) === "rejected" ? "rejected" : "installed",
    path: string(value.path) ?? "",
    generationBefore: number(value.generation_before) ?? 0,
    generationAfter: number(value.generation_after) ?? 0,
  };
  const errorKind = string(value.error_kind);
  if (errorKind !== undefined) {
    event.errorKind = errorKind;
  }
  const message = string(value.message);
  if (message !== undefined) {
    event.message = message;
  }
  return event;
}

function parseLive(value: Record<string, unknown>): LiveEvent {
  const status =
    string(value.status) === "recycled"
      ? "recycled"
      : string(value.status) === "stopped"
        ? "stopped"
        : "started";
  const event: LiveEvent = { kind: "live", status };
  const generation = number(value.generation);
  if (generation !== undefined) {
    event.generation = generation;
  }
  if (typeof value.interrupted === "boolean") {
    event.interrupted = value.interrupted;
  }
  const outcomeKind = string(value.outcome_kind);
  if (outcomeKind !== undefined) {
    event.outcomeKind = outcomeKind;
  }
  return event;
}

function jsonEvent(value: Record<string, unknown>, text: string): RustelEvent {
  const scoreActive = fields(value.score_active);
  if (scoreActive !== null) {
    return parseScoreActive(scoreActive);
  }
  const liveError = fields(value.live_error);
  if (liveError !== null) {
    return parseLiveError(liveError);
  }
  const reload = fields(value.reload);
  if (reload !== null) {
    return parseReload(reload);
  }
  const live = fields(value.live);
  if (live !== null) {
    return parseLive(live);
  }
  // A JSON line this parser does not know is still part of the stream the
  // terminal showed, so it stays in the ring as text rather than vanishing.
  return { kind: "narration", text };
}

/**
 * Parse one stderr line. Returns null for blank lines, a typed event for a
 * JSON line, and narration for everything else.
 */
export function parseEventLine(line: string): RustelEvent | null {
  const text = line.trim();
  if (text === "") {
    return null;
  }
  if (text.startsWith("{")) {
    try {
      const parsed: unknown = JSON.parse(text);
      const value = fields(parsed);
      if (value !== null) {
        return jsonEvent(value, text);
      }
    } catch {
      // A line that starts with "{" but does not parse is still text the
      // terminal showed; keep it as narration.
    }
  }
  return { kind: "narration", text };
}
