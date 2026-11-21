/**
 * pi-rustel: a pi extension that gives agents tools to live-code Rustel.
 *
 * This file is the thin pi-facing layer: it registers the four `rustel_*`
 * tools, turns the core's stream events into session context messages, and
 * stops the child when the session ends. The spawn/parse/lifecycle core in
 * ./core/ has no pi imports, so another host could lift it unchanged.
 *
 * Install: copy or link this folder into `~/.pi/agent/extensions/`, or run
 * `pi -e pi-rustel/src/index.ts` for a quick test.
 */

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { resolve } from "node:path";
import {
  RustelProcess,
  type RustelStatus,
  type StopOutcome,
  type UpdateOutcome,
} from "./core/rustel.ts";
import type { RustelEvent } from "./core/events.ts";

/** Messages pushed into the session context are coalesced to at most one
 * per this many milliseconds, so a chatty set cannot flood the context. */
const FEED_MIN_INTERVAL_MS = 750;
/** How many pending feed lines may wait for the next send window. */
const FEED_QUEUE_LIMIT = 32;
/** Score text in a context message is cut here, with a marker. */
const FEED_SOURCE_LIMIT = 2_000;

/**
 * Bounded, rate-limited delivery of rustel events to the session.
 *
 * Lines offered while a send window is open wait for the next one; a window
 * sends everything it holds as one message. Overflow drops the oldest line
 * and says how many went.
 */
class SessionFeed {
  private readonly send: (text: string) => void;
  private pending: string[] = [];
  private dropped = 0;
  private lastSendMs = 0;
  private timer: ReturnType<typeof setTimeout> | null = null;

  constructor(send: (text: string) => void) {
    this.send = send;
  }

  offer(line: string): void {
    if (this.pending.length >= FEED_QUEUE_LIMIT) {
      this.pending.shift();
      this.dropped += 1;
    }
    this.pending.push(line);
    this.deliverWhenAllowed();
  }

  /** Stop holding lines; whatever waits is sent now. */
  flush(): void {
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    this.lastSendMs = 0;
    this.deliver();
  }

  private deliverWhenAllowed(): void {
    const wait = FEED_MIN_INTERVAL_MS - (Date.now() - this.lastSendMs);
    if (wait <= 0) {
      if (this.timer !== null) {
        clearTimeout(this.timer);
        this.timer = null;
      }
      this.deliver();
    } else if (this.timer === null) {
      this.timer = setTimeout(() => {
        this.timer = null;
        this.deliver();
      }, wait);
    }
  }

  private deliver(): void {
    if (this.pending.length === 0) {
      return;
    }
    const lines = this.pending;
    this.pending = [];
    const dropped = this.dropped;
    this.dropped = 0;
    this.lastSendMs = Date.now();
    const note =
      dropped > 0 ? `\n(${dropped} earlier event${dropped === 1 ? "" : "s"} dropped)` : "";
    this.send(`${lines.join("\n")}${note}`);
  }
}

/** Turn the event stream into the lines the session should hear. Only the
 * events an agent can act on pass: installed saves, refused saves, and the
 * set ending. Narration stays in `rustel_status`'s ring. */
function feedLines(
  event: RustelEvent,
  activeIndexAtStart: number,
): string[] {
  if (event.kind === "score_active" && event.index > activeIndexAtStart) {
    const source =
      event.source.length > FEED_SOURCE_LIMIT
        ? `${event.source.slice(0, FEED_SOURCE_LIMIT)}…[truncated]`
        : event.source;
    return [
      `rustel: save ${event.index} installed and playing:\n${source}`,
    ];
  }
  if (event.kind === "narration" && event.text.startsWith("Update failed for ")) {
    const rest = event.text.slice("Update failed for ".length);
    const colon = rest.indexOf(": ");
    const reason = colon === -1 ? "the score was rejected" : rest.slice(colon + 2);
    return [
      `rustel: a save was refused and the last good score keeps playing - ${reason}`,
    ];
  }
  if (event.kind === "reload" && event.status === "rejected") {
    return [
      `rustel: a save was refused and the last good score keeps playing - ${
        event.message ?? event.errorKind ?? "the score was rejected"
      }`,
    ];
  }
  return [];
}

function bounded(text: string, limit: number): string {
  return text.length <= limit ? text : `${text.slice(0, limit)}…[truncated]`;
}

