# Editor API

Every change to a project is an op: a small JSON object with an `"op"` name. The command line, the MCP server, the desktop editor and you all use the same ops. An op either applies fully and leaves a valid project, or fails with a plain error and changes nothing.

```
veecut ops '{"op":"cut_range","from":5,"to":7}'      one op
veecut ops '[{...},{...}]'                           several ops, all or nothing
veecut ops --file ops.json                           ops from a file (best on Windows)
veecut ops -                                         ops from standard input
```

`veecut schema` prints the JSON schema of every op. `veecut undo` restores the project as it was before the last change, and `veecut redo` brings it back.

## Times, places and ids

- Times in ops are timeline seconds: where things are in the edited video, after the cuts. 2:05 is `125`.
- `veecut transcript` shows each sentence with its timeline time and its source time, so you can find where words sit in the edit. The review list from `veecut edit` gives source times; look them up there.
- Positions are fractions of the frame. `{"x":0,"y":0}` is the top-left corner, `{"x":1,"y":1}` the bottom-right, `{"x":0.5,"y":0.5}` the center.
- Sizes of text are fractions of the frame height. `0.045` is a medium caption.
- `veecut status --json` lists every track with its items. Each item has an `id`, a `start` and a `duration`. Clip ids look like `clip-3`, text ids like `text-1`, caption pages like `cap-12`. Assets have ids like `asset-1`.
- The standard tracks are `main` (the magnetic main track), `broll`, `text`, `captions`, `music` and `sfx`. Some recipes add a `voice` track. On the main track, clips play back to back and their starts are computed, so you change their order, not their start.
- Edits on the main track keep every track in sync. `cut_range`, deleting a main clip and trimming one shorter cut that time from all tracks. Inserting a main clip before the end (`add_clip` with `index` or `start`, `duplicate`) or trimming one longer pushes later items on every track along; appending at the end moves nothing. Items that start at or after that point move; an item that runs across it stays where it is, so `split` that track there first if its second half should move too. Reordering main clips with `move` does not move the other tracks, so run `veecut captions --rebuild` after reordering. When the voice is on its own track (podcasts with `--voice` or several cameras, explainers), new picture time has no voice under it; add the matching voice piece with `add_clip` on the `voice` track.

## The ops

### cut_range

Removes a stretch of the timeline from every track and closes the gap. Use it for pauses, mistakes and anything the viewer should not see.

```json
{"op":"cut_range","from":12.4,"to":14.1}
```

### split

Splits items at a time. Without `track`, every item crossing that time is split.

```json
{"op":"split","at":8.0}
{"op":"split","at":8.0,"track":"main"}
```

### trim

Moves an item's edges, in timeline seconds. On a main-track clip the change ripples through every track: trimmed time is cut everywhere, and added time makes room everywhere. A trim may also reach outward to bring back source that was cut, as long as the source file is long enough.

```json
{"op":"trim","item":"clip-4","start":10.2}
{"op":"trim","item":"clip-4","end":12.6}
```

### move

Main-track clips move by `index` (their place in the sequence, 0 is first). Other items move by `start`. `track` moves an item to another track.

```json
{"op":"move","item":"clip-7","index":0}
{"op":"move","item":"text-2","start":4.5}
```

### delete

Deletes an item. Deleting a main-track clip also cuts its stretch of time from every other track, music included. On other tracks the gap stays.

```json
{"op":"delete","item":"text-1"}
```

### duplicate

Copies an item and places the copy right after it. On the main track, later items on every track move along.

```json
{"op":"duplicate","item":"clip-2"}
```

### add_clip

Puts part of an asset on a track. Without `track` it picks one by the asset's role: the main recording goes on `main`, other footage and images on `broll`, sound effects on `sfx` and other audio on `music`. Without `start` or `index` a main-track clip goes at the end; inserted earlier, it pushes later items on every track along. `src_in` is where in the source file the clip starts; `duration` defaults to the rest of the file, or 3 s for an image. `frame` places the clip on part of the canvas (picture in picture); without it the clip fills the canvas. `volume` is a linear gain (1.0 unchanged, 0.5 about -6 dB). `duck` lowers the clip while someone speaks; music is ducked by default.

```json
{"op":"add_clip","asset":"asset-1","track":"main","index":5,"src_in":31.2,"duration":2.4}
{"op":"add_clip","asset":"asset-3","track":"broll","start":6.0,"duration":2.5,"frame":{"x":0.05,"y":0.08,"w":0.9,"h":0.4}}
```

The first example puts a cut part back: the recording `asset-1` from 31.2 s for 2.4 s, as the sixth clip.

### add_text

Adds on-screen text. Wrap words in `*asterisks*` to draw them in the accent color.

```json
{"op":"add_text","text":"3 *free* tools","start":4.0,"duration":2.5,"preset":"card"}
{"op":"add_text","text":"Maria Santos\nFounder","start":1.0,"duration":4.0,"preset":"lower_third","anim_in":"slide_up","anim_out":"fade"}
```

