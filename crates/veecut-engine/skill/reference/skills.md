# Skills and native look packs

A skill is a folder containing `SKILL.md` with frontmatter `name` and `description`. Its instructions can guide an agent without changing VeeCut. An optional `veecut.toml` lets VeeCut apply the style through its own renderer. Imported scripts remain files for inspection: VeeCut never runs them.

`veecut init` installs and refreshes three MIT built-ins in both `.agents/skills/` and `.claude/skills/`: the core `veecut` skill, `veecut-arcade` and `veecut-editorial`. Arcade uses pixel type, stepped pixel transitions, a progress meter and original synthesized game audio. Editorial uses paper, soft monochrome footage, terracotta highlights and whip cuts. Built-ins cannot be removed.

## Turn any skill into a native look

1. Ask the user to import it through Studio's Skills tab or `veecut skills add SOURCE`. Sources include a folder, ZIP, tar.gz, GitHub repo, Google Drive file link and HTTPS archive. Installing outside content is a human action; MCP has no add tool.
2. Read the installed `SKILL.md` and relevant reference files. Identify its colours, typography, timing, motion, sound and layout. Check the licence notes before reusing files. Importing a skill grants no rights to its art, code, fonts, music or sound files. When no licence is present, keep it for personal inspection and do not redistribute it.
3. Map the style to the fields below. Write fresh instructions and look values. Use bundled fonts and synthesized kit sounds where possible. If a skill needs an effect VeeCut does not have, explain that limit; do not invent a field or run the skill's scripts. Import an authorised finished clip through the normal asset flow when needed.
4. Put `veecut.toml` beside `SKILL.md`. Relative font and audio files must stay inside that folder. Use upstream fonts with their licence files, never fonts taken from an unlicensed pack. VeeCut does not execute shell commands or download media from TOML.
5. Run `veecut skills check NAME`. Fix every parse, reference or font error. Run `veecut skills apply NAME -p PROJECT`, then `veecut frame -p PROJECT --at T` at representative moments. Check narrow frames, captions over faces, entrances and cuts. Applying saves one undo step. In Studio, export only when asked.
6. Iterate using the same file and commands. Reapplying removes the previous look's generated overlays and audio before adding replacements. Ordinary text and manually added sound remain. The `look-` item ID prefix is reserved for generated look items; do not assign it to manual items.

List installed skills with `veecut skills list`; `skills check` reports kinds, fonts, scripts and licence notes. Catalog SPDX information is used when GitHub supplies it. Open the installed folder to edit your own pack. `skills remove NAME` removes added or own skills; `skills update` re-fetches imported sources and catalog installs.

## Save this project's look

Run `veecut skills new my-look --description "My editing style" --from-project -p PROJECT`. It creates both skill copies with `SKILL.md` and a captured `veecut.toml`; existing names are refused. Edit the primary copy under `.agents/skills/my-look/`, then check and apply it. If agents also use the Claude copy, keep its files in sync.

Capture keeps caption settings, the global look, canvas background, existing text style overrides, look-generated overlays, the first eligible visual clip's grade/motion/fit and the main-track transition pattern. Ordinary story text is not copied into overlays. A captured overlay ending with the project keeps an open end so it follows the next project's duration. Capture omits external fonts and audio references because their files may live outside the new skill; copy authorised files into the new folder and add those refs yourself. Clip motion is captured as the nearest `hold`, `push` or `glide`, not its exact camera path.

For a blank pack, use `veecut skills new my-look --description "When to use this look"`. Fill in its generated schema-1 template and instructions, then run the same check/apply/frame loop. MCP equivalents are `skills_installed`, `skills_check`, `skills_new` (with `from_project`) and `skills_apply`; `edit` accepts `skill`.

## Schema 1 field reference

Keys are case sensitive. Unknown top-level fields are errors. All fields except `schema` are optional. TOML omits absent options; do not write `null`. Times are seconds; positions and sizes are fractions of the canvas. Colours are `#RRGGBB` or `#RRGGBBAA`.

