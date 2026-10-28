# Promo

A product launch film or ad: short lines of text slammed on the beat, over product footage, ending on a call to action.

## When to use it

The user wants a short ad, teaser or launch video for a product, a course, an event or an offer, and can give you the lines it should say. A promo needs lines of copy; footage is optional.

## Ask first

Ask for these, then wait (from the launch-video skill below):

1. What is launching, in one line.
2. The one sentence the video says, split into 3 to 5 short lines. These become the `--copy` lines.
3. Proof: real screenshots, screen recordings or product footage. Only files they give you.
4. The call to action, word for word, exactly as it appears on their page. It becomes the last line, the end card.
5. A song they have the rights to, if it is not in `my-sounds/music/`.

Never invent a price, a date, a "spots left" count, a testimonial or a result.

## Command

Pass the lines with `--copy`, separated by `\n`, the call to action last. A text file with one line each works too.

```
veecut edit inbox/app-demo.mp4 inbox/dashboard.png --kind promo --copy "Your next post\nin your voice\nchecked before you see it\nJoin the beta" --music my-sounds/music/launch.mp3 --json
veecut edit inbox/app-demo.mp4 --kind promo --copy promo-lines.txt --json
```

The shape follows the style format (vertical by default); pass `--aspect 16:9` or `--aspect 4:5` for other placements.

## What the recipe does

- Finds the song's beat (or uses a steady 120 BPM grid).
- Gives each `--copy` line two bars. Lines of one to three words become a full-frame kinetic slam; longer lines become a title. Entrances alternate between a pop and a slide up.
- Shows the last line as the end card, held to the end, plus one more bar for it to land.
- Cuts the footage and images every 4 beats under the text. Footage plays muted; images get a slow pan and zoom.
- Plays the music at full level and fades it out at the end. The video is as long as the lines need, at most the length of the song.
- Adds sound effects from `my-sounds/sfx/` when text lands, unless the style is `voice_only`.
- Renders `outbox/<slug>-v1.mp4` and checks it.

## Editing rules

From Herk Edit Kit (MIT): the hook must work with the sound off; every shot moves; sound effects follow visible actions (a damped tap when a logo lands, a dry click for small UI), never a whoosh on every cut, and vary the files for repeated items.

From the launch-video skill in [charlie947/motion-graphics-skills](https://github.com/charlie947/motion-graphics-skills/blob/229022ebca76972e119e36ada23a1648d51665ab/skills/launch-video/SKILL.md) (MIT):

- A launch video sells one thing in 30 to 45 s.
- A stranger should know what is launching by second 4.
- One sentence split across the beats, each fragment landing with a short, confident move. No typewriter reveals on headlines, no glow, no bouncing.
- Every number on screen needs a source the user gave you.
- The call to action holds for at least 3 s.
- It must read on a phone with the sound off.
- Make a 16:9 and a 4:5 version: `veecut render --aspect 16:9` and `veecut render --aspect 4:5`.

From BrewReel, [Finderchangchang/promo-video-skill](https://github.com/Finderchangchang/promo-video-skill/blob/217eb1f9c6c047b908bfa95ec03f729bec1247ce/SKILL.md) (Apache-2.0), a promo skill built so that cheap models can make good vertical promos:

- 20 to 30 s is best; 15 to 45 s is the allowed range.
- The first shot is the hook (2 to 3 s), the end card is last (4 s).
- One line on screen for at most 5 s.
- At most one emphasized word or phrase per line; in VeeCut that is one pair of `*asterisks*`.
- No absolute claims such as "best", "first", "only" or "100%" unless the user can prove them.
- Use the product name exactly as the user wrote it, the same everywhere.
- Use only features the brief mentions. Copy the call to action as given; if they have none, leave it out rather than inventing one.
- English voice-over runs about 2.5 words per second, if they add narration later.

## Common notes

| Note | Fix |
| --- | --- |
| "Change this line" | `{"op":"patch","item":"text-3","patch":{"text":"Edit in *minutes*"}}` |
| "Make this word pop" | Wrap it in asterisks in the text: `"Ship *today*"` |
| "Make this line big" | `{"op":"patch","item":"text-2","patch":{"preset":"kinetic"}}` (best for one to three words) |
| "Too long" | Run the recipe again with fewer lines, or `cut_range` a line's two bars |
| "Show the logo at the end" | `veecut add logo.png --role logo`, then `add_clip` it on `broll` with a `frame` over the end card, using the printed id |
| "Use our brand color" | `{"op":"patch","item":"text-5","patch":{"style":{"accent":"#FF4F00"}}}` on each text, and save it as a rule |
| "Square version" | `veecut render --aspect 1:1` |
| "No sound effects" | `{"op":"set","path":"/tracks/5/muted","value":true}` after checking that track 5 is `sfx` |

More in `reference/editor-api.md`.

## More skills

`veecut skills recommend --kind promo` lists catalog skills from the `promo` and `motion` categories. Most build animated product scenes in code (HyperFrames, Remotion) and need Node and Chrome; read their README and ask before anything that uses a paid API.
