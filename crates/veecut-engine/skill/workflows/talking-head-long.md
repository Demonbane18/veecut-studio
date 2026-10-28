# Talking head, long

YouTube or the Facebook feed: one person talking to camera, usually several minutes, published horizontal (or 4:5 for the feed).

## When to use it

The user recorded a longer talk to camera and wants it as one horizontal video. It is also the default for `veecut edit` when the style format is `horizontal`. For short vertical cuts of the same recording, run `clips` on it afterwards.

## Ask first

- Where it goes, if they have not said: YouTube (16:9, the default) or the Facebook or Instagram feed (pass `--aspect 4:5`).
- Whether they want a title or hook card. The recipe adds no automatic hook for long videos; add one with `add_text` if they want it.

## Command

```
veecut edit inbox/episode-12.mp4 --kind talking-head-long --json
veecut edit inbox/episode-12.mp4 --kind talking-head-long --aspect 4:5 --json
veecut edit inbox/episode-12.mp4 --kind talking-head-long --broll inbox/office.mp4 inbox/chart.png --json
```

## What the recipe does

- Makes a 16:9 project (or the `--aspect` you pass) and transcribes the recording.
- Cuts pauses the natural way, over 0.55 s, keeping a breath of 0.14 to 0.24 s (over 0.25 s when the style pace is `tight`). Stutters, false starts and high-confidence retakes are cut, keeping the last take; the rest go in the review list.
- Keeps the face in frame when the crop changes the shape of the picture.
- Punches in to 1.10x about every 7 s, at most 10 s apart, at cuts and phrase boundaries.
- Adds no automatic hook.
- Uses each `--broll` file once as a full-frame cutaway of at most 3 s, muted, at a sentence start after the first 3 s, spread evenly through the video; stills get a slow push-in.
- Adds captions in the style's mode, moved off the face, with a scrim behind them when the footage would make them hard to read.
- Adds a ducked music bed (11 dB under the voice, 1.5 s fades) from `--music` or `my-sounds/music/` when the style sound is `sfx_music`, and sound effects on text from `my-sounds/sfx/` unless the style is `voice_only`.
- Renders `outbox/<slug>-v1.mp4` and checks it.

## Editing rules

From Herk Edit Kit's playbook (MIT), adapted to long form:

- Cut between phrases at real silence, never inside a phrase. Keep at least about 0.08 s after a word ends. Never speed the voice up.
- Drop a filler only when there is silence on both sides of it. Keep the last clean take of a retake. Ask before cutting emphatic repetition ("never, never").
- Fewer cuts between phrases beat many cuts inside them. The version that worked in the playbook ran 5 s longer.
- The first seconds still matter: open on a claim or a question, not a greeting or a logo. Show the list of what is coming early, and call back to the opening at the end.
- Text and graphics land within 2 frames of the spoken word, at most 0.15 s early. A graphic shows the example, the evidence or the consequence, not the same words again.
- Every shot moves: stills and long static shots get a slow push-in.
- The horizontal version gets its own framing and text placement; do not just crop the vertical one. `veecut render --aspect` re-places text and captions.
- Music sits about 11 dB under the voice (9 to 14 dB), ducked under speech. Sound effects follow visible actions, not every cut.
- Master near -14 LUFS (`/audio/target_lufs`), and never fade the last spoken word.

## Common notes

| Note | Fix |
| --- | --- |
| "Too many zooms" | `patch` the clips with `{"zooms":[]}` and keep the ones on key lines |
| "Add a title at the start" | `{"op":"add_text","text":"How I *edit* in 10 minutes","start":0,"duration":3,"preset":"title"}` |
| "Put my name on screen" | `{"op":"add_text","text":"Maria Santos\nFounder","start":2,"duration":4,"preset":"lower_third"}` |
| "Chapter cards" | `add_text` with preset `card` or `counter` at each section start (find times in `veecut transcript`) |
| "No captions on YouTube" | `veecut captions --mode off`, and offer `veecut transcript --srt` for a subtitle file instead |
| "Cut this section" | `cut_range` from its first word to the next section's first word |
| "Vertical teaser" | `veecut edit SAME-FILE --kind clips --clips 1` |

More in `reference/editor-api.md`.

## More skills

`veecut skills recommend --kind talking-head-long` lists catalog skills from the `editing` and `shorts` categories.
