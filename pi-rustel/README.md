# pi-rustel

A [pi](https://github.com/earendil-works/pi-coding-agent) extension that
gives agents tools to live-code [Rustel](../README.md): play a set, rewrite
the sounding score, and hear what the engine did with each save.

The extension spawns `rustel play <first score of the set> --watch
--score-events --no-save-session` and parses the child's stderr. The
`--watch` flag makes every save of the score file reload without restarting
the audio stream; the engine's lint gate installs a good save and refuses a
broken one, in which case the last good score keeps playing.

## Install

Copy or link this folder into `~/.pi/agent/extensions/` (or a project's
`.pi/extensions/`), or point pi at the entry for a quick test:

```sh
pi -e pi-rustel/src/index.ts
```

The `rustel` binary must be on `PATH`, or pass `binary` to `rustel_start`
(for example `~/.cargo/bin/rustel`). The extension has no npm dependencies.

## Tools

| Tool | What it does |
| --- | --- |
| `rustel_start` | Play a set folder. Its `.strudel` files are the set's scenes, in the order the studio opens them; the first one is played and watched. Resolves when the opening score is sounding. Options: `binary` (default `rustel` from `PATH`), `output` (a device name, or `silent` to run with no audio hardware). |
| `rustel_update` | Write the given score text to the watched file and report the engine's verdict: the save's index and decoded source when it installed, or the refusal reason when the lint gate rejected it (the last good score keeps playing either way). |
| `rustel_stop` | Stop the set. One SIGINT stops playback and lets the tail ring out; a second SIGINT skips the wait; the child is killed only if it ignores both. |
| `rustel_status` | Report the set: alive state, the last `score_active` (decoded source text), the current save index and engine generation, recent errors, and a bounded ring of recent stream events. |

## What the session hears

The child's stream is pushed into the session as context: installed saves
(with the decoded score text), refused saves (with the engine's reason) and
the set ending. Messages are coalesced to at most one per 750 ms, queued
entries are bounded, and long score text is truncated.

The push uses `pi.sendMessage` with `triggerTurn: false`: while the agent
runs, a message is delivered after the current turn; while the session is
idle it waits in the session for the next turn. The agent hears what
happened without being woken, and `rustel_status` tells the same on demand.
A message that should start a turn is the user's call, not the set's.

## Process hygiene

At most one rustel child exists per extension instance, and starting a
second set is refused while one runs. The four tools are registered with
pi's sequential execution mode, so calls in one assistant message run one
at a time and cannot race the single child. The child is stopped by the
`session_shutdown` handler when the session ends, so a set never outlives
the session (a start whose session ends before the opening score arrives
stops itself). Every buffer the extension keeps over the stream is bounded.

## Design

`src/core/` is the spawn/parse/lifecycle core with no pi imports, so another
host (an MCP server, say) could lift it unchanged; `src/index.ts` is the
thin pi-facing layer that adapts it to tool registrations. The parser
mirrors the CLI's own emitters: `score_active` (`crates/cli/src/replay.rs`,
`announce_active_score`), the `live`, `reload` and `live_error` JSON events
(`crates/cli/src/live.rs`), and the human narration (`human_live_event` in
`crates/cli/src/main.rs`) that shares the stderr stream with them.

Not built: an MCP surface for the same core.

## Development

```sh
npm test        # node --test: parser and core unit tests, load smoke,
                # and an integration test against the real rustel binary
```

The integration test runs the installed `rustel` binary (found via
`RUSTEL_BIN`, `PATH`, or `~/.cargo/bin/rustel`) against a temporary set with
`--output silent`, and skips itself when no binary is present.
