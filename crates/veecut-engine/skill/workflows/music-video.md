# Music video

A song with footage cut on the beat, with the lyrics on screen.

## When to use it

The user has a song (theirs, or one they have the rights to) and footage or photos for it, and wants a music video or a lyric video. For a vlog where the song is background, use `vlog`.

## Ask first

- The song file, if it is not among the files or in `my-sounds/music/`.
- The lyrics. Always ask for the official lyrics; transcription is the fallback when nothing else exists. Timed lyrics in LRC format work best, one line each:

  ```
  [00:12.40] Walking down the empty road
  [00:16.85] Every light is turning gold
  ```

  If they only have plain lyrics, the recipe transcribes the song instead, which is less reliable on sung words.
- The credit line they want shown, for example "Music and lyrics: NAME". Ask for it word for word.

## Command

Write the LRC lyrics to a text file (outside `inbox/`), then:

```
veecut edit inbox/clip1.mp4 inbox/clip2.mp4 inbox/clip3.mp4 --kind music-video --music inbox/song.wav --copy lyrics.lrc --json
```

The shape follows the style format (vertical by default); pass `--aspect 16:9` for YouTube.

## What the recipe does

- Finds the song's beat (or uses a steady 120 BPM grid) and cuts the footage every 1, 2 or 4 beats, whichever comes closest to 1.5 s.
- Uses each stretch of footage once, taking the files in turn. Footage plays muted; photos get a slow pan and zoom. The video ends when the footage runs out, even if the song goes on; the review list says so.
- Lyrics: with `--copy` in LRC format, each line becomes a caption page at its time, held until the next line (at most 6 s), with its words spread evenly across it. Without LRC lyrics it transcribes the song and captions what it hears; if it finds no clear vocals, the review list says so.
- Plays the song at full level and fades it out at the end.
- Renders `outbox/<slug>-v1.mp4` and checks it.

## Editing rules

From Herk Edit Kit (MIT): every shot moves (photos get a slow push); use each piece of footage once; text never sits over the mouth or chin.

From [BuzyOwl/song-video-director](https://github.com/BuzyOwl/song-video-director/blob/c0f475271867ac68d2c46886aac1befd49070ec5/skill/song-video-director/SKILL.md) (MIT):

- Listen before you plan. Check the tempo the edit summary reports against the tempo you feel: the beat finder can land on half or double it.
- The tempo decides the motion. A 63 BPM ballad breathes on the bar; a 130 BPM track can cut on the beat.
- A hook in the first 1.5 s.
- Stillness is a tool: quiet sections get almost no effects, so the peaks land.
- Text never covers a face. Type needs a minimum readable size on a phone and one key word per line in the accent color.
- A returning chorus keeps its motif but gets new staging; never replay the first chorus exactly.
- Machine-timed lyrics are not ear-checked. Say so every time you show a draft with transcribed lyrics.
- Credit honestly: who wrote and sang the song, and which parts AI tools made.
- For timestamped notes ("1:26 looks odd"), look at that frame (`veecut frame --at 86`), name the real cause, then fix it.

From [EGSECDA/vocaloid-style-mv-pipeline](https://github.com/EGSECDA/vocaloid-style-mv-pipeline/blob/13b380dab077d4c8ea956825c9693c720e0c2b91/skills/vocaloid-style-mv/SKILL.md) (MIT):

- Lyrics appear about 0.2 s before the voice.
- Cuts land on downbeats; the big moments go on chorus downbeats, stops and post-chorus hits.
- Restraint, then bursts: quiet verses with long holds and one effect at a time make the choruses hit. The last chorus brings back earlier ideas plus one new move.
- Credit the artist on screen with the exact credit line they give you.

## Common notes

| Note | Fix |
| --- | --- |
| "The lyrics come late" | Move the caption pages earlier, for example 0.2 s: `{"op":"patch","item":"cap-4","patch":{"start":15.65}}` for each page. For the whole song, fix the LRC times and run the recipe again. |
| "Wrong words in the lyrics" | Fix the LRC file and run the recipe again. With transcribed lyrics, ask for the real lyrics as LRC. |
| "Lyrics bigger" / "higher" | `{"op":"set","path":"/captions/size","value":0.06}`, `{"op":"set","path":"/captions/position","value":0.3}` |
| "Lyrics in a different style" | `veecut captions --mode subtitle` (or `highlight`, `pills`, `word_pop`) |
| "Add the credit at the end" | `{"op":"add_text","text":"Music and lyrics: NAME","start":170.0,"duration":4.0,"preset":"lower_third"}` with the exact credit line |
| "Use this clip in the chorus" | `{"op":"move","item":"clip-12","index":20}`, then check the cut lands on the beat with `veecut frames` |
| "The video stops before the song ends" | More footage; run the recipe again with the extra files |
| "More cuts in the chorus" | `split` shots there on the beat; times come from the beat at the tempo the summary reports |

Do not run `veecut captions --rebuild` on a music video with LRC lyrics: it rebuilds from a speech transcript and would replace the lyric pages.

More in `reference/editor-api.md`.

## More skills

`veecut skills recommend --kind music-video` lists catalog skills from the `music` category. Most draw animated music videos in code and need Node, Chrome or Blender; read their README first.
