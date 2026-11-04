# Documentary edit

A talking head cut into a fast documentary: found pictures on most lines, each one re-coloured to belong to the film, the speaker's own face between runs of pictures, small words that appear as they are said, a click on every change and a music bed that drops out under the punchline. The style and its numbers come from NextPhase Editor V1 by Jad M.H (NextPhase AI community), credited in CREDITS.md. VeeCut uses its ideas and published measurements, none of its code, text or sounds.

## When to use it

The user recorded themselves talking and wants it as a fast documentary with found footage, or asks for "a documentary", "Editor V1 style" or "a found footage edit". It suits a story, an explainer with a face or a news-style take. For a plain talk to camera with a few cutaways, use `talking-head-short` or `talking-head-long`. For a narration with no face, use `explainer`.

## Ask first

- Do they have B-roll files (footage, photos, screenshots)? Their own files go in first.
- May VeeCut search free footage online for the lines without their files (`--find`)? Say where it searches: NASA, Wikimedia Commons, Internet Archive and Openverse, plus Pexels when `PEXELS_API_KEY` is in `.env`. It only downloads files whose licence allows the edit, into `footage/`, and records every source.
- Do they have a music bed they have the rights to? Pass it with `--music`, or the recipe uses the first file in `my-sounds/music/`.
- Is there a punchline they want the music to drop out under? The recipe picks one and lists it in review; if they name another line, move the cut after the edit (Common notes below).
- Shape, if it matters: the recipe makes 16:9. Pass `--aspect 9:16` for a vertical version.

## Command

```
veecut edit inbox/take.mp4 --kind documentary --find --music my-sounds/music/bed.mp3 --json
veecut edit inbox/take.mp4 --kind documentary --broll inbox/office.mp4 inbox/chart.png --json
veecut edit inbox/take.mp4 --kind documentary --broll inbox/office.mp4 --find --hook "The year *everything* broke" --json
```

Without `--broll` or `--find` there are no pictures, so the speaker stays on screen and review says so.

## What the recipe does

- Transcribes the take and cuts pauses and mistakes like the talking-head recipes, then crops around the face. The speaker's camera gets no grade.
- Splits the timeline into runs of pictures and face beats, cutting on word onsets. It opens on pictures and never puts two face beats in a row. A run holds about 4 s of pictures; a face beat lasts 1.2 to 2.5 s (aim 1.8 s), which puts the face on screen about 30 percent of the time.
- Changes the picture every 0.6 to 1.5 s (aim 0.95 s). In videos over 3 minutes, the first 10 s keep that pace and the rest breathes to 1.2 to 2.0 s (aim 1.6 s).
- Uses the `--broll` files first, in order. With `--find`, it searches one query per run: the run's own name or concrete word, anchored to the video's subject (concrete words said at least twice, names first), so a line about "the test" searches "sls test", not "test". Vague words such as "ability", "important" or "properly" never go into a query. It keeps only results whose title shares a word with the query and drops titles about abuse, violence or sex, skips flagged results, prefers moving footage, takes at most 2 files per run and at most 12 per edit. A provider that fails twice is left out of the later searches. With fewer files than pictures it reuses the least used one and says so in review.
- Starts each video picture past the file's opening (a tenth of it, and at least its first 10 s, where B-roll packages put slates, titles and contact cards) and steps past stretches that would read as black, so a dark space shot or a fade never becomes a black beat.
- Grades every picture from what the line says: money, fire or history turns warm; documents, data and news mono; code, tech, night or space cold; places and buildings teal. Other pictures rotate so about 40 percent stay natural. Stills that are not mono get a 0.22 halftone; vignette is 0.35 on natural pictures and 0.25 on the rest. Every picture gets a slow push of 5.5 percent per second, at most 1.2x.
- Puts a cut effect on some pictures: a 9-frame tile reveal on the very first picture and at the start of every third run, a 4-frame negative into a new chapter (a line after a pause of 1 s or more), a 9-frame rack focus at other run starts, a 4-frame datamosh when the line says "broke", "leaked", "crash", "error" and similar, a 4-frame whip out of a shot of 1.2 s or more (at most every 20 s), and a 1-frame white box on every third cut inside a run.
- Shows small white captions that appear word by word as they are said (`reveal` mode, Inter, 0.057 of the height in 16:9). On light pictures they turn dark ink.
- Adds display words on pictures, never on the face and never two at once: one stressed word per run as a `keyword` for 1.2 s, a `shock` word in red capitals for numbers and money (0.8 s, about 1.5 a minute), and a `black_card` when the speaker says "nothing", "nobody", "none", "wala" or "no one" (at most two). Only words that can stand alone go on screen: an amount ("20%", "$40,000", "2023"), an acronym, or a concrete word of five letters or more. "two" or "2" never do.
- When the cut-out model is downloaded, puts a huge red `mega` word behind the speaker on face beats of 1.2 s or more that hold such a word, about one per 45 s, only where the face was found through the beat (recordings often cut to their own B-roll mid-sentence). Cutting out takes about 0.6 s a frame on an Apple silicon Mac. Without the model, review gives the `veecut cutout` command to run once.
- `--hook TEXT` shows a `chapter` title near the top for the first 2.5 s.
- Places sounds from the built-in kit: a click on every picture change and face beat, nine pane clicks on a tile reveal, a glitch on a datamosh, a swish on a whip, a tick when a display word lands, a sub tone under a black card. A `voice_only` style skips them. Details in `reference/sound.md`.
- Lays the music bed from frame 0 about 5.5 LU under the voice with a fixed trim, not ducked. It cuts out completely under the punchline (the last short line in the back part that is not a call to action), comes back with an impact on the next line, and drops 8 dB under black cards.
- Renders `outbox/<slug>-v1.mp4` and checks it. The summary gives the face share, the grades and cut effects counted, the display words, the mega words, the number of sounds and the bed. Review lists the punchline it picked and anything missing.

