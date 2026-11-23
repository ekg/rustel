# Does the language reference change what the model writes?

Measured 2026-11-23 against the Lunarroute gateway
(`https://gw.lunaroute.com/v1`, model `deepseek-4.1-flash`, the 2048-token
reply budget the pod sends). The question is the one the `llm context` setting
asks: the llm pod can send the model the instructions alone (`off`), the
one-line language reference (`compact`) or the full reference (`full`). Does
the reference change the answer, and by how much does it cost?

The harness is `crates/pod-llm/tests/reference_ab.rs`. It sends the five
prompts below once per condition through the pod's own `Endpoint` and the real
`system_prompt`, and scores every reply mechanically: the runtime's lint (no
syntax finding, so the reply parses as a score) and a name scan (every
function-like name the reply calls exists in the one-line reference - one
yardstick for all three arms, including the arm that was sent no reference at
all). The product's own `rustel check` - the lint plus a dry evaluation - is
the second pass over the same files, and it is the one that decides the last
two columns. A reader adds the judgement column: does the reply do what the
request asked.

## The prompts

Each prompt is a starting score plus a change, worded the way the pod's user
message words them ("The current score: ... The change: ..."). They are chosen
to need names a model would have to guess: a chord voicing from the engine's
own dictionary, a less common control (sidechain ducking), mini-notation, a
synth voice from the sound catalogue, and one plain beat where the reference
should make no difference.

```ab-prompts
--- p1-chord-voicing
$: note("c4 e4 g4 b4").s("triangle").gain(0.6)
+++ Voice these four notes as a Cmaj7 with the engine's own voicing dictionary instead of spelling them out, and put the top note at c5.
--- p2-sidechain
$: s("bd*4").orbit(1)
$: note("<c3 eb3 g3>").s("sawtooth").orbit(2)
+++ Make the bass duck under the kick by 0.7, reaching its quietest point 0.02 seconds after the kick and recovering over 0.2 seconds.
--- p3-euclid-mini
$: s("bd ~ bd ~").gain(0.8)
+++ Give the kick a Euclidean rhythm of 5 hits in 8 steps rotated by 2, and double the hats to sixteenths with every fourth one open.
--- p4-synth-voice
$: note("<c2 c2 eb2 g2>").s("piano").gain(0.7)
+++ Swap the piano sample for one of the engine's own synth voices, and open a resonant low-pass filter on each note whose envelope sweeps the cutoff up two octaves.
--- p5-plain-beat
$: s("bd sd bd sd").gain(0.8)
+++ Make this a basic four-on-the-floor beat: a kick on every quarter, a snare on the backbeat, and eighth-note hats.
```

## What each condition costs

The system message is the pod's own: `STRUDEL_SYSTEM_PROMPT`, then the sheet
`rustel reference` renders. Input tokens are the gateway's own count for the
whole request (system plus the p1 user turn), read from the response's
`usage.prompt_tokens`.

| Condition | System message | Input tokens | Bytes per token |
| --- | --- | --- | --- |
| `off` | 551 B | 224 | 2.5 (the instructions are prose) |
| `compact` | 65,092 B | 18,630 | 3.49 |
| `full` | 127,242 B | 35,352 | 3.60 |

So the full reference costs 1.9 times the compact one, and compact costs 83
times the instructions alone. The sheet is denser than four bytes a token -
3.5 - because it is mostly names and punctuation.

Those are the sizes of the pod crate's own build, where the runtime is linked
without its `extensions` and `hydra` features. The complete product build's
sheet adds those tables, and its message is the same 551-byte instruction
block plus the same two-byte separators and size header:

| Condition | Sheet | Assembled message | Tokens at the ratio above |
| --- | --- | --- | --- |
| `off` | - | 551 B | about 0.2k |
| `compact` | 67,951 B | 68,616 B | about 20k |
| `full` | 140,021 B | 140,703 B | about 39k |

That is what the studio's `llm context` row and `docs/studio.md` quote. No
prompt below names an extension or a Hydra control, so the shape of the result
holds for both builds.

The prefix does cache. One extra pair of identical calls with the compact
message reported `cached_tokens` 0 on the first and 19,328 of 19,357 input
tokens on the second: a provider that caches the prefix bills a repeated
reference at a fraction of the uncached price, which is what the pod's
"system message is a function of the build" rule buys.

## Latency

Seconds per reply, in prompt order, and the mean.

| Condition | p1 | p2 | p3 | p4 | p5 | Mean |
| --- | --- | --- | --- | --- | --- | --- |
| `off` | 13.5 | 4.7 | 10.1 | 3.0 | 4.7 | 7.2 |
| `compact` | 2.6 | 4.2 | 26.0 | 5.7 | 5.1 | 8.7 |
| `full` | 4.1 | 4.4 | 6.5 | 1.6 | 5.1 | 4.3 |

No measurable penalty from the larger prompt: the spread inside each row is
model-side variance (13.5 s and 26.0 s outliers), which at five samples a
condition is larger than any difference between the rows.

## Results

`parses` is the harness's static lint (no syntax finding). `check` is the
product's `rustel check` over the written reply: it lints and then evaluates
once, so it also catches an argument the API does not take. `unknown` lists
called names the reference does not carry.