function renderStatus(status: RustelStatus): string {
  const lines: string[] = [];
  if (status.running) {
    lines.push(`running: yes (pid ${status.pid})`);
  } else if (status.exit !== null) {
    lines.push(
      `running: no (exited with code ${status.exit.code}, signal ${status.exit.signal})`,
    );
  } else {
    lines.push("running: no (no set was started)");
  }
  lines.push(`set: ${status.setPath ?? "-"}`);
  lines.push(`score: ${status.scorePath ?? "-"}`);
  lines.push(`binary: ${status.binary ?? "-"}`);
  lines.push(
    `generation: ${
      status.generation === null
        ? "- (the stream these flags produce does not carry one)"
        : status.generation
    }`,
  );
  if (status.active !== null) {
    lines.push(
      `save ${status.active.index}${
        status.active.of !== undefined ? ` of ${status.active.of}` : ""
      } is sounding (at ${status.active.at}s):`,
    );
    lines.push(status.active.decodeError ?? bounded(status.active.source, 8_000));
  } else {
    lines.push("save: none has been announced yet");
  }
  const errors = status.recentErrors;
  if (errors.length > 0) {
    lines.push("recent errors:");
    for (const error of errors) {
      lines.push(`  - [${error.kind}] ${error.message}`);
    }
  }
  lines.push(`recent events (${status.events.length} held):`);
  for (const entry of status.events) {
    lines.push(`  [${(entry.ms / 1000).toFixed(1)}s] ${describeEvent(entry.event)}`);
  }
  return lines.join("\n");
}

function describeEvent(event: RustelEvent): string {
  switch (event.kind) {
    case "score_active":
      return `score_active: save ${event.index}${
        event.of !== undefined ? ` of ${event.of}` : ""
      } (${event.lines ?? event.source.split("\n").filter((line) => line.trim() !== "").length} lines)`;
    case "live_error":
      return `live_error: [${event.errorKind}] ${event.message}`;
    case "reload":
      return `reload: ${event.status} ${event.path}${
        event.message !== undefined ? ` - ${event.message}` : ""
      }`;
    case "live":
      return `live: ${event.status}`;
    case "narration":
      return `narration: ${event.text}`;
  }
}

function renderUpdate(outcome: UpdateOutcome): string {
  switch (outcome.outcome) {
    case "installed":
      return `save ${outcome.index} is playing:\n${outcome.source}`;
    case "rejected":
      return `the score was refused and the last good score keeps playing - ${outcome.reason}`;
    case "unknown":
      return `written; ${outcome.note}`;
  }
}

function renderStop(outcome: StopOutcome): string {
  switch (outcome.outcome) {
    case "not-running":
      return "no set was running";
    case "already-exited":
      return `the set had already exited (code ${outcome.code}, signal ${outcome.signal})`;
    case "stopped":
      return `stopped by one SIGINT (code ${outcome.code}, signal ${outcome.signal})`;
    case "quit":
      return `stopped by a second SIGINT (code ${outcome.code}, signal ${outcome.signal})`;
    case "killed":
      return `would not stop; killed (code ${outcome.code}, signal ${outcome.signal})`;
  }
}

/** Tool parameter schemas as plain JSON Schema. Pi validates tool arguments
 * against JSON Schema whatever built it, so the extension needs no schema
 * package of its own. */
const START_PARAMS = {
  type: "object",
  properties: {
    set: {
      type: "string",
      description:
        "Path of the set folder to play; its .strudel files are the set's scenes and the first one is played",
    },
    binary: {
      type: "string",
      description: "rustel binary to run; default `rustel` from PATH",
    },
    output: {
      type: "string",
      description:
        "Audio output device name, or `silent` to run with no audio hardware; omit for the default device",
    },
  },
  required: ["set"],
  additionalProperties: false,
} as const;

const UPDATE_PARAMS = {
  type: "object",
  properties: {
    source: {
      type: "string",
      description:
        "The full score source to write; a broken score is refused by the engine's lint gate and the last good score keeps playing",
    },
  },
  required: ["source"],
  additionalProperties: false,
} as const;

const NO_PARAMS = {
  type: "object",
  properties: {},
  additionalProperties: false,
} as const;

/** Strip the @ prefix some models add to path arguments, and resolve against
 * the session's working directory. */
function resolveSetPath(cwd: string | undefined, rawSet: string): string {
  const trimmed = rawSet.startsWith("@") ? rawSet.slice(1) : rawSet;
  return resolve(cwd ?? process.cwd(), trimmed);
}

