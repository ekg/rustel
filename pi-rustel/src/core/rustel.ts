/**
 * The spawn/parse/lifecycle core for one live rustel set.
 *
 * This module has no pi imports: it spawns `rustel play <score> --watch
 * --score-events --no-save-session`, parses the child's stderr (JSON events
 * and human narration, see ./events.ts), and owns the child's whole life.
 * A thin pi-facing layer in ../index.ts adapts it to tool registrations, so
 * another host (an MCP server, say) could lift this core unchanged.
 */

import { spawn } from "node:child_process";
import type { ChildProcess } from "node:child_process";
import { readdirSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { BoundedLog } from "./bounded.ts";
import {
  parseEventLine,
  type LiveErrorEvent,
  type RustelEvent,
  type ScoreActiveEvent,
} from "./events.ts";

/** A scene of a set is a `.strudel` file, as in the studio's scene loader. */
export const SCENE_EXTENSION = "strudel";

/** How many recent events the ring keeps for `rustel_status`. */
const RING_LIMIT = 64;
/** How many recent errors are kept for `rustel_status`. */
const ERROR_LIMIT = 8;
/** Score text stored in the ring is cut here, with a marker, so one huge
 * save cannot make the status payload huge too. */
const STORED_SOURCE_LIMIT = 8_192;
/** How long `start` waits for the opening `score_active` line. */
const DEFAULT_READY_TIMEOUT_MS = 10_000;
/** How long `update` waits for the engine's verdict on one save. The watch
 * polls every 20 ms and debounces 100 ms (crates/cli/src/live.rs), so a
 * verdict that needs longer than this is not coming. */
const DEFAULT_UPDATE_TIMEOUT_MS = 5_000;
/** One SIGINT stops the set and lets the tail ring out; this is how long
 * that may take before a second SIGINT skips the wait. */
const STOP_GRACE_MS = 3_000;
/** How long the second SIGINT may take before the child is killed. */
const QUIT_GRACE_MS = 3_000;
/** How long SIGKILL gets to be observed. */
const KILL_GRACE_MS = 2_000;

/** The sentence the CLI writes when the lint gate refused a save; the reason
 * follows the colon (crates/cli/src/main.rs, `human_live_event`). */
const UPDATE_FAILED_PREFIX = "Update failed for ";

export type StartOptions = {
  /** The rustel binary to run; the default is `rustel` from PATH. */
  binary?: string;
  /** Audio output device name, or `silent` to run with no audio hardware;
   * omitted runs on the default device. */
  output?: string;
  /** How long `start` waits for the opening score; default 10 s. */
  readyTimeoutMs?: number;
};

/** What happened to one save written through `update`. */
export type UpdateOutcome =
  | {
      outcome: "installed";
      /** The save number that is now sounding. */
      index: number;
      source: string;
      decodeError?: string;
    }
  | {
      outcome: "rejected";
      /** Why the engine's lint gate refused the score. */
      reason: string;
    }
  | {
      outcome: "unknown";
      /** Why no verdict was seen: a timeout, or the child dying first. */
      note: string;
    };

export type StopOutcome = {
  outcome: "not-running" | "already-exited" | "stopped" | "quit" | "killed";
  code: number | null;
  signal: string | null;
};

export type RecentError = {
  /** Milliseconds since the set spawned. */
  ms: number;
  /** What kind of error the stream called it; "narration" for `Error:` lines. */
  kind: string;
  message: string;
};

export type RingEntry = {
  /** Milliseconds since the set spawned. */
  ms: number;
  event: RustelEvent;
};

export type RustelStatus = {
  running: boolean;
  pid: number | null;
  setPath: string | null;
  scorePath: string | null;
  binary: string | null;
  exit: { code: number | null; signal: string | null } | null;
  /** The last `score_active`, its source decoded; null before the first. */
  active: {
    index: number;
    at: number;
    of?: number;
    source: string;
    decodeError?: string;
  } | null;
  /** The engine generation the stream last named; narration-only streams
   * (the flags this module spawns with) do not carry one. */
  generation: number | null;
  recentErrors: readonly RecentError[];
  events: readonly RingEntry[];
};

type ExitInfo = { code: number | null; signal: string | null };

/**
 * The first `.strudel` score of a set folder, in the order the studio opens
 * scenes: file name, case-insensitive (crates/studio/src/scenes.rs,
 * `open_folder`).
 */
export function firstScoreOfSet(setPath: string): string {
  let stats;
  try {
    stats = statSync(setPath);
  } catch {
    throw new Error(`the set folder does not exist: ${setPath}`);
  }
  if (!stats.isDirectory()) {
    throw new Error(
      `a set is a folder of .${SCENE_EXTENSION} scores; this path is not a folder: ${setPath}`,
    );
  }
  const scores = readdirSync(setPath, { withFileTypes: true })
    .filter(
      (entry) =>
        entry.isFile() && entry.name.toLowerCase().endsWith(`.${SCENE_EXTENSION}`),
    )
    .map((entry) => entry.name)
    .sort((a, b) => a.toLowerCase().localeCompare(b.toLowerCase()));
  if (scores.length === 0) {
    throw new Error(`the set folder holds no .${SCENE_EXTENSION} score: ${setPath}`);
  }
  return join(setPath, scores[0]);
}

/** Write one save of the watched score. The engine's watch follows the file
 * and lints the save; a broken score is refused there, not here. */
export function writeScoreFile(scorePath: string, source: string): void {
  writeFileSync(scorePath, source);
}

/**
 * The verdict one stream event carries about a save, given the index the set
 * was on before the save. Returns null when the event says nothing about it.
 *
 * With the flags this module spawns with, an installed save announces a new
 * `score_active` (a higher index) and a refused one writes the
 * "Update failed for <path>: <reason>" narration. A JSON-speaking run says
 * the same through a `reload` object instead.
 */
export function verdictFrom(event: RustelEvent, lastIndex: number): UpdateOutcome | null {
  if (event.kind === "score_active") {
    if (event.index <= lastIndex) {
      return null;
    }
    const outcome: UpdateOutcome = {
      outcome: "installed",
      index: event.index,
      source: event.source,
    };
    if (event.decodeError !== undefined) {
      outcome.decodeError = event.decodeError;
    }
    return outcome;
  }
  if (event.kind === "reload" && event.status === "rejected") {
    return {
      outcome: "rejected",
      reason: event.message ?? event.errorKind ?? "the score was rejected",
    };
  }
  if (
    event.kind === "narration" &&
    event.text.startsWith(UPDATE_FAILED_PREFIX)
  ) {
    const reason = event.text.slice(UPDATE_FAILED_PREFIX.length);
    const colon = reason.indexOf(": ");
    return {
      outcome: "rejected",
      reason:
        colon === -1 ? "the score was rejected" : reason.slice(colon + 2),
    };
  }
  return null;
}

/** Cap stored score text so one save cannot dominate the ring. */
function boundedSource(source: string): string {
  if (source.length <= STORED_SOURCE_LIMIT) {
    return source;
  }
  return `${source.slice(0, STORED_SOURCE_LIMIT)}…[truncated]`;
}

function boundedEvent(event: RustelEvent): RustelEvent {
  if (event.kind === "score_active") {
    return { ...event, source: boundedSource(event.source) };
  }
  return event;
}

/**
 * One live rustel set: the child process, the parsed stream, and the state a
 * host reads back. At most one child exists per instance; `stop` (or the
 * host's cleanup on session end) ends it before a new `start` may run.
 */
export class RustelProcess {
  private child: ChildProcess | null = null;
  private startedAtMs = 0;
  private readonly ring = new BoundedLog<RingEntry>(RING_LIMIT);
  private readonly errors = new BoundedLog<RecentError>(ERROR_LIMIT);
  private active: ScoreActiveEvent | null = null;
  private generation: number | null = null;
  private setPath: string | null = null;
  private scorePath: string | null = null;
  private binary: string | null = null;
  private exit: ExitInfo | null = null;
  /** Waiters for stream events; each is called with every new event and with
   * "exit" when the child dies, and removed once it returns true. */
  private waiters: Array<(event: RustelEvent | "exit") => boolean> = [];

  /** Called with every parsed stream event, after this instance's own state
   * is updated. Set before `start`. */
  onEvent: ((event: RustelEvent) => void) | null = null;
  /** Called once when the child exits, whatever ended it. */
  onExit: ((exit: ExitInfo) => void) | null = null;

  get running(): boolean {
    return this.child !== null && this.exit === null;
  }

  /**
   * Spawn the set and wait for its opening `score_active` line, so a caller
   * that gets a resolved promise knows the set is really playing. Throws
   * when the set folder holds no score, the binary cannot run, or the child
   * dies before the opening line.
   */
  async start(setPath: string, options: StartOptions = {}): Promise<void> {
    if (this.running) {
      throw new Error("this instance already runs a set; stop it before starting another");
    }
    this.scorePath = firstScoreOfSet(setPath);
    this.setPath = setPath;
    this.binary = options.binary ?? "rustel";
    this.ring.clear();
    this.errors.clear();
    this.active = null;
    this.generation = null;
    this.exit = null;

    const args = [
      "play",
      this.scorePath,
      "--watch",
      "--score-events",
      "--no-save-session",
    ];
    if (options.output !== undefined) {
      args.push("--output", options.output);
    }
    const child = spawn(this.binary, args, { stdio: ["ignore", "pipe", "pipe"] });
    this.child = child;
    this.startedAtMs = Date.now();
    // The protocol and the narration share stderr; stdout stays empty in
    // this mode but is drained so a full pipe could never block the child.
    child.stdout?.resume();
    const lines = createInterface({ input: child.stderr });
    lines.on("line", (line: string) => this.consume(line));
    // A late exit of a replaced child must not speak for the new one, so
    // both handlers check they are still about *this* child.
    child.once("exit", (code, signal) => {
      if (this.child === child) {
        this.noteExit(code, signal);
      }
    });
    child.once("error", () => {
      if (this.child === child) {
        this.noteExit(null, null);
      }
    });

    const settled = new Promise<"started" | ExitInfo>((resolve) => {
      child.once("error", () => resolve({ code: null, signal: null }));
      child.once("exit", (code, signal) => resolve({ code, signal }));
      this.waitForEvent(
        (event) =>
          event.kind === "score_active" ||
          (event.kind === "live" && event.status === "started"),
        options.readyTimeoutMs ?? DEFAULT_READY_TIMEOUT_MS,
      ).then((event) => resolve(event === null ? "timeout" : "started"));
    });
    const outcome = await settled;
    if (outcome !== "started") {
      const how =
        outcome === "timeout"
          ? `no score_events line arrived within ${
              options.readyTimeoutMs ?? DEFAULT_READY_TIMEOUT_MS
            } ms`
          : outcome.code === null && outcome.signal === null
            ? `the binary could not run: ${this.binary}`
            : `the set exited early (code ${outcome.code}, signal ${outcome.signal})`;
      const tail = this.ring
        .snapshot()
        .map((entry) => entry.event)
        .filter((event) => event.kind === "narration")
        .map((event) => (event as { text: string }).text)
        .slice(-8)
        .join("\n");
      await this.killAndWait();
      throw new Error(
        `the set did not start: ${how}${tail === "" ? "" : `\nstderr said:\n${tail}`}`,
      );
    }
  }

  /**
   * Write one save of the watched score and wait for the engine's verdict:
   * the lint gate installs the score, or refuses it and the last good score
   * keeps playing. The file is written either way, so a human editor sees
   * what the agent tried.
   */
  async update(source: string, waitMs = DEFAULT_UPDATE_TIMEOUT_MS): Promise<UpdateOutcome> {
    if (this.scorePath === null || !this.running) {
      throw new Error("no set is running; start one first");
    }
    const lastIndex = this.active?.index ?? 0;
    writeScoreFile(this.scorePath, source);
    const event = await this.waitForEvent(
      (candidate) => verdictFrom(candidate, lastIndex) !== null,
      waitMs,
    );
    if (event === null) {
      return {
        outcome: "unknown",
        note: this.exit !== null
          ? `the set exited before the verdict (code ${this.exit.code}, signal ${this.exit.signal})`
          : `no verdict arrived within ${waitMs} ms; call rustel_status to see the set`,
      };
    }
    const verdict = verdictFrom(event, lastIndex);
    return verdict === null
      ? { outcome: "unknown", note: "the verdict event could not be read" }
      : verdict;
  }

  /**
   * Stop the set. One SIGINT stops playback and lets the tail ring out; a
   * second SIGINT skips that wait; SIGKILL is the last resort
   * (crates/cli/src/main.rs, `INTERRUPT_COUNT`).
   */
  async stop(): Promise<StopOutcome> {
    const child = this.child;
    if (child === null) {
      return { outcome: "not-running", code: null, signal: null };
    }
    if (this.exit !== null) {
      const exit = this.exit;
      this.child = null;
      return { outcome: "already-exited", code: exit.code, signal: exit.signal };
    }
    child.kill("SIGINT");
    if (await this.waitForExit(STOP_GRACE_MS)) {
      this.child = null;
      return { outcome: "stopped", ...this.recordedExit() };
    }
    child.kill("SIGINT");
    if (await this.waitForExit(QUIT_GRACE_MS)) {
      this.child = null;
      return { outcome: "quit", ...this.recordedExit() };
    }
    await this.killAndWait();
    this.child = null;
    return { outcome: "killed", ...this.recordedExit() };
  }

  status(): RustelStatus {
    const active = this.active;
    return {
      running: this.running,
      pid: this.child?.pid ?? null,
      setPath: this.setPath,
      scorePath: this.scorePath,
      binary: this.binary,
      exit: this.exit,
      active:
        active === null
          ? null
          : {
              index: active.index,
              at: active.at,
              ...(active.of !== undefined ? { of: active.of } : {}),
              source: active.source,
              ...(active.decodeError !== undefined
                ? { decodeError: active.decodeError }
                : {}),
            },
      generation: this.generation,
      recentErrors: this.errors.snapshot(),
      events: this.ring.snapshot(),
    };
  }

  private recordedExit(): ExitInfo {
    return this.exit ?? { code: null, signal: null };
  }

  private consume(line: string): void {
    const event = parseEventLine(line);
    if (event === null) {
      return;
    }
    this.observe(event);
  }

  /** Update this instance's state from one parsed event and hand it to the
   * host callback. */
  private observe(event: RustelEvent): void {
    const ms = Date.now() - this.startedAtMs;
    this.ring.push({ ms, event: boundedEvent(event) });
    if (event.kind === "score_active") {
      this.active = event;
    } else if (event.kind === "live" && event.generation !== undefined) {
      this.generation = event.generation;
    } else if (event.kind === "reload") {
      this.generation = event.generationAfter;
    }
    if (this.isError(event)) {
      this.errors.push({
        ms,
        kind:
          event.kind === "live_error"
            ? event.errorKind
            : event.text.startsWith(UPDATE_FAILED_PREFIX)
              ? "refused-save"
              : "narration",
        message:
          event.kind === "live_error"
            ? event.message
            : event.text.startsWith("Error: ")
              ? event.text.slice("Error: ".length)
              : event.text.slice(UPDATE_FAILED_PREFIX.length),
      });
    }
    for (const waiter of [...this.waiters]) {
      if (waiter(event)) {
        this.waiters = this.waiters.filter((kept) => kept !== waiter);
      }
    }
    this.onEvent?.(event);
  }

  /** An error worth keeping for `rustel_status`. A `log` live_error is score
   * output, one line per sounding note (crates/cli/src/main.rs), not a
   * failure; warnings and waiting lines are not kept either. The refusal
   * narration is kept: it is the reason a save did not take. */
  private isError(event: RustelEvent): boolean {
    if (event.kind === "live_error") {
      return event.errorKind !== "log";
    }
    return (
      event.kind === "narration" &&
      (event.text.startsWith("Error: ") || event.text.startsWith(UPDATE_FAILED_PREFIX))
    );
  }

  private waitForEvent(
    predicate: (event: RustelEvent) => boolean,
    timeoutMs: number,
  ): Promise<RustelEvent | null> {
    return new Promise((resolve) => {
      const waiter = (event: RustelEvent | "exit"): boolean => {
        if (event === "exit") {
          finish(null);
          return true;
        }
        if (predicate(event)) {
          finish(event);
          return true;
        }
        return false;
      };
      const timer = setTimeout(() => {
        this.waiters = this.waiters.filter((kept) => kept !== waiter);
        finish(null);
      }, timeoutMs);
      const finish = (result: RustelEvent | null): void => {
        clearTimeout(timer);
        resolve(result);
      };
      this.waiters.push(waiter);
    });
  }

  /** True when the child exited within the grace, false on timeout. */
  private async waitForExit(graceMs: number): Promise<boolean> {
    if (this.exit !== null) {
      return true;
    }
    return await new Promise<boolean>((resolve) => {
      const timer = setTimeout(() => {
        this.waiters = this.waiters.filter((kept) => kept !== waiter);
        resolve(false);
      }, graceMs);
      const waiter = (event: RustelEvent | "exit"): boolean => {
        if (event === "exit") {
          clearTimeout(timer);
          resolve(true);
          return true;
        }
        return false;
      };
      this.waiters.push(waiter);
    });
  }

  /** SIGKILL and wait for the exit record; used only after the two SIGINTs. */
  private async killAndWait(): Promise<void> {
    this.child?.kill("SIGKILL");
    await this.waitForExit(KILL_GRACE_MS);
  }

  /** Record one child exit, release every waiter, and tell the host. */
  private noteExit(code: number | null, signal: string | null): void {
    this.exit = { code, signal };
    for (const waiter of [...this.waiters]) {
      waiter("exit");
    }
    this.waiters = [];
    this.onExit?.(this.exit);
  }
}