| Prompt | Condition | parses | check | unknown names | How the reply reads |
| --- | --- | --- | --- | --- | --- |
| p1 chord voicing | `off` | yes | **fails**: `.voicing()` takes no inputs, chord `Cmaj7` unknown | none | Invented `voicing({top:"c5"})`; right idea, an API that does not exist |
| p1 | `compact` | yes | **fails**: chord `Cmaj7` unknown to the dictionary | none | Right chain (`.voicing().anchor("c5")`), wrong chord spelling |
| p1 | `full` | yes | passes | none | `chord("C^7").voicing().anchor("c5")` - the symbol from the chord catalogue |
| p2 sidechain | `off` | yes | passes | none | Invented a Tidal-style string: `.duck("2:0.7:0.02:0.2")`; four numbers in one undocumented argument, so nothing proves what it does |
| p2 | `compact` | yes | **fails**: unknown function `duckrelease` | `duckrelease` | Wrote the controls on the bass and ducked the kick; guessed a release that does not exist |
| p2 | `full` | yes | passes | none | `.duckorbit(2).duckdepth(0.7).duckonset(0.02).duckattack(0.2)` - the two parameter descriptions put the quietest point and the recovery in the right controls |
| p3 euclid + hats | `off` | yes | passes | none | `s("bd(5,8,2)")` in mini-notation, `[hh hh hh oh]*4` - exactly the request |
| p3 | `compact` | yes | passes | none | `euclidRot(5, 8, 2)` and the same hats - the function form, also right |
| p3 | `full` | yes | passes | none | `euclidRot(5, 8, 2)`, same hats - right |
| p4 synth voice | `off` | yes | passes | none | `s("sawtooth").lpf(400).lpenv(2).lpq(12)` - a synth voice, resonant, envelope up |
| p4 | `compact` | yes | passes | none | Same, plus a fuller filter envelope (`lpa`/`lpd`/`lps`/`lpr`) |
| p4 | `full` | yes | passes | none | Same, spelled `lpattack`/`lpdecay`/`lpsustain`/`lprelease` |
| p5 plain beat | `off` | yes | passes | none | `bd*4`, `~ sd ~ sd`, `hh*8` - right |
| p5 | `compact` | yes | passes | none | Identical in substance |
| p5 | `full` | yes | passes | none | Identical in substance (`bd bd bd bd`) |

Totals: 15 of 15 replies parse. The name scan catches one reply (`compact`,
p2). The engine's own check passes every `full` reply and the `off` run's four
non-voicing replies, so by that measure `full` is 5/5, `off` 4/5 and `compact`
3/5.

## What the numbers say

The reference did not change whether the model wrote a valid score. All
fifteen replies are syntactically valid Rustel and twelve of them survive the
engine's dry evaluation, including four of the five replies from the
instructions-only run. On these prompts the reference is not what makes a
reply a score.

It changed the two things it is the only source for:

- **spellings from a catalogue.** The chord catalogue is what offers
  `chord("C^7")`, the symbol the voicing dictionary knows; the two runs that
  wrote `Cmaj7` had nothing to read it from, or did not read it. Both sizes
  carry the catalogue (it is the same word list in each), so this is a win of
  the reference over none rather than of `full` over `compact` - and one of the
  two runs that had it still spelled the chord wrong.
- **what a parameter means.** `duckonset` and `duckattack` are
  counter-intuitive - onset is the time to reach the quietest point, attack is
  the time to come back - and only `full` carries the descriptions that say
  so. `off` guessed a string packing all four numbers, `compact` guessed a
  `duckrelease` that does not exist. A one-line-per-name sheet names the
  family but not the members' meaning.

Where the reference bought nothing: Euclidean rhythm, filter controls, synth
voice names and the plain beat were all answered correctly without it, and the
answers are near-identical across the three conditions. `sawtooth`, `euclidRot`
and `lpf`/`lpenv` were guessed as readily as they were read.

`compact` was not better than `off` here, and on p2 it was worse: given a list
of duck names it produced a plausible one that the engine does not install,
while the run with no list at all invented a shape that at least the checker
accepts. That is five prompts, one sample each - not a rate - but it is the
honest reading of this pair.

The function-name scan turned out to be a weak discriminator: it flags
1 reply of 15, while 4 replies of 15 are wrong in ways no name list can see
(invented argument shapes, ducking the wrong way round, an unknown chord
symbol). The engine's own `check` over the written score is the score worth
quoting, which is why both are in the table.

## Re-running it

```sh
RUSTEL_LLM_KEY=<key> cargo test -p rustel-pod-llm --test reference_ab -- \
    --ignored --nocapture
for reply in /tmp/rustel-reference-ab/*.strudel; do
  printf '%s: ' "$reply"; rustel check "$reply" || true
done
```

Without `RUSTEL_LLM_KEY` the harness reads the pi agent's own store
(`$HOME/.pi/agent/auth.json`) and takes its `lunaroute.access` token; it prints
neither the key nor the store's contents. Replies land in `$RUSTEL_LLM_AB_DIR`
(default `<temp>/rustel-reference-ab`) as `<condition>--<prompt>.strudel`. The
model, the endpoint and the three conditions are constants at the top of the
harness, and the prompts are parsed from this document's `ab-prompts` block, so
the prompts quoted here are the prompts sent. The harness's two offline tests
and its scoring are part of `cargo test -p rustel-pod-llm`.

## What this does not measure

- One reply per cell, with no temperature set (the pod sends none, so the
  provider's default applies). A five-prompt, single-sample A/B shows the
  direction and the failure modes; it does not estimate a success rate.
- The prompts were written after reading the sheet, which biases them toward
  names the sheet lists. A set of prompts written by someone who has not read
  it could rank the sizes differently.
- "How the reply reads" is one person reading fifteen short replies against
  the request. The parse, name and `check` columns are mechanical; that one is
  not.
- Nothing here measures whether the scores sound right. The pod's own e2e
  tests cover the request shape; this document covers the content of the
  answer.