| Field | Meaning |
| --- | --- |
| `schema` | Required integer `1`. |
| `kinds` | List of suitable recipe kinds: `talking_head_short`, `talking_head_long`, `tutorial`, `podcast`, `clips`, `vlog`, `explainer`, `promo`, `music_video`, `documentary`. Studio uses the first as its initial kind. |
| `aspect` | `9:16`, `16:9`, `4:5` or `1:1`. Omit to keep the canvas size. |
| `background` | Canvas colour. |
| `[captions]` | Patch any caption field: `mode`, `font`, `size`, `color`, `highlight`, `stroke`, `shadow`, `boxed`, `ink_on_light`, `uppercase`, `position`, `max_words`, `max_chars`. Unspecified values stay unchanged. |
| `[look]` | Whole-frame look: `preset`, `brightness`, `contrast`, `saturation`, `temperature`, `vignette`. Fine adjustments are -1..1, vignette 0..1. Presets: `natural`, `warm`, `cool`, `black_white`, `teal_orange`, `vivid`, `matte`. Set on the whole frame, including text; prefer clip grades when accents should keep their colour. |
| `[text.PRESET]` | Style overrides for every current item of that preset, including this pack's overlays. Fields: `font`, `size`, `color`, `accent`, `background`, `stroke`, `uppercase`, `align`. `align` is `left`, `center` or `right`; `stroke` is a fraction of font size. Items added later keep preset defaults until reapply. |
| `[clips]` | `motion` = `hold`, `push` or `glide`; `fit` = `cover` or `contain`. Affects images and B-roll, not the speaker's own camera. |
| `[clips.grade]` | `look` = a TreatLook below; `pixelate` = block size as a fraction of canvas height (0 off, 0..1, try 0.012); `halftone` and `vignette` = 0..1; optional `recolor = { from = 0, to = 30, target = 120 }` uses hue degrees, with wrapped ranges allowed. |
| `[transitions]` | Required within this table: `kind`, positive `duration`; `every` defaults to 1 and must be at least 1. Applies at each Nth main-track cut. Other cuts retain their current transitions. Only `crossfade` overlaps clips. |
| `[[overlays]]` | Repeated text decoration: required `preset`; optional `text` (default empty), `position = { x = 0.5, y = 0.3 }`, `start` (default 0), `end` (omitted means project end), `anim_in`, `anim_out`. Require 0 <= start < end. Use `[overlays.style]` for the same text style fields; these override `[text.PRESET]` for this item. |
| `[sound]` | `text` cue at text entrances, `cut` cue at main-track cuts, `music` looping bed; each is `kit:SLUG` or a relative file such as `sounds/pop.wav`. Optional `music_db` is gain in dB; omitted gain uses the existing under-voice bed rule. Beds fade in/out over 1.5 s. |
| `fonts` | List of relative `.ttf`/`.otf` paths. Empty or omitted scans `fonts/`. Include the upstream licence. Refer to the font's family name, not its filename, in styles. Fonts are registered on the project and loaded for preview, measurement and export. |

Caption modes: `word_pop`, `highlight`, `pills`, `subtitle`, `reveal`, `off`. `size` is a fraction of height; `stroke` is a fraction of font size; `position` is vertical (0 top, 1 bottom). `ink_on_light` adapts reveal text to light footage. `shadow`, `boxed` and `uppercase` are booleans.

Every TextPreset: `hook`, `title`, `card`, `lower_third`, `callout`, `counter`, `label`, `quote`, `list`, `kinetic`, `cta`, `keyword`, `chapter`, `shock`, `black_card`, `mega`, `marker`, `meter`. Use `*words*` in text for the accent colour. `mega` renders behind overlay clips. `marker` wipes its translucent band left to right in 0.35 s. `meter` fills twelve segments over its item's duration, with an optional label; omit `end` for a whole-story bar.

Every TreatLook: `natural`, `teal`, `mono`, `soft_mono`, `warm`, `cold`, `invert`. `soft_mono` lifts blacks with gentle contrast. These are per-clip treatments, distinct from the whole-frame `[look].preset` list.

Every TransitionKind: `crossfade`, `dip`, `flash`, `negative`, `glitch`, `tiles`, `focus`, `whip`, `pixelate`. Pixelate resolves incoming blocks to sharp in six steps; start with 0.4 s. Whip and pixelate keep hard-cut timing.

Every Anim: `none`, `fade`, `pop`, `slide_up`, `slide_down`, `slide_left`, `slide_right`, `typewriter`, `wipe`. Omitted animations use each preset's defaults: `none` for `shock`, `black_card`, `mega`, `marker`, `meter`; `fade` otherwise.

Every kit slug: `click`, `tick`, `pane_click`, `swish`, `glitch`, `riser`, `sub_tone`, `sub_boom`, `impact`, `hit`, `outro`, `chiptune`, `coin`, `blip`. Slugs use underscores, unlike some WAV filenames. `chiptune` is a seamless sixteen-bar bed at 140 BPM, `coin` is a 0.25 s rising pickup and `blip` is a 45 ms pip. See `reference/sound.md` for levels, timing and voice safety. Keep cues sparse when words become hard to hear.

## Example: a highlight and a story meter

```toml
schema = 1
kinds = ["explainer"]
background = "#E9E7E1"

[captions]
font = "Inter"
highlight = "#D97757"

[text.marker]
color = "#1C1B1E"
accent = "#D97757"

[clips]
motion = "push"
[clips.grade]
look = "soft_mono"

[[overlays]]
preset = "marker"
text = "The key idea"
start = 1.0
end = 3.0
position = { x = 0.5, y = 0.3 }

[[overlays]]
preset = "meter"
text = "STORY"
anim_in = "none"
anim_out = "none"

[sound]
cut = "kit:sub_boom"
```

For portrait pixel styling, start from `veecut-arcade/veecut.toml` instead. Its `Press Start 2P` family is bundled from upstream Google Fonts under the OFL. VeeCut's built-in look instructions and values are MIT; that licence does not cover outside packs or grant rights to their assets.
