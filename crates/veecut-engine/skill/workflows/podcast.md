# Podcast

Podcasts and interviews: two or more people talking, filmed with one to four cameras.

## When to use it

The recording is a conversation. With one camera, the recipe edits it like a calm talking head. With two to four cameras, each with its own microphone, it syncs them and cuts to whoever is speaking. For short vertical clips from an episode, run `clips` on it afterwards.

## Ask first

- How many camera files there are, and whether there is a separate microphone or mixer recording. Pass the cameras as files and the separate recording with `--voice`.
- The names and roles of the people, if they want lower thirds.

## Command

```
veecut edit inbox/ep7.mp4 --kind podcast --json
veecut edit inbox/ep7-cam-a.mp4 inbox/ep7-cam-b.mp4 --kind podcast --voice inbox/ep7-mixer.wav --json
```

## What the recipe does

- Makes a 16:9 project and transcribes the speech (the `--voice` file when given, else the first camera).
- Cuts pauses over 0.8 s with the natural pace (over 0.25 s when the style pace is `tight`), and stutters, false starts and high-confidence retakes, keeping the last take.
- One camera: gentle 1.08x punch-ins about every 8 s.
- Two to four cameras, each with its own sound: syncs them by sound, then cuts to whoever is speaking, with shots of at least 2 s. One continuous voice track plays under the edit (the `--voice` recording, or the first camera's sound), and the camera sound is muted.
- Uses each `--broll` file once as a full-frame cutaway of at most 3 s, muted, at a sentence start after the first 3 s, spread evenly; stills get a slow push-in.
- Adds captions in the style's mode, moved off the face, a ducked music bed only as the style asks, and sound effects on text unless the style is `voice_only`.
- Renders `outbox/<slug>-v1.mp4` and checks it.

## Editing rules

From Herk Edit Kit (MIT): cut between phrases at real silence, never inside a phrase; never speed the voice up; keep at least about 0.08 s after a word ends; text lands within 2 frames of the word.

From the podcast and interview recipe in [kmilo93sd/agent-video-editor](https://github.com/kmilo93sd/agent-video-editor/blob/8e32c0aff64c445ef1e2b87bc04408a34d68c973/skills/editing-videos/recipes/podcast-to-youtube.md) (MIT):

- Open cold with the strongest exchange of the episode, 15 to 60 s, cut tight. Then a short intro: who is talking and about what, in one or two sentences. No long branded intro.
- Split the conversation into topics, one chapter each. YouTube chapters start at 00:00, there are at least 3, and each lasts 10 s or more. Name them after the topic, not "Part 2".
- Close with the end screen in the last 5 to 20 s.
- Show who is talking, in a close-up, for most of their turn. Cut on the speaker change. Cut on a timer and you get angle changes nobody asked for.
- Reactions are content when they say something (a laugh, a surprised face at the key line), not for every "mm-hm".
- The wide shot re-establishes the room: at the start, after a topic change, during fast cross-talk.
- Remove dead air, false starts, off-topic stretches and technical problems, but keep the natural pauses. An interview cut like a talking head stops sounding like a conversation.
- Never join the start of one answer to the end of another in a way that changes what someone meant.
- Music only for the intro, the outro and topic breaks, not under the conversation.
- Speakers at the same level. -14 LUFS and -1 dBTP suit YouTube. A separate master for podcast feeds sits at -16 LKFS ± 1 (Apple Podcasts).
- Names and roles on lower thirds the first time each person appears.

## Common notes

| Note | Fix |
| --- | --- |
| "Stay on me here", "wrong camera" | Swap the camera on that clip: patch its `asset` to the other camera and shift `src_in` by the difference between the two cameras' offsets. The edit summary lists them, for example `offsets [0.0, 1.52] s` in the same order as the camera files. A clip on the first camera at `src_in` 40.0 becomes `{"op":"patch","item":"clip-12","patch":{"asset":"asset-2","src_in":41.52}}`. Check with `veecut frame --at T`. |
| "Keep more of the pauses" | With the natural pace the recipe already keeps pauses up to 0.8 s; check that the style pace is not `tight`. With one camera and no `--voice`, `trim` the clip before the pause outward. With a separate voice track, trim the main clip outward (that makes room on every track), then trim the voice clip that ends at the same moment outward by the same amount so the voice fills it. |
| "Add our names" | `{"op":"add_text","text":"Ana Reyes\nHost","start":3.0,"duration":4.0,"preset":"lower_third"}` |
| "Add chapters" | A `card` text at each topic start, and list the times for the YouTube description from `veecut transcript` |
| "Cold open" | Insert the strongest exchange at the start: `add_clip` of that camera piece on `main` with `"index":0` (later items on every track move along), then the matching voice piece on the `voice` track with `"start":0`. The voice `src_in` is the speech time from `veecut transcript`; the picture's `src_in` is that time plus the camera's offset. Check with `veecut frames`. If it gets fiddly, offer a separate teaser: `veecut edit SAME-FILE --kind clips --clips 1`. |
| "No music under us talking" | `{"op":"set","path":"/tracks/4/muted","value":true}` after checking that track 4 is `music` |
| "Make it for podcast apps too" | After the YouTube final, `{"op":"set","path":"/audio/target_lufs","value":-16}`, then `veecut render --final --out outbox/<slug>-podcast-feed.mp4`, then set it back to -14 |
| "Clips for Shorts" | `veecut edit SAME-FILE --kind clips --clips 5` |

More in `reference/editor-api.md`.

## More skills

`veecut skills recommend --kind podcast` lists catalog skills from the `editing` and `shorts` categories.
