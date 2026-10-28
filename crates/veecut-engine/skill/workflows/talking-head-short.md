# Talking head, short

Reels, TikTok and Shorts: one person talking to camera, under 3 minutes, published vertical.

## When to use it

The user recorded themselves talking and wants a short social video. This is also what `veecut edit` does when no kind is given (unless the style format is `horizontal`). If the recording is longer than about 3 minutes and they want several shorts from it, use `clips` instead.

## Ask first

Nothing, usually. Ask only if it helps:

- Do they have a hook line in mind? Pass it with `--hook`; otherwise the recipe takes it from the first sentence.
- Which language do they speak in it, if the style says `auto` and the first try comes out wrong? Pass `--language tl` or `--language en`.

## Command

```
veecut edit inbox/my-take.mp4 --kind talking-head-short --json
veecut edit --kind talking-head-short --hook "Stop paying for *editors*" --music my-sounds/music/bed.mp3 --json
```

Without a file it edits the newest video in `inbox/`.

## What the recipe does

- Makes a 9:16 project and transcribes the recording.
- Cuts pauses and mistakes. The pace comes from the style: `tight` cuts pauses over 0.25 s, `natural` cuts pauses over 0.55 s, and each trimmed pause keeps a breath of 0.14 to 0.24 s. Stutters, false starts and high-confidence retakes are cut, keeping the last take. Medium and low-confidence ones stay in and appear in the review list.
- Crops around the face.
- Alternates punch-ins (1.15x) and the wide shot at cuts and phrase boundaries, so no shot runs past 2.8 s.
- Puts hook text over the first 3 s when the style has `hook = true`: the `--hook` text, or the first sentence cut to at most 7 words with the strongest word in the accent color. A picked hook goes in the review list.
- Adds captions in the style's mode and moves them off the face.
- Adds a music bed from `--music`, or the first file in `my-sounds/music/` when the style sound is `sfx_music`. It sits 11 dB under the voice, ducks under speech and fades in and out over 1.5 s.
- Adds sound effects from `my-sounds/sfx/` when text lands, unless the style is `voice_only`.
- Renders `outbox/<slug>-v1.mp4` and checks it.

## Editing rules

These come from Herk Edit Kit's editing playbook (MIT), measured on 27 top reels. The recipe follows most of them; use them to judge the draft and the user's notes.

Pacing:

- Speech starts within 0.1 s of the first frame. No logo bumper and no silence before it.
- Tight reel style keeps pauses between phrases under about 0.2 s. Natural beats after a comma or a list marker ("First,") run 0.2 to 0.4 s and stay.
- Good reels run 4.0 to 4.7 words per second.
- Cut between phrases at real silence, never inside a phrase. Fewer cuts between phrases beat many cuts inside them.
- Drop a filler word only when there is silence on both sides of it.
- Never speed the voice up.
- Cut only after a word has fully faded: keep at least about 0.08 s after a word's end.

Mistakes:

- A stutter is the same word twice with less than 0.5 s between them.
- A false start has the same opening, a first take under 6 s and a restart within 12 s.
- For a retake, keep the last clean take.
- Emphatic repetition ("never, never") looks like a stutter. Ask before cutting it.

Camera:

- Shots average 1.1 to 1.4 s and none runs over about 2.8 s.
- The first cut lands 0.6 to 1.0 s in, on the first charged word.
- Each item in a spoken list gets its own shot.
- Use punch-ins for pivots, emphasis and the call to action. Without B-roll, alternate wide and punch-in at cuts.
- Every shot moves. Stills get a slow push-in; no frozen frames.

Captions:

- 1 to 3 words per page, split on phrase boundaries, appearing 2 frames before the first word.
- Herk's house look is Montserrat ExtraBold in all caps, about 0.033 of the frame height (62 to 64 px on 1080x1920). VeeCut's `small` size is 0.035.
- Color on 25 to 45% of pages. One look for the whole video; `boxed` captions read on black, white and busy footage.
- Never over the mouth or chin, above all in the first second. In a punch-in they sit on the shirt.
- Keep text inside the safe region of a 1080x1920 frame: x from 90 to 930, y from 200 to 1600. The right edge and the bottom belong to the app buttons.

Story:

- Hook in the first 3 s: a claim or command that names the tool or the person, and works with the sound off.
- One line of credibility, then the list items early as an open loop.
- For each item, a short card, a one-line definition, visuals and a punch-in on the key line.
- A resolution that calls back to the hook, then the call to action: show the resource, then punch in.
- Text lands within 2 frames of the spoken word, at most 0.15 s early.

Sound:

- The music bed sits about 11 dB under the voice (9 to 14 dB works), ducked under speech, quieter in the first 1 to 2 s.
- Sound effects follow visible actions, not every cut. No laser zaps, pitch-drop plops, sci-fi chirps or a whoosh on every cut. Vary the files for repeated items.
- The playbook masters near -14 LUFS with true peak at or below -1.5 dBTP. VeeCut's defaults are -14 LUFS and -1.0 dBTP (`/audio/target_lufs`, `/audio/true_peak_db`).
- Never fade out the last spoken word.

Checks before you show the draft:

- `veecut frames`, then `veecut frame --at T` around cuts: lips in sync, every word whole, nothing over the mouth in the first 2.5 s, captions readable.
- `veecut qa outbox/<slug>-v1.mp4` for loudness and true peak. The user's review is the final check.

## Common notes

| Note | Fix |
| --- | --- |
| "Faster", "bilisan mo" | `cut_range` on the pauses they mean. For every video: a rule plus `veecut style set pace tight`. |
| "Change the hook" | `patch` the text item with preset `hook`: `{"op":"patch","item":"text-1","patch":{"text":"New *hook*"}}` |
| "No hook" | `delete` the hook item; if lasting, `veecut style set hook false` |
| "Captions cover my face" | `{"op":"set","path":"/captions/position","value":0.75}` (or 0.25 if the face is low) |
| "Bigger captions" | `{"op":"set","path":"/captions/size","value":0.06}` |
| "Too much zooming" | `patch` the clips with `{"zooms":[]}`, or keep one zoom per key line |
| "Zoom in here" | `{"op":"add_zoom","at":12.0,"duration":1.5,"scale":1.15}` |
| "You cut my word" | `trim` the clip before the cut outward by about 0.1 s |
| "Keep the first take" | Put it back with `add_clip` from the source time, and `cut_range` the later take |
| "Music too loud" | `patch` each music item, for example `{"volume":0.18}` |
| "Horizontal version too" | `veecut render --aspect 16:9` |

More in `reference/editor-api.md`.

## More skills

`veecut skills recommend --kind talking-head-short` lists the best catalog skills for this kind, from the `editing` and `shorts` categories.
