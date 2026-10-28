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
veecut edit --kind explainer --voice inbox/narration.m4a --broll inbox/img1.jpg inbox/img2.jpg inbox/city.mp4 --json
veecut edit inbox/narration.m4a inbox/chart.png --kind explainer --copy headings.txt --json
```

An audio file among the files counts as the narration when `--voice` is not given. `--copy` takes the heading lines, separated by `\n` (`--copy "Why rent keeps rising\nWhat you can do"`), or the path to a text file with one line per heading.

## What the recipe does

- Puts the narration on a voice track and cuts its pauses (over 0.55 s with the natural pace, over 0.25 s when tight) and its stutters, false starts and high-confidence retakes.
- Shows one visual per sentence, in the order given, merging sentences shorter than 1.5 s into the next. Images get a slow pan and zoom; footage plays muted. With fewer visuals than sentences, they repeat, and long footage restarts from its beginning.
- Without any visuals, the captions become the picture: big and centered.
- Turns each `--copy` line into a heading card near the top, one per shot from the start, up to 4 s each.
- Adds a ducked music bed from `--music` or `my-sounds/music/` when the style sound is `sfx_music`, and sound effects on text unless the style is `voice_only`.
- Renders `outbox/<slug>-v1.mp4` and checks it.

## Editing rules

From Herk Edit Kit (MIT):

- Hook in the first 3 s: a claim or command that works with the sound off. No logo and no silence first.
- One line of credibility, then show the list of what is coming early, as an open loop. Each item gets a heading card, a one-line definition and visuals. End with a resolution that calls back to the hook, then the call to action.
- Text and graphics land within 2 frames of the spoken word, at most 0.15 s early.
- A graphic shows the example, the relationship, the evidence or the consequence, not a third restatement of the words.
- Every shot moves: stills get a slow push; no frozen frames.
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
| "Cut this sentence" | `cut_range` over it; the voice and the pictures move together |
| "Captions too big" | `{"op":"set","path":"/captions/size","value":0.045}` |

More in `reference/editor-api.md`.

## More skills

`veecut skills recommend --kind explainer` lists catalog skills from the `explainer` and `motion` categories. Many of them draw animated scenes with code and need Node and a browser; read their README first.
