# Explainer

A faceless explainer: a recorded narration over images or footage, with captions and heading cards.

## When to use it

The user has a voice recording (or a video whose sound is the narration) and pictures to go with it, or only the voice. Nobody talks to camera. For a screen recording with a voice, use `tutorial`.

## Ask first

- Which file is the narration, and which files are the visuals, if that is not obvious.
- Whether they have heading lines for the cards, one per line, in the order they come up.
- Shape, if the style does not say: the recipe follows the style format (vertical by default). Pass `--aspect 16:9` for YouTube.

## Command

```
veecut edit --kind explainer --voice inbox/narration.m4a --broll inbox/img1.jpg inbox/img2.jpg inbox/city.mp4 --motion glide --music my-sounds/music/bed.mp3 --json
veecut edit inbox/narration.mp4 --kind explainer --broll inbox/chart.png inbox/demo.mp4 --motion hold --copy headings.txt --no-render --json
```

`--voice` takes precedence. Without it, an audio source supplies narration; if there is no audio source, the first video supplies its sound. Put visuals in `--broll` in story order. `--copy` takes heading lines separated by `\n` (`--copy "Why rent keeps rising\nWhat you can do"`), or a text file with one heading per line.

Other source visuals come first, followed by `--broll` in its supplied order. When the first source video supplies narration, it is excluded from the visuals if B-roll is supplied; without B-roll, its picture remains as a muted visual.

`--motion` is explainer-only: `hold` adds no camera move, `push` smoothly zooms from 1.0x to 1.12x, and `glide` combines a small smooth zoom with alternating focus movement. These choices apply to images and muted video. Omit the flag to keep the recipe's original motion. Studio calls this choice Recipe default.

## What the recipe does

- Puts the narration on a voice track and cuts its pauses (over 0.55 s with the natural pace, over 0.25 s when tight) and its stutters, false starts and high-confidence retakes.
- Shows one visual per sentence, in the order given, merging sentences shorter than 1.5 s into the next. Visuals repeat when needed to cover narration, and the review says how many shots reuse a visual (more visuals look better). A short video repeats within its sentence without moving the next shot boundary; footage plays muted. Motion follows `--motion`, or the original recipe when omitted. `glide` starts slightly zoomed so its pan never hits the picture edge.
- Pictures are cropped to fill the frame. When a crop throws away more than 30% of a picture, the review names it and gives the patch that shows it whole instead (`{"fit":"contain","ken_burns":null}`).
- Without any visuals, the captions become the picture: one large word at a time, centered, on a dark slate background.
- Puts each `--copy` heading on the shot where the narration says it (matching spoken phrases or topic words, in order), each ending before the next heading. A heading that matches nothing is left out and named in the review so you can place it with `add_text`.
- In 16:9, captions default to the lower part of the frame instead of the upper third.
- Adds a ducked music bed from `--music` or `my-sounds/music/` when the style sound is `sfx_music`, and sound effects on text unless the style is `voice_only`.
- Renders `outbox/<slug>-v1.mp4` and checks it, unless `--no-render` is set.

## Produce and review

1. Write a short original script with one factual claim per beat. Check claims against sources; keep uncertain details qualified. Plan each beat's spoken line, visual and heading before recording. Do not reuse a pack's sample story as verified history.
2. Record your own narration locally, then choose your own or licensed pictures, clips and music. Keep source pages and credit requirements; see `reference/sourcing.md`. Recording and native editing need no paid generation service. Local transcription downloads its model on first use; check transcription settings because configured Groq uploads audio.
3. Order visuals by narration beat. Use `hold` for diagrams or text that needs reading, `push` for emphasis, or `glide` for a gentle change of framing. Motion is optional; check crops instead of making every shot move.
4. Read the recipe's review items and inspect `veecut frames -p SLUG`. Use `veecut frame -p SLUG --at T` at each beat's start, middle and end. Check the final visual, caption timing, repeated footage and safe areas. Fix through ops; never overwrite originals.
5. Outside Studio, render with `veecut render -p SLUG`, watch the whole draft and listen for clear narration over music. After user approval, run `veecut render -p SLUG --final`. In Studio, follow the router's preview-first rule and export only when requested.

VeeCut supplies native image/video editing and camera motion. Generated characters, 3D sets and depth/parallax generation remain external; import their finished, rights-cleared clips as ordinary B-roll. Do not install an unknown root skill or run its scripts to complete this workflow.

## Editing rules

From Herk Edit Kit (MIT):

- Hook in the first 3 s: a claim or command that works with the sound off. No logo and no silence first.
- One line of credibility, then show the list of what is coming early, as an open loop. Each item gets a heading card, a one-line definition and visuals. End with a resolution that calls back to the hook, then the call to action.
- Text and graphics land within 2 frames of the spoken word, at most 0.15 s early.
- A graphic shows the example, the relationship, the evidence or the consequence, not a third restatement of the words.
- The original playbook asks for movement in every shot. For this workflow, deliberate holds are valid; choose motion to suit the material.
- The music bed sits about 11 dB under the voice, ducked under speech, quieter in the first 1 to 2 s.

From [vincentsch/explainroo](https://github.com/vincentsch/explainroo/blob/e95232c8d535ed2aa390e7b1caee0d1326dd9e83/AGENTS.md) (MIT), useful when the user asks for help with the script before recording:

- Sentences of about 8 to 18 words, one idea each. A finished video runs about 150 words a minute, pauses included.
- Start with the question or problem, explain it in small steps, give one concrete example, then sum up.
- One idea per scene, usually 1 to 3 sentences (4 to 15 s). Something on screen changes every 2 to 4 s.
- Keep screen text short: labels, numbers and key phrases. When captions are on, do not also show the sentence the voice is saying as text.
- At most five or six things on screen at once.

## Common notes

| Note | Fix |
| --- | --- |
| "Wrong picture here" | Find the clip at that time in `veecut status --json` and `patch` its `asset` to the right one, with `"src_in":0` for a video |
| "Hold this image longer" | That adds a pause to the narration. Split the voice where the image ends, `{"op":"split","at":18.0,"track":"voice"}`, then trim the image clip's end later, `{"op":"trim","item":"clip-4","end":19.5}`. Everything after it, the rest of the voice included, moves along. Check with `veecut frames`. |
| "Zoom in more on the images" | `patch` the image clips: `{"ken_burns":{"from_scale":1.0,"to_scale":1.2}}` |
| "Add a heading here" | `{"op":"add_text","text":"Step 2: *compare*","start":18.0,"duration":3.0,"preset":"card"}` |
| "Show the number big" | `{"op":"add_text","text":"₱12,000","start":24.0,"duration":2.5,"preset":"counter"}` |
| "Cut this sentence" | `veecut transcript-search "SENTENCE" --json`, then `veecut cut-phrase "SENTENCE"`. If it repeats, ask which match and pass `--occurrence N`. Voice and pictures ripple together. |
| "Captions too big" | `{"op":"set","path":"/captions/size","value":0.045}` |

More in `reference/editor-api.md`, relative to the loaded `SKILL.md` directory, never the workspace root.

## More skills

`veecut skills recommend --kind explainer` lists catalog skills from the `explainer` and `motion` categories. Many of them draw animated scenes with code and need Node and a browser; read their README first.

The reviewed animated-shorts and Faceless Videos v2 packs informed beat planning, narration-first visual order and restrained motion. Their licences, source identities and limits are disclosed in `reference/sourcing.md`; neither pack is installed or bundled.