- `preset`: `hook` (big text in the first seconds), `title`, `card` (the default, a rounded card with a line or two), `lower_third` (name and role), `callout` (a label pointing at something), `counter` (a big number or step count like "STEP 2/5"), `label` (a small pill), `quote`, `list` (one line per `\n`, revealed one by one), `kinetic` (one or two words slammed full screen) and `cta` (end card).
- `anim_in` and `anim_out`: `none`, `fade` (default), `pop`, `slide_up`, `slide_down`, `slide_left`, `slide_right`, `typewriter`, `wipe`.
- `position`: the center of the text, like `{"x":0.5,"y":0.3}`. Without it the preset picks a place inside the platform safe zones.
- `style` overrides the preset: `font`, `size` (fraction of the height), `color`, `accent`, `background`, `stroke` (outline width as a fraction of the font size), `uppercase`, `align` (`left`, `center`, `right`). Colors are `#RRGGBB` or `#RRGGBBAA`.
- `track`: a text track id. Without it the text goes on the first text track.

### add_zoom

Zooms the main-track picture at a time. `scale` defaults to 1.12; 1.0 is no zoom. `ease` 0 is an instant punch-in; a number of seconds makes a smooth push. `focus` is the point to zoom toward in the source picture; without it the clip's focus (usually the face) is used. The zoom stops at the end of the clip it starts in.

```json
{"op":"add_zoom","at":9.5,"duration":2.0,"scale":1.15}
{"op":"add_zoom","at":20.0,"duration":4.0,"scale":1.08,"ease":1.5}
```

### patch

A JSON merge patch on one item: the fields you give replace the old ones, and a list replaces the whole list. Item fields are `start` (not on the main track), `duration` and the fields of its body.

```json
{"op":"patch","item":"clip-3","patch":{"volume":0.5}}
{"op":"patch","item":"clip-3","patch":{"zooms":[]}}
{"op":"patch","item":"text-1","patch":{"text":"Stop editing *by hand*"}}
{"op":"patch","item":"clip-9","patch":{"fade_out":0.8}}
```

Clip fields: `speed` (the clip covers `duration * speed` seconds of source), `volume`, `muted`, `opacity`, `fit` (`cover` fills and crops around `focus`, `contain` shows the whole picture), `focus` (`{"x":..,"y":..}`), `zooms`, `frame`, `radius`, `fade_in`, `fade_out` (seconds), `transition` (`{"kind":"crossfade","duration":0.3}`; kinds `crossfade`, `dip`, `flash`), `ken_burns` (`{"from_scale":1.0,"to_scale":1.1}` plus optional `from_focus` and `to_focus`) and `duck`.

Text fields: `text`, `preset`, `style`, `position`, `anim_in`, `anim_out`.

When you change a clip's `speed`, change its `duration` too, or it will cover a different part of the source. To play a 12 s piece of source at 3x: `{"speed":3,"duration":4}`.

### set

Sets any project value by JSON pointer. The parent must exist; `-` at the end of a path appends to a list.

```json
{"op":"set","path":"/captions/position","value":0.3}
{"op":"set","path":"/look/preset","value":"warm"}
```

### add_marker

Pins a note to a moment. `veecut note "..." --at T` does the same from the command line.

```json
{"op":"add_marker","at":14.0,"note":"swap this shot"}
```

### add_track

Adds a track on top of the others. Kinds: `overlay`, `text`, `captions`, `audio`. A project has exactly one `main` track.

```json
{"op":"add_track","kind":"overlay","name":"Screenshots"}
```

## Common `set` paths

Captions (`/captions/...`, one style for every caption page):

| Path | Values |
| --- | --- |
| `/captions/mode` | `word_pop`, `highlight`, `pills`, `subtitle`, `off` |
| `/captions/size` | fraction of the height; 0.035 small, 0.045 medium, 0.06 large |
| `/captions/position` | vertical center as a fraction of the height; 0.25 upper third, 0.5 middle, 0.75 lower third |
| `/captions/color` | text color, `#RRGGBB` |
| `/captions/highlight` | color of the spoken word or key words and pills |
| `/captions/stroke` | outline width as a fraction of the font size; 0 turns it off (default 0.12) |
| `/captions/shadow` | `true` or `false` |
| `/captions/boxed` | `true` puts every caption in one dark box with a thin light edge |
| `/captions/uppercase` | `true` or `false` |
| `/captions/font` | a family from `veecut fonts` |
| `/captions/max_words` | most words on screen at once (default 3) |
| `/captions/max_chars` | most characters per line (default 20) |

After changing `max_words` or `max_chars`, run `veecut captions --rebuild` so the pages are grouped again. `veecut captions --mode pills` changes the mode in one step. When the footage behind text or captions would make them hard to read, VeeCut draws a dark (or light) scrim behind them on its own; it never changes the colors the user picked.

Look (`/look/...`): `preset` is `natural`, `warm`, `cool`, `black_white`, `teal_orange`, `vivid` or `matte`. The adjustments `brightness`, `contrast`, `saturation` and `temperature` run from -1 to 1, and `vignette` from 0 to 1. They add to what the preset does.

