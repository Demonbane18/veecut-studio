# Sound effects and the music bed

VeeCut has its own sound-effects kit, made in code. The roles, the timing after the picture and the levels under the voice follow NextPhase Editor V1's published measurements (see CREDITS.md). None of its sound files are used.

## The built-in kit

`veecut sfx` writes any missing kit file and lists each one with its length, the time of its hit and its role. The files are 48 kHz mono WAVs with a -3 dBFS peak in `.veecut/sfx/v1/`. They are made the same way every time, so a deleted file comes back unchanged; `veecut sfx --regenerate` writes them all again. `--json` gives each file's path.

| File | Length | Role | Placed by the documentary recipe |
| --- | --- | --- | --- |
| `click-1` to `click-4` | about 70 ms | a picture change, a pop-in, a cut in a run | on every picture change without its own sound, and where a face beat starts; the four variants rotate |
| `tick` | about 25 ms | a small marker or a strobe image | when a keyword or shock word lands; other recipes use it when text lands |
| `pane-click` | 16 ms | one tile pane, one per frame | nine of them, one per frame, on a tile reveal |
| `swish` | 0.38 s, the hit at its end | into a title; its hit is the cut | on a whip |
| `glitch` | 0.35 s | under a datamosh | on a datamosh |
| `riser` | 1.5 s, peaking at its end | builds into a cut and peaks on it | not placed |
| `sub-tone` | 2 s low pad | very low under a black or UI card | under a black card |
| `sub-boom` | 2 s, a 55 to 40 Hz drop | very low drop under a card | not placed |
| `impact` | 0.9 s | the beat slamming back after a silence | where the bed comes back after the punchline |
| `hit` | 1.6 s | one cut that needs weight (rare) | not placed |
| `outro` | 2.5 s | under the end card | not placed |

A `voice_only` style places none of them.

## Timing and levels

- Each sound's hit lands 8 ms after the picture change it belongs to (Editor V1 measured 4 to 14 ms). A riser starts early so its peak lands there. A sound that would start before 0 loses its head; tails stop at the end of the main track. Clicks closer than one frame merge into the first.
- Each sound's loudest 50 ms sits a fixed distance under the voice's loudness: clicks 4.5 dB, tick, swish and glitch 5.5, pane clicks 9 (nine land inside a third of a second), riser, impact and hit 1.5, outro 8, sub boom 15, sub tone 18. Editor V1 measured clicks 2 to 7 dB under the voice. A gain never lifts a sound's own peak above -1 dBFS. When the voice cannot be measured, VeeCut assumes -20 LUFS.
- The sounds go on the `sfx` track, one clip each. Patch a clip's `volume` to change one; delete clips for fewer.

## Your own sounds

Every recipe except the documentary puts a sound where text lands. It cycles through the files in `my-sounds/sfx/` at half volume, starting 0.05 s before the text, so repeats do not sound like a loop. With no files there, it uses the kit's `tick`. The user's own sounds always win for these text moments.

The documentary recipe always uses the kit, because its sounds are tied to the picture effects.

To add a kit sound by hand, find its path and hit time with `veecut sfx --json`, bring it in with `veecut add .veecut/sfx/v1/riser.wav --role sfx`, then `add_clip` it on the `sfx` track starting at the picture change plus 0.008 s minus its hit time.

## Rules

From Editor V1:

- A sound on every picture change. Rotate the click variants; one file repeated 80 times sounds like a metronome.
- The effects write the cues. Do not hand-time a row of clicks; use the effect that comes with them.
- Never put a boom on a flash, a whoosh on a wall sweep, or a typing sound anywhere.
- Never put a drop or an impact on a short punchline word or a name. It masks the consonant: "Claude did." came back from a re-transcription as "Flaw dead".
- Check by ear and with the words check: `veecut qa outbox/<slug>-vN.mp4 --words` lists words that went missing or came back different, and names the sound or music clip that starts within 0.15 s of each. Lower or move that clip, then render again.

## The music bed

Two kinds of bed:

- Talking heads, tutorials, podcasts, clips and explainers use a ducked bed: `--music`, or the first file in `my-sounds/music/` when the style sound is `sfx_music`. It is turned down 11 dB, ducks under speech by `/audio/duck_db` (default 12) and fades in and out over 1.5 s. Vlogs, promos and music videos cut to the song itself.
- The documentary bed is cut, not ducked. It takes `--music`, or the first file in `my-sounds/music/`, and runs from frame 0 at a fixed trim about 5.5 LU under the voice's measured loudness (never boosted; 0.35 when either loudness cannot be measured). It loops to the end, fades in over 0.03 s and rings out over 0.9 s. It goes silent from just before the punchline until just before the next line, with 0.02 s fades on each side, and comes back with an `impact`. Under each black card it drops 8 dB.

Editor V1's bed rules behind this: about 5.5 LU under the voice from frame 0 with a fixed trim (no loudness normalising of the bed), not ducked; cut out completely under the punchline so it plays dry, slammed back on the next line; about 8 dB down under a black word card and about 4 dB under a rebuilt UI card; a fade-in of at most 0.05 s and a ring-out of about 0.9 s; line the song's drop up with the line after the punchline.

Only use music the user has the rights to. Changing the bed after the edit (keeping it under the punchline, cutting it under another line) is in `workflows/documentary.md` under Common notes.
