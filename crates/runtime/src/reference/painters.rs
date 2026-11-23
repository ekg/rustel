//! The painters' reference entries, in the order the studio's renderer
//! dispatches them.
//!
//! The painters are documented where a score meets them, beside the engine
//! surface rather than the terminal that draws them: upstream's words where
//! the painter mirrors strudel.cc (Documentation text from the Strudel
//! project (AGPL-3.0-or-later), https://strudel.cc). The studio's own
//! divergences - what its terminal does with each option - stay in its
//! `visuals` module, because they describe that rendering and not the
//! painter's contract.

use rustel_core::reference::{ReferenceEntry, ReferenceParam};

/// The painters' entries, in the order the renderer dispatches them.
pub const PAINTER_REFERENCE_ENTRIES: &[ReferenceEntry] = &[
    ReferenceEntry {
        name: "pianoroll",
        synonyms: &[],
        summary: "Visualises a pattern as a scrolling 'pianoroll', displayed in the background of the editor.",
        description: "Visualises a pattern as a scrolling 'pianoroll', displayed in the background of the editor. To show a pianoroll for all running patterns, use `all(pianoroll)`. To have a pianoroll appear below\na pattern instead, prefix with `_`, e.g.: `sound(\"bd sd\")._pianoroll()`.",
        params: &[
            ReferenceParam {
                name: "options",
                r#type: "Object",
                description: "Object containing all the optional following parameters as key value pairs:",
            },
            ReferenceParam {
                name: "cycles",
                r#type: "integer",
                description: "number of cycles to be displayed at the same time - defaults to 4",
            },
            ReferenceParam {
                name: "playhead",
                r#type: "number",
                description: "location of the active notes on the time axis - 0 to 1, defaults to 0.5",
            },
            ReferenceParam {
                name: "vertical",
                r#type: "boolean",
                description: "displays the roll vertically - 0 by default",
            },
            ReferenceParam {
                name: "labels",
                r#type: "boolean",
                description: "displays labels on individual notes (see the label function) - 0 by default",
            },
            ReferenceParam {
                name: "flipTime",
                r#type: "boolean",
                description: "reverse the direction of the roll - 0 by default",
            },
            ReferenceParam {
                name: "flipValues",
                r#type: "boolean",
                description: "reverse the relative location of notes on the value axis - 0 by default",
            },
            ReferenceParam {
                name: "overscan",
                r#type: "number",
                description: "lookup X cycles outside of the cycles window to display notes in advance - 1 by default",
            },
            ReferenceParam {
                name: "hideNegative",
                r#type: "boolean",
                description: "hide notes with negative time (before starting playing the pattern) - 0 by default",
            },
            ReferenceParam {
                name: "smear",
                r#type: "boolean",
                description: "notes leave a solid trace - 0 by default",
            },
            ReferenceParam {
                name: "fold",
                r#type: "boolean",
                description: "notes takes the full value axis width - 0 by default",
            },
            ReferenceParam {
                name: "active",
                r#type: "string",
                description: "hexadecimal or CSS color of the active notes - defaults to #FFCA28",
            },
            ReferenceParam {
                name: "inactive",
                r#type: "string",
                description: "hexadecimal or CSS color of the inactive notes - defaults to #7491D2",
            },
            ReferenceParam {
                name: "background",
                r#type: "string",
                description: "hexadecimal or CSS color of the background - defaults to transparent",
            },
            ReferenceParam {
                name: "playheadColor",
                r#type: "string",
                description: "hexadecimal or CSS color of the line representing the play head - defaults to white",
            },
            ReferenceParam {
                name: "fill",
                r#type: "boolean",
                description: "notes are filled with color (otherwise only the label is displayed) - 0 by default",
            },
            ReferenceParam {
                name: "fillActive",
                r#type: "boolean",
                description: "active notes are filled with color - 0 by default",
            },
            ReferenceParam {
                name: "stroke",
                r#type: "boolean",
                description: "notes are shown with colored borders - 0 by default",
            },
            ReferenceParam {
                name: "strokeActive",
                r#type: "boolean",
                description: "active notes are shown with colored borders - 0 by default",
            },
            ReferenceParam {
                name: "hideInactive",
                r#type: "boolean",
                description: "only active notes are shown - 0 by default",
            },
            ReferenceParam {
                name: "colorizeInactive",
                r#type: "boolean",
                description: "use note color for inactive notes - 1 by default",
            },
            ReferenceParam {
                name: "fontFamily",
                r#type: "string",
                description: "define the font used by notes labels - defaults to 'monospace'",
            },
            ReferenceParam {
                name: "minMidi",
                r#type: "integer",
                description: "minimum note value to display on the value axis - defaults to 10",
            },
            ReferenceParam {
                name: "maxMidi",
                r#type: "integer",
                description: "maximum note value to display on the value axis - defaults to 90",
            },
            ReferenceParam {
                name: "autorange",
                r#type: "boolean",
                description: "automatically calculate the minMidi and maxMidi parameters - 0 by default",
            },
        ],
        examples: &[
            "note(\"c2 a2 eb2\")\n.euclid(5,8)\n.s('sawtooth')\n.lpenv(4).lpf(300)\n.pianoroll({ labels: 1 })",
        ],
        tags: &["visualization"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "punchcard",
        synonyms: &[],
        summary: "A pianoroll fed by the highlighter's events: the whole stack for `punchcard()`, its own pattern for `_punchcard()`.",
        description: "Visualises a pattern as a scrolling 'pianoroll', displayed in the background of the editor. To show a pianoroll for all running patterns, use `all(pianoroll)`. To have a pianoroll appear below\na pattern instead, prefix with `_`, e.g.: `sound(\"bd sd\")._pianoroll()`.\n\nHere and everywhere else this language plays, `pianoroll` and `punchcard` are one renderer fed differently: a plain `punchcard()` shows every event the highlighter sees, the whole stack, while `_punchcard()` is tagged to its own pattern like `_pianoroll()`.",
        params: &[ReferenceParam {
            name: "options",
            r#type: "Object",
            description: "the same options as pianoroll",
        }],
        examples: &[
            "note(\"c a f e\").color(\"white\").punchcard()",
            "note(\"c a f e\").color(\"white\")\n.punchcard()\n.color(\"cyan\")",
        ],
        tags: &["visualization"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "wordfall",
        synonyms: &[],
        summary: "Displays a vertical pianoroll with event labels.",
        description: "Displays a vertical pianoroll with event labels.\nSupports all the same options as pianoroll.",
        params: &[],
        examples: &[],
        tags: &["visualization"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "spiral",
        synonyms: &[],
        summary: "Displays a spiral visual.",
        description: "Displays a spiral visual.",
        params: &[
            ReferenceParam {
                name: "options",
                r#type: "Object",
                description: "Object containing all the optional following parameters as key value pairs:",
            },
            ReferenceParam {
                name: "stretch",
                r#type: "number",
                description: "controls the rotations per cycle ratio, where 1 = 1 cycle / 360 degrees",
            },
            ReferenceParam {
                name: "size",
                r#type: "number",
                description: "the diameter of the spiral",
            },
            ReferenceParam {
                name: "thickness",
                r#type: "number",
                description: "line thickness",
            },
            ReferenceParam {
                name: "cap",
                r#type: "string",
                description: "style of line ends: butt (default), round, square",
            },
            ReferenceParam {
                name: "inset",
                r#type: "number",
                description: "number of rotations before spiral starts (default 3)",
            },
            ReferenceParam {
                name: "playheadColor",
                r#type: "string",
                description: "color of playhead, defaults to white",
            },
            ReferenceParam {
                name: "playheadLength",
                r#type: "number",
                description: "length of playhead in rotations, defaults to 0.02",
            },
            ReferenceParam {
                name: "playheadThickness",
                r#type: "number",
                description: "thickness of playheadrotations, defaults to thickness",
            },
            ReferenceParam {
                name: "padding",
                r#type: "number",
                description: "space around spiral",
            },
            ReferenceParam {
                name: "steady",
                r#type: "number",
                description: "steadyness of spiral vs playhead. 1 = spiral doesn't move, playhead does.",
            },
            ReferenceParam {
                name: "activeColor",
                r#type: "number",
                description: "color of active segment. defaults to foreground of theme",
            },
            ReferenceParam {
                name: "inactiveColor",
                r#type: "number",
                description: "color of inactive segments. defaults to gutterForeground of theme",
            },
            ReferenceParam {
                name: "colorizeInactive",
                r#type: "boolean",
                description: "wether or not to colorize inactive segments, defaults to 0",
            },
            ReferenceParam {
                name: "fade",
                r#type: "boolean",
                description: "wether or not past and future should fade out. defaults to 1",
            },
            ReferenceParam {
                name: "logSpiral",
                r#type: "boolean",
                description: "wether or not the spiral should be logarithmic. defaults to 0",
            },
        ],
        examples: &[
            "note(\"c2 a2 eb2\")\n.euclid(5,8)\n.s('sawtooth')\n.lpenv(4).lpf(300)\n._spiral({ steady: .96 })",
        ],
        tags: &["visualization"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "pitchwheel",
        synonyms: &[],
        summary: "Renders a pitch circle to visualize frequencies within one octave",
        description: "Renders a pitch circle to visualize frequencies within one octave",
        params: &[
            ReferenceParam {
                name: "hapcircles",
                r#type: "number",
                description: "",
            },
            ReferenceParam {
                name: "circle",
                r#type: "number",
                description: "",
            },
            ReferenceParam {
                name: "edo",
                r#type: "number",
                description: "",
            },
            ReferenceParam {
                name: "root",
                r#type: "string",
                description: "",
            },
            ReferenceParam {
                name: "thickness",
                r#type: "number",
                description: "",
            },
            ReferenceParam {
                name: "hapRadius",
                r#type: "number",
                description: "",
            },
            ReferenceParam {
                name: "mode",
                r#type: "string",
                description: "",
            },
            ReferenceParam {
                name: "margin",
                r#type: "number",
                description: "",
            },
        ],
        examples: &[
            "n(\"0 .. 12\").scale(\"C:chromatic\")\n.s(\"sawtooth\")\n.lpf(500)\n._pitchwheel()",
        ],
        tags: &["visualization"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "scope",
        synonyms: &["tscope"],
        summary: "Renders an oscilloscope for the time domain of the audio signal.",
        description: "Renders an oscilloscope for the time domain of the audio signal.",
        params: &[
            ReferenceParam {
                name: "config",
                r#type: "object",
                description: "optional config with options:",
            },
            ReferenceParam {
                name: "align",
                r#type: "boolean",
                description: "if 1, the scope will be aligned to the first zero crossing. defaults to 1",
            },
            ReferenceParam {
                name: "color",
                r#type: "string",
                description: "line color as hex or color name. defaults to white.",
            },
            ReferenceParam {
                name: "thickness",
                r#type: "number",
                description: "line thickness. defaults to 3",
            },
            ReferenceParam {
                name: "scale",
                r#type: "number",
                description: "scales the y-axis. Defaults to 0.25",
            },
            ReferenceParam {
                name: "pos",
                r#type: "number",
                description: "y-position relative to screen height. 0 = top, 1 = bottom of screen",
            },
            ReferenceParam {
                name: "trigger",
                r#type: "number",
                description: "amplitude value that is used to align the scope. defaults to 0.",
            },
        ],
        examples: &["s(\"sawtooth\")._scope()"],
        tags: &["visualization"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "spectrum",
        synonyms: &[],
        summary: "Renders a spectrum analyzer for the incoming audio signal.",
        description: "Renders a spectrum analyzer for the incoming audio signal.",
        params: &[
            ReferenceParam {
                name: "config",
                r#type: "object",
                description: "optional config with options:",
            },
            ReferenceParam {
                name: "thickness",
                r#type: "integer",
                description: "line thickness in px (default 3)",
            },
            ReferenceParam {
                name: "speed",
                r#type: "integer",
                description: "scroll speed (default 1)",
            },
            ReferenceParam {
                name: "min",
                r#type: "integer",
                description: "min db (default -80)",
            },
            ReferenceParam {
                name: "max",
                r#type: "integer",
                description: "max db (default 0)",
            },
            ReferenceParam {
                name: "scroll",
                r#type: "boolean",
                description: "draw a scrolling spectrogram instead of the analyser (default 0)",
            },
            ReferenceParam {
                name: "color",
                r#type: "string",
                description: "the bars' colour; the pattern's own color() wins over it",
            },
        ],
        examples: &[
            "n(\"<0 4 <2 3> 1>*3\")\n.off(1/8, add(n(5)))\n.off(1/5, add(n(7)))\n.scale(\"d3:minor:pentatonic\")\n.s('sine')\n.dec(.3).room(.5)\n._spectrum()",
        ],
        tags: &["visualization"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "video",
        synonyms: &[],
        summary: "Shows the picture of a video sample while its sound plays.",
        description: "Shows the picture of a video sample while its sound plays. The frame follows the pattern's clock, so the picture plays with the sample and holds when the transport holds. The picture decodes once per clip and stays small. H.264, HEVC, AV1, VP9, VP8 and Motion JPEG decode in this build. A clip in another video codec says which codec it is.",
        params: &[],
        examples: &["$: s(\"bd\")._video()"],
        tags: &["visualization"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "markcss",
        synonyms: &[],
        summary: "Overrides the css of highlighted events.",
        description: "Overrides the css of highlighted events. Make sure to use single quotes!",
        params: &[ReferenceParam {
            name: "css",
            r#type: "string",
            description: "the css to give a sounding event's highlight, in single quotes",
        }],
        examples: &["note(\"c a f e\")\n.markcss('text-decoration:underline')"],
        tags: &["visualization"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
];