export default function rustelExtension(pi: ExtensionAPI): void {
  // One rustel child per extension instance at most. The factory starts
  // nothing; the child exists only between rustel_start and its stop, or
  // the session's end.
  let live: RustelProcess | null = null;
  let feed: SessionFeed | null = null;
  let uiNotify: ((message: string) => void) | null = null;
  let openingIndex = 1;

  const pushToSession = (text: string): void => {
    // A context message with no triggered turn: while the agent runs it is
    // delivered after the current turn, and while idle it waits in the
    // session for the next turn. Either way the agent hears what happened
    // without being woken, and rustel_status says the same on demand.
    pi.sendMessage(
      { customType: "rustel-event", content: text, display: true },
      { triggerTurn: false },
    );
    try {
      uiNotify?.(text);
    } catch {
      // A dead session UI is not the set's problem.
    }
  };

  const startFeed = (): void => {
    feed = new SessionFeed(pushToSession);
  };

  const ensureNotRunning = (): { isError: true; text: string } | null => {
    if (live !== null && live.running) {
      return {
        isError: true,
        text: `a set is already running; use rustel_status to see it and rustel_stop before starting another`,
      };
    }
    return null;
  };

  pi.on("session_start", (_event, ctx) => {
    uiNotify = ctx.hasUI
      ? (message: string) => ctx.ui.notify(message, "info")
      : null;
  });

  // Idempotent: stop() reports "not-running" when there is nothing to stop.
  pi.on("session_shutdown", async () => {
    await live?.stop();
    feed?.flush();
    feed = null;
    uiNotify = null;
  });

  pi.registerTool({
    name: "rustel_start",
    label: "Play a Rustel set",
    description:
      "Play a Rustel set live: spawns `rustel play <first score of the set> --watch --score-events` so later rustel_update calls reload the sounding score without restarting audio. Resolves when the opening score is sounding.",
    promptSnippet:
      "Play a Rustel set live and follow score edits with rustel_update",
    promptGuidelines: [
      "Use rustel_start to play a Rustel set folder, then rustel_update to rewrite the sounding score; do not spawn rustel through bash when these tools are available.",
    ],
    parameters: START_PARAMS,
    async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
      const busy = ensureNotRunning();
      if (busy !== null) {
        return {
          content: [{ type: "text", text: busy.text }],
          details: { outcome: "already-running" },
          isError: true,
        };
      }
      const setPath = resolveSetPath(ctx?.cwd, params.set);
      const set = new RustelProcess();
      try {
        await set.start(setPath, {
          binary: params.binary,
          output: params.output,
        });
      } catch (error) {
        return {
          content: [{ type: "text", text: String(error) }],
          details: { outcome: "start-failed" },
          isError: true,
        };
      }
      live = set;
      startFeed();
      openingIndex = set.status().active?.index ?? 1;
      set.onEvent = (event) => {
        for (const line of feedLines(event, openingIndex)) {
          feed?.offer(line);
        }
      };
      set.onExit = (exit) => {
        feed?.offer(
          `rustel: the set ended (code ${exit.code}, signal ${exit.signal})`,
        );
        feed?.flush();
      };
      const status = set.status();
      const first =
        status.active?.decodeError === undefined
          ? `save ${status.active?.index} is sounding:\n${status.active?.source}`
          : `save ${status.active?.index} is sounding, but its source could not be decoded (${status.active?.decodeError})`;
      return {
        content: [
          {
            type: "text",
            text: `playing ${status.scorePath} from set ${status.setPath} (pid ${status.pid}).\n${first}`,
          },
        ],
        details: { status },
      };
    },
  });

  pi.registerTool({
    name: "rustel_update",
    label: "Rewrite the sounding score",
    description:
      "Write a new score to the watched file. The engine lints it: an installed save becomes the sounding score, a broken one is refused and the last good score keeps playing. Returns the verdict.",
    promptSnippet: "Rewrite the sounding Rustel score and report the verdict",
    promptGuidelines: [
      "Use rustel_update with the complete new score source; a broken score is refused and the previous score keeps playing, so fix the reported reason and try again.",
    ],
    parameters: UPDATE_PARAMS,
    async execute(_toolCallId, params) {
      if (live === null || !live.running) {
        return {
          content: [{ type: "text", text: "no set is running; call rustel_start first" }],
          details: { outcome: "not-running" },
          isError: true,
        };
      }
      const outcome = await live.update(params.source);
      return {
        content: [{ type: "text", text: renderUpdate(outcome) }],
        details: { outcome },
        isError: outcome.outcome === "rejected",
      };
    },
  });

  pi.registerTool({
    name: "rustel_stop",
    label: "Stop the Rustel set",
    description:
      "Stop the live set: one SIGINT stops playback and lets the tail ring out, a second SIGINT skips the wait, and the child is killed only if it ignores both.",
    promptSnippet: "Stop the live Rustel set started by rustel_start",
    promptGuidelines: [
      "Use rustel_stop when the set should end; it also runs on session shutdown, so a set never outlives the session.",
    ],
    parameters: NO_PARAMS,
    async execute() {
      const outcome = live === null ? null : await live.stop();
      feed?.flush();
      return {
        content: [
          {
            type: "text",
            text:
              outcome === null
                ? "no set was running"
                : renderStop(outcome),
          },
        ],
        details: { outcome },
      };
    },
  });

  pi.registerTool({
    name: "rustel_status",
    label: "Report the live Rustel set",
    description:
      "Report the live set: whether it runs, the sounding score's decoded source and save index, recent errors, and a bounded ring of recent stream events.",
    promptSnippet: "Report the live Rustel set and its recent events",
    promptGuidelines: [
      "Use rustel_status to check the set instead of polling files or processes.",
    ],
    parameters: NO_PARAMS,
    async execute() {
      const status = live === null ? null : live.status();
      return {
        content: [
          {
            type: "text",
            text:
              status === null
                ? "no set was ever started in this session"
                : renderStatus(status),
          },
        ],
        details: { status },
      };
    },
  });
}
