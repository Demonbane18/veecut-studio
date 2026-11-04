# Vlog

A trip, a day or an event: footage and photos cut to a song.

## When to use it

The user has a pile of clips and maybe photos, and wants them cut to music. If most of it is them talking to camera, use `talking-head-short` or `talking-head-long` instead.

## Ask first

- Which song. Pass it with `--music`, include it among the files, or put it in `my-sounds/music/`. Remind them to use music they have the rights to.
- A title, if they want one (the first `--copy` line).
- The order they want, if it matters. The recipe uses the files in the order you pass them.
- Shape, if the style does not say: the recipe follows the style format (vertical by default). Pass `--aspect 16:9` for YouTube.

## Command

```
veecut edit inbox/beach1.mp4 inbox/beach2.mp4 inbox/sunset.jpg inbox/dinner.mp4 --kind vlog --music my-sounds/music/summer.mp3 --copy "Siargao 2026" --json
```

## What the recipe does

- Finds the song's beat and cuts the footage on it, 2, 4 or 8 beats per shot, whichever comes closest to 2 s. Without a clear beat it uses a steady 120 BPM grid.
- Takes the files in turn and uses each stretch of footage once, starting past each clip's opening and skipping black or dark stretches. A photo is used once, with a slow pan and zoom. Footage plays muted. If the footage runs out before the song ends, the review says so.
- Adds a 0.3 s crossfade every 8 shots, fades in the first shot and fades out the last.
- Shows the first `--copy` line as a title at the start.
- Plays the music at full level under everything and fades it out at the end.
- Ends the video when the footage runs out, even if the song goes on; the review list says so. More footage makes it longer.
- Renders `outbox/<slug>-v1.mp4` and checks it.

## Editing rules

From Herk Edit Kit (MIT): use every B-roll scene once per video; every shot moves (stills get a slow push); sound effects follow visible actions, not every cut.

From the vlog recipe in [kmilo93sd/agent-video-editor](https://github.com/kmilo93sd/agent-video-editor/blob/8e32c0aff64c445ef1e2b87bc04408a34d68c973/skills/editing-videos/recipes/vlog.md) (MIT):

- Open cold on the best or most surprising moment, a few seconds long, before any introduction. It must be a moment that is really in the video.
- Then the setup: where we are and what is going to happen. Then the story in beats, and cut whatever does not move it, however pretty.
- Establish the place (the street, the sign) before the close-ups.
- Sequences of 3 to 5 shots for one action (wide, medium, detail) read better than one long handheld take. Cut on action.
- Film averages about 4 to 5 s per shot; use it as a reference for slower sections.
- Under speech, music sits 18 to 25 dB under the voice and ducked. In montages without speech it can lead.
- Cut music on its phrases and use the downbeats for the key cuts.
- Music playing at the location (a café, a shop) can trigger copyright claims. Mute that clip or cover it.
- People in the background did not agree to be filmed. Ask the user about identifiable bystanders, plates or personal data, since VeeCut cannot blur them; cut those shots if needed.

From [znyupup/ai-video-editing-skill](https://github.com/znyupup/ai-video-editing-skill/blob/b6429ab550a64c595dc68d42c47cf2b489a50619/SKILL.md) (MIT), a travel vlog pipeline:

- Shots of about 3 to 4 s suit a travel vlog; short shots under 2 s are for montage moments, long shots over 8 s for telling a story.
- Three acts: about 25% setup and suspense, 50% the trip, 25% the close. Put the best shots first.
- An opening montage of 6 to 10 shots, 0.5 to 1 s each, 5 to 8 s in total, works as a cold open.
- Do not mix vertical clips into a horizontal video; leave them out.
- Stretches with no speech over 6 s feel long; trim them to 5 to 8 s.
- Music level as a linear gain: 0.08 to 0.12 under a lot of speech, 0.15 to 0.25 when it is mostly scenery, 0.3 to 0.4 in a pure montage.
- Titles as white text with a soft shadow over the picture, not black title cards.
- Don't cut anyone off mid-sentence. When two songs follow each other, keep their tempos within 20 BPM.

## Common notes

| Note | Fix |
| --- | --- |
| "I want to hear the sound in this clip" | Unmute it: `{"op":"patch","item":"clip-7","patch":{"muted":false}}`. The music is one long item, so to lower it only there, split it at the clip's start and end (`{"op":"split","at":14.0,"track":"music"}` and again at the end), then patch the middle piece: `{"volume":0.12}`. |
| "Too fast" / "too slow" | The recipe picks 2, 4 or 8 beats per shot from the tempo. For faster cutting in one section, `split` long shots on a beat and `patch` the second half to other footage (`asset` and `src_in`). For the whole video, a slower or faster song changes it. |
| "Use the beach clip first" | `{"op":"move","item":"clip-9","index":0}` |
| "Drop this shot" | `{"op":"delete","item":"clip-4"}` also cuts that stretch out of the music, so the song jumps ahead. To keep the song whole, swap the shot instead: `{"op":"patch","item":"clip-4","patch":{"asset":"asset-5","src_in":12.0}}` with footage that is not used elsewhere. |
| "Longer video" | More footage. Run the edit again with the extra files. |
| "Add a title card for each place" | `{"op":"add_text","text":"Day 2: *Cloud 9*","start":14.0,"duration":2.5,"preset":"title"}` |
| "Warmer colors" | `{"op":"set","path":"/look/preset","value":"warm"}` |
| "Music fades too early" | `patch` the music item's `fade_out`, for example `{"fade_out":3.0}` |

More in `reference/editor-api.md`, relative to the loaded `SKILL.md` directory, never the workspace root.

## More skills

`veecut skills recommend --kind vlog` lists catalog skills from the `editing` and `motion` categories.