## Style rules

From NextPhase Editor V1's published measurements (by Jad M.H, NextPhase AI community; see CREDITS.md):

- Real material on almost every line, edge to edge. For each line ask what real thing the speaker means and where its own picture lives. Names, companies and claims are always the real thing; a metaphor fits only where the line allows it ("leaked" can be a dripping tap).
- The picture changes every 0.6 to 1.5 s (held-shot medians 0.56 to 0.79 s). Long videos keep that pace in the cold open and chapter opens and let the body breathe to 1.2 to 2.0 s.
- The speaker's own camera stays raw and is just one picture in the stream: 25 to 35 percent of the runtime, in 1.2 to 2.5 s stretches, never two face beats in a row.
- Every found picture is re-coloured. Measured shares: teal split about 22 percent (places), hard mono about 18 (documents, prices, archive), warm amber 8 to 11 (fire, money, history), natural with a push and vignette 32 to 42. Invert is for 1 to 2-frame flashes. Halftone 0.16 to 0.35 on stills that would look like stock. No grain, no lifted blacks.
- Moving footage covers 50 to 75 percent of the runtime; stills get a push of 4 to 7.5 percent per second.
- Cut effects and their lengths at 30 fps: white box 1 frame, negative 2 frames out and 2 in, datamosh 3 to 4, tile reveal about 9 with a click per pane, rack focus about 9, whip 4.
- Display words come in fixed tiers, never in between: key (white bold sans, 0.14 of the height, blurs in over 6 frames), title (italic serif, 0.126), shock (red #F2030A serif capitals, 0.163, cuts on hard, once or twice a minute at most), mega (red, 0.52, behind the speaker's head) and the black card (a small word alone on black).
- Captions are about 0.057 of the height, white, with a soft shadow, and always the speaker's own words. Words appear at their onsets in their final places. On a light picture they turn dark ink with a light halo.
- A click on every change. Clicks sit 2 to 7 dB under the voice and land 4 to 14 ms after the picture; VeeCut uses 4.5 dB and 8 ms. Rotate the click variants.
- The bed runs from frame 0 about 5.5 LU under the voice with a fixed trim, cut on structure instead of ducked: out completely under the punchline, back on the next line, about 8 dB down under a black card.
- Never put a drop or an impact on a short punchline word or a name. It masks the consonant: Editor V1's "Claude did." came back from a re-transcription as "Flaw dead".

## After the draft

1. Look at the pictures: `veecut frames`, or `veecut frame --at T` for one moment. Found footage is matched by keywords, so check that each picture fits its line.
2. `veecut sources` lists every found file with its provider, licence, author, page and flag. Tell the user what was downloaded and from where.
3. To swap a picture: `veecut find "IDEA" --json`, look at the contact sheet it saves, `veecut fetch N --json`, then patch the clip (Common notes). How to search well is in `reference/sourcing.md`.
4. For the red word behind the head: `veecut cutout --at T --dur D --mega WORD`. The range must stay inside one main-track clip. The first run downloads the cut-out model (about 26 MB) once.
5. Before calling it final, run `veecut qa outbox/<slug>-vN.mp4 --words`. It transcribes the render again and lists words that went missing or came back different, with any cut or sound starting next to them. Lower or move that sound, or trim the cut outward, then render again. It runs locally unless the user's config picked Groq.
6. When the user approves, run `veecut render --final`. It refuses while the edit uses a flagged source nobody approved, and lists them. Show the user that list. `veecut sources --credits` gives the credit lines for the video description.

## Common notes

Look up ids with `veecut status --json`. Pictures are clips on the `broll` track; display words are on the `text` track; sounds on `sfx`; the bed on `music`.

| Note | Fix |
| --- | --- |
| "Wrong picture here", "iba yung picture" | Find a better file (`veecut find`, then `veecut fetch N`), then patch the B-roll clip at that time: `{"op":"patch","item":"clip-12","patch":{"asset":"asset-15","src_in":0}}`. Its grade, push and transition stay. Give a video a `src_in` inside the file; give a video `{"grade":{"halftone":0}}` if it replaced a still. Do not delete the clip for this. |
| "Show my face here" | Delete the B-roll clip at that time: `{"op":"delete","item":"clip-12"}`. The gap shows the speaker. Only when they want the face there. |
| "Black and white on this one" | `{"op":"patch","item":"clip-12","patch":{"grade":{"look":"mono","halftone":0}}}`. Looks: `natural`, `teal`, `mono`, `warm`, `cold`, `invert`. |
| "No colour effect on this" | `{"op":"patch","item":"clip-12","patch":{"grade":null}}` |
| "Different transition here" | `{"op":"patch","item":"clip-12","patch":{"transition":{"kind":"negative","duration":0.133}}}`. Lengths are frames divided by the fps: 4 frames at 30 fps is 0.133. `{"transition":null}` makes it a plain cut. |
| "Move that word" | `{"op":"move","item":"text-4","start":12.3}`, and move the tick on the `sfx` track that starts with it. |
| "Remove that word" | `{"op":"delete","item":"text-4"}`, and delete its tick on the `sfx` track. |
| "Change that word" | `{"op":"patch","item":"text-4","patch":{"text":"BILLIONS"}}` |
| "Fewer clicks" | Delete the clicks they mean from the `sfx` track, or quieten them all by patching their `volume`. |
| "Add a click here" | Find the kit file with `veecut sfx --json`, `veecut add .veecut/sfx/v1/click-2.wav --role sfx`, then `add_clip` it on `sfx` at the picture change plus 0.008 s minus its hit time, with the same volume as the other clicks. |
| "Put a big word behind me" | `veecut cutout --at 21.4 --dur 1.6 --mega "NEVER"` inside one main-track clip. |
| "Keep the music under the punchline" | The bed is split around the punchline: the middle piece on the `music` track has volume 0, and its neighbours fade out and in over 0.02 s. Patch that piece back to the volume of its neighbours, `{"op":"patch","item":"clip-40","patch":{"volume":0.21}}` with their value, and delete the impact on the `sfx` track where the bed came back. |
| "Drop the music under this line instead" | Bring the old cut back (row above). Get the line's `timeline_start` and the next line's from `veecut transcript --json`, split the music there, `{"op":"split","at":31.2,"track":"music"}` and `{"op":"split","at":33.9,"track":"music"}`, patch the piece between to `{"volume":0}`, the piece before to `{"fade_out":0.02}` and the piece after to `{"fade_in":0.02}`. |
| "Cut this sentence" | `veecut transcript-search "SENTENCE" --json`, then `veecut cut-phrase "SENTENCE"`. Pictures, words and sounds ripple with it. |

More ops in `reference/editor-api.md`, sound in `reference/sound.md`, footage and rights in `reference/sourcing.md`, all relative to the loaded `SKILL.md` directory, never the workspace root.

## Rights

- Every found file keeps its own licence, recorded in the project. `veecut sources` shows them.
- Files whose rights are unclear are flagged: no licence, non-commercial, no-derivatives, fair use, GFDL only, or a credit naming someone other than NASA. `--find` never picks a flagged file, but `veecut fetch` can download one if the user picks it.
- A final render refuses flagged files nobody approved. Drafts still render.
- You never decide that a flagged source is fine. Show the user the file, its page and the reason, and let them check it. Only the user approves: ask them to press Approve in Studio's Sources tab or run `veecut sources --approve ASSET` themselves in a terminal. You cannot approve for them, even when they ask. Otherwise replace the file.
- If a file looks like a company's film, a news photo or someone's own clip, flag it even when its licence looks fine: `veecut sources --flag asset-4 --reason "looks like a news photo"`.
- Never upload or publish anything.

## More skills

`veecut skills recommend --kind documentary` lists catalog skills from the `editing` and `motion` categories. Read their README first; many need Node or a browser.
