# Notes and rules

This loop comes from free247editor and Herk Edit Kit (both MIT): the user watches a draft, says what to change in plain words, and you fix it. VeeCut Studio previews the change; outside Studio, render the next version. When a note will hold for every video, also save it as a rule.

## One round

1. Read `my-rules.md` before every edit (`veecut rules`). Follow every rule in it.
2. Take the user's notes one at a time. Repeat each back in a few words if it is unclear ("Yung captions, mas mataas?"), and ask only when you cannot tell what they mean.
3. For quoted words, run `veecut transcript-search "PHRASE" --json`, then `veecut cut-phrase "PHRASE"`. Multiple matches require a 1-based `--occurrence N` chosen by the user. Do not calculate phrase cuts from rounded timestamps. For other notes, apply ops with `veecut ops`; read `reference/editor-api.md` relative to the loaded `SKILL.md` directory, never the workspace root. Pin a note about one moment with `veecut note "THE NOTE" --at T` if useful.
4. Decide whether the note is lasting (below). If it is, save it as a rule.
5. For a Studio note, optionally check `veecut frame` in the project's `.cache/`, answer in one or two lines and stop. Render or open a video only when the note asks for one. Outside Studio, run `veecut render`, open the next draft (`-v2`, `-v3`) and say what changed.
6. When the user approves ("ok na", "approved", "final"), run `veecut render --final`.

## When a note becomes a rule

A note is lasting when any of these is true:

- It says always or never ("always put captions lower", "huwag na ng music kahit kailan").
- It is about their style rather than this one video: caption look, pace, colors, music, how the hook should sound.
- The same kind of note comes a second time, even in other words.

A note about one moment ("cut 0:05 to 0:07", "this joke doesn't land") is not a rule. Fix it and move on.

Save a rule with the rule written as an instruction, plus the user's own words:

```
veecut rules add "Captions in the lower third on every video" --words "lagay mo lagi sa baba yung captions"
```

It appends a line to `my-rules.md` with the rule, the user's words and the date. `veecut note "THE NOTE" --rule` pins the note and saves it as a rule in one step, but it stores the note as the rule word for word, so use `veecut rules add` when the note needs rewording.

Then confirm in one line, in the user's language: "Got it, captions go in the lower third from now on." or "Noted, wala nang music sa lahat ng video mo."

If the rule matches a style setting, change the style too, so recipes follow it from the start: `veecut style set captions.position lower`.

## What wins

Rules beat the style, and the style beats the recipe defaults. If `my-rules.md` says "no music" and `my-style.toml` says `sound = "sfx_music"`, there is no music. If a new note contradicts an old rule, ask once which one holds, then save the new rule and tell the user they can delete the old line in `my-rules.md`.

## Examples

| The user says | Lasting? | Rule to save |
| --- | --- | --- |
| "Always cut out my 'um's." | Yes, says always | Cut every filler word, even short ones |
| "Bigger captions." (second time this week) | Yes, came twice | Captions size large on every video |
| "Mas mabilis yung pacing, lagi." | Yes, says lagi (always) | Tight pace: cut every pause and breath |
| "Ayoko ng music sa background." | Yes, about their style | No music bed on any video |
| "Gawin mong yellow yung highlight, brand color ko yun." | Yes, about their style | Highlight color #FFD400 on captions and hook text |
| "Remove the part where I cough at 1:12." | No, one moment | none |
| "Yung hook, palitan mo ng 'Stop paying for editors'." | No, this video's hook | none |
| "Never put text over my face." | Yes, says never | Keep all text and captions off the face |

For the brand color example, also run `veecut style set captions.highlight "#FFD400"`. For "bigger captions", run `veecut style set captions.size large`.
