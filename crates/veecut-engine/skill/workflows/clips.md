# Clips

One long recording (a talk, a podcast episode, a livestream, a long YouTube video) cut into several short vertical clips.

## When to use it

The user wants Reels, TikToks or Shorts from something long. For a single short recording, use `talking-head-short`.

## Ask first

- How many clips they want. The default is 3.
- Whether they already know a moment they want. If so, you can still run the recipe and tell them which clip matches, or cut that moment yourself after.

## Command

```
veecut edit inbox/webinar.mp4 --kind clips --clips 5 --json
```

Leave out `--hook` here: it would put the same title on every clip. Fix titles one clip at a time afterwards.

## What the recipe does

- Transcribes the long video once and plans its cuts once.
- Finds the strongest self-contained moments of 20 to 60 s. A moment starts and ends on a sentence and scores higher for a strong first sentence (a question, a number, a bold claim, talking to "you"), for the video's topic words, for lively delivery and for ending on a full sentence. Openings like "and" or "it" score lower.
- Makes one vertical project and one draft per moment, best first, with tight cuts, a face crop, 1.15x punch-ins (no shot past 2.8 s) and captions. The opening hook uses a complete short sentence from that moment; if none fits, it is omitted with a review item.
- Adds a ducked music bed only from `--music` or when the style sound is `sfx_music`, and sound effects on text unless the style is `voice_only`.
- The summary lists each clip with its length, score and why it scored, for example "opens with a question; topic words: pricing, clients".

## Working with several projects

Every clip is its own project in `projects/`. Commands use the most recently changed project unless you pass `-p` with a slug, a project folder or a `project.json` path:

```
veecut status -p SLUG --json
veecut ops -p SLUG '{"op":"cut_range","from":0,"to":1.2}'
veecut render -p SLUG
```

SLUG is the folder name under `projects/`, from the `projects` list that `veecut edit` printed. Slugs end with the date, like `webinar-clip-2-2026-10-28`. Show the user all drafts, name them by number and title, and take notes per clip.

## Editing rules

All the short-form rules in `talking-head-short.md` (from Herk Edit Kit, MIT) apply. These matter most for clips:

- The clip must make sense to someone who never saw the long video. If it opens mid-thought, cut to the next full sentence.
- Hook in the first 3 s, and it must work with the sound off. The picked title is the moment's first words; rewrite it as a claim or a command when it is weak.
- Speech starts within 0.1 s. Cut any lead-in.
- Shots average 1.1 to 1.4 s and none runs past about 2.8 s.
- Captions 1 to 3 words per page, never over the mouth or chin.
- When the source was horizontal, keep labels below the top 130 px of the 1080x1920 frame, and inside x 90 to 930, y 200 to 1600.
- End on a full sentence, never mid-thought.

## Common notes

| Note | Fix |
| --- | --- |
| "Clip 2 starts too late" | In clip 2's project, put back the lead-in with `add_clip` from the source time on `main` with `"index":0` (see `veecut transcript -p SLUG`). Everything after it moves along, the hook too; move the hook back with `{"op":"move","item":"text-1","start":0}` if it should still open the clip. |
| "Cut the first sentence" | Read it with `veecut transcript -p SLUG --json`, search its text with `veecut transcript-search "SENTENCE" -p SLUG --json`, then `veecut cut-phrase "SENTENCE" -p SLUG`. If it repeats, select the matching 1-based `--occurrence N`. |
| "Better title" | `patch` the hook text item: `{"op":"patch","item":"text-1","patch":{"text":"The *one* pricing mistake"}}` |
| "Make 3 more" | `veecut edit SAME-FILE --kind clips --clips 6`. It reuses the transcript, but makes all six again, so the first three repeat the earlier ones. |
| "I don't like clip 3" | Leave its project and draft alone and say so; nothing needs deleting |
| "Horizontal too" | `veecut render -p SLUG --aspect 16:9` |

More in `reference/editor-api.md`, relative to the loaded `SKILL.md` directory, never the workspace root.

## More skills

`veecut skills recommend --kind clips` lists catalog skills from the `editing` and `shorts` categories.
