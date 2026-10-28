# Tutorial

Screen recordings, software tutorials and demos, with the user's voice explaining.

## When to use it

The main recording is a screen capture (with or without a small face cam) and the voice explains what happens on screen. For a narration over images with no screen recording, use `explainer`.

## Ask first

- Does the screen show anything private: emails, names, client data, API keys, tokens? VeeCut cannot blur. If it does, cut those stretches with `cut_range` or ask the user to re-record with demo data before anything leaves their computer.
- Where it goes. The default is 16:9 for YouTube; pass `--aspect 9:16` only if they really want it vertical, because small UI text is hard to read on a phone.

## Command

```
veecut edit inbox/setup-walkthrough.mov --kind tutorial --json
```

## What the recipe does

- Makes a 16:9 project and transcribes the voice.
- Cuts pauses the natural way (over 0.55 s, or 0.25 s when the style pace is `tight`) and cuts stutters, false starts and high-confidence retakes, keeping the last take.
- Keeps silent stretches of 2 s or more, because the viewer may need to see them (typing, loading), but plays them 2 to 8 times faster and muted, with an "N× SPEED" label.
- Shows plain subtitles low in the frame (0.88 of the height), not in capitals, unless the style turns captions off.
- Uses each `--broll` file (for example a slide or a photo of the result) once as a full-frame cutaway of at most 3 s, muted, at a sentence start after the first 3 s, spread evenly; stills get a slow push-in. In a tutorial the screen usually matters more, so pass B-roll only when the user asks.
- Adds a music bed and sound effects only as the style asks (see `workflows/talking-head-short.md`, relative to the loaded `SKILL.md` directory, for the levels).
- Renders `outbox/<slug>-v1.mp4` and checks it.

## Editing rules

From Herk Edit Kit (MIT): cut between phrases at real silence, never inside a phrase; never speed the voice up; text lands within 2 frames of the word that names it; keep labels clear of the platform's buttons.

From the narrated screen tutorial recipe in [kmilo93sd/agent-video-editor](https://github.com/kmilo93sd/agent-video-editor/blob/8e32c0aff64c445ef1e2b87bc04408a34d68c973/skills/editing-videos/recipes/narrated-screen-tutorial.md) (MIT). The viewer repeats the steps with the app open next to the video, so they need time:

- The screen leads and the voice follows it. A phrase comes in when what it describes happens on screen.
- Play navigation, clicks and results at normal speed. Typing can run at up to 1.5x. Remove dead waits (loading, searching for a button) with clean cuts. That recipe found 2 to 5x speed-ups hard to follow; if the user says the fast parts fly by, slow them down (notes below) or cut them.
- Shots of at least 5 s, and never cut in the middle of an app animation such as a modal opening.
- Never show a form jumping half-filled.
- Zoom only on the detail the voice names: up to 1.10x, at least 1.5 s to zoom in, at most one zoom every two phrases, always within one shot.
- At least 0.4 s between phrases. Over 3 s of silence needs a reason, and 6 s is the limit unless the screen shows a self-explanatory action.
- On-screen text only for what is typed or searched for: a menu path, a key, an expanded acronym. Do not repeat what the voice says.
- Open with the end result in the first 10 to 20 s. Give each chapter a 3 s title card ("Step 2 of 5"). Keep blocks under 6 minutes.
- No music by default; music is for the intro, the title cards and the end. No sound effect on real clicks and no whoosh on cuts inside a chapter.

From [gquthier/autonomous-sop-video](https://github.com/gquthier/autonomous-sop-video/blob/19a7f7f7a2d91554968c5c03a97c71c4a69b15bc/SKILL.md) (MIT):

- One action or decision per step: where to go, what to click, what to check and what success looks like.
- Start with the visible result and what the viewer needs before starting. End with two or three success checks and the most likely fix if something goes wrong.
- Keep the whole control label visible when zooming, and show the section name before zooming into the exact control.
- Do not narrate a click the video cannot show.
- A caption must not outlast the step it belongs to.
- The last frame holds a useful result or checklist, never an accidental black frame.

## Common notes

| Note | Fix |
| --- | --- |
| "The fast part is too fast" | Lower that clip's speed and lengthen it to match: a 4 s clip at 6x covers 24 s, so at 3x it needs `{"op":"patch","item":"clip-5","patch":{"speed":3,"duration":8}}`. Then patch the "6× SPEED" label: `{"op":"patch","item":"text-3","patch":{"text":"3× SPEED","duration":8}}`. |
| "Just cut the waiting" | `delete` the sped-up clip and its label |
| "Zoom in on this button" | `{"op":"add_zoom","at":42.0,"duration":3.0,"scale":1.1,"ease":1.5,"focus":{"x":0.8,"y":0.2}}` with `focus` on the button |
| "Subtitles too small" | `{"op":"set","path":"/captions/size","value":0.05}` |
| "Subtitles cover the toolbar" | `{"op":"set","path":"/captions/position","value":0.1}` if the toolbar is at the bottom |
| "Add a step card" | `{"op":"add_text","text":"STEP 2/5","start":30.0,"duration":3.0,"preset":"counter"}` |
| "Show the shortcut" | `{"op":"add_text","text":"Cmd+Shift+P","start":51.0,"duration":2.0,"preset":"label"}` |
| "Subtitle file for YouTube" | `veecut transcript --srt` |

More in `reference/editor-api.md`, relative to the loaded `SKILL.md` directory, never the workspace root.

## More skills

`veecut skills recommend --kind tutorial` lists catalog skills from the `explainer` and `editing` categories.