Audio (`/audio/...`): `target_lufs` (default -14, which suits Reels, TikTok, YouTube and Facebook), `true_peak_db` (default -1.0), `duck_db` (how many dB ducked music drops under speech, default 12) and `voice_cleanup` (`true` adds a high-pass and gentle noise reduction on the voice).

Canvas (`/canvas/...`): `width`, `height` (even numbers), `fps` and `background`. To render another shape once, use `veecut render --aspect 9:16` (or `16:9`, `4:5`, `1:1`) instead; text and captions re-place themselves. To change the project itself, set both sides in one array:

```json
[{"op":"set","path":"/canvas/width","value":1080},{"op":"set","path":"/canvas/height","value":1350}]
```

Tracks (`/tracks/N/...`): `muted` and `hidden`, where N is the track's place in the `tracks` list of `veecut status --json`, counting from 0.

## Bringing in a new file

`veecut add FILE... [--role ROLE]` brings files into the project and prints their asset ids. FILE is a path or the name of a file in `inbox/`. Roles are `source`, `broll`, `music`, `sfx`, `voice`, `image` and `logo`; without `--role` a video becomes `broll`, a picture `image` and a sound `music`. Then place it with `add_clip`:

```
veecut add inbox/street.mp4 --json
veecut ops '{"op":"add_clip","asset":"asset-9","track":"broll","start":6.0,"duration":2.5}'
```

Use the id that `veecut add` printed, not `asset-9`.

Never copy, move or convert files in `inbox/` to do this; the project points at them where they are.

## Notes and the ops that fix them

Look up ids with `veecut status --json` and times with `veecut transcript` first.

| The user says | Do this |
| --- | --- |
| "Faster", "tighter", "bilisan mo" | Cut the pauses they mean with `cut_range`. For the whole video, also save a rule and run `veecut style set pace tight` so the next edit cuts every pause. Never speed up the voice. |
| "Cut 0:05 to 0:07", "tanggalin mo yung 0:05 hanggang 0:07" | `{"op":"cut_range","from":5,"to":7}` |
| "Bigger captions", "lakihan mo yung captions" | `{"op":"set","path":"/captions/size","value":0.06}` |
| "Captions higher", "itaas mo yung captions" | `{"op":"set","path":"/captions/position","value":0.25}`. A smaller number is higher. |
| "Captions cover my face" | Check the affected time with `veecut frame --at T` and read the actual caption bounds reported by `veecut ops`. A lower requested position may be clamped by the safe area. Reduce caption size or words per page if needed; use an upper slot only when clear of both the face and hook. Check again before claiming the face is clear. |
| "Change the hook" | Find the text item with preset `hook` and `{"op":"patch","item":"text-1","patch":{"text":"New *hook* line"}}`. |
| "No hook" | `{"op":"delete","item":"text-1"}` (the hook's id) |
| "Music too loud", "hinaan mo yung music" | Patch every item on the `music` track, for example `{"op":"patch","item":"clip-30","patch":{"volume":0.18}}`. For more dip only under speech, `{"op":"set","path":"/audio/duck_db","value":18}`. |
| "No music" | `{"op":"set","path":"/tracks/4/muted","value":true}` after checking that track 4 is `music` |
| "Add text here" | `{"op":"add_text","text":"...","start":12.0,"duration":2.5,"preset":"card"}` |
| "Put B-roll here", "lagyan mo ng b-roll dito" | `veecut add FILE`, then `{"op":"add_clip","asset":"ID","track":"broll","start":12.0,"duration":2.5}` with the printed id |
| "Zoom in on this" | `{"op":"add_zoom","at":12.0,"duration":2.0,"scale":1.15}` |
| "Remove the zoom" | Patch the clip at that time: `{"op":"patch","item":"clip-6","patch":{"zooms":[]}}` |
| "Warmer", "mas warm" | `{"op":"set","path":"/look/preset","value":"warm"}`, or a lighter touch: `{"op":"set","path":"/look/temperature","value":0.2}` |
| "Black and white" | `{"op":"set","path":"/look/preset","value":"black_white"}` |
| "No captions" | `veecut captions --mode off` |
| "Different caption style" | `veecut captions --mode word_pop` (or `highlight`, `pills`, `subtitle`) |
| "You cut off my word" | Trim the clip before that cut outward by a little: `{"op":"trim","item":"clip-4","end":12.6}` (its old end plus about 0.1 s) |
| "Put back the part you cut" | `add_clip` with the source time from `veecut transcript` (see the add_clip example) |
| "Vertical version" | `veecut render --aspect 9:16` |
| "Horizontal version" | `veecut render --aspect 16:9` |
| "Undo that", "ibalik mo" | `veecut undo` |

After ops from Studio, check a frame if needed, answer briefly and stop. Render or open a video only when the note asks for one. Outside Studio, run `veecut render` for the next draft.
