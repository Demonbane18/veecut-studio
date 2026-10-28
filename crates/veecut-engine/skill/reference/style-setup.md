# Style setup

Style setup finds out how the user likes their videos, once, so every edit starts close to what they want. It follows free247editor's setup (MIT): two or three rounds of short multiple-choice questions, then a 5-second sample on their own footage.

## How to ask

Ask in two or three rounds, a few questions per round. Use your agent's question tool if it has one. Otherwise list the options as A, B, C so the user can answer with letters ("1A 2C 3B"). Mark the default in each question. Ask in the user's language. If they say "ikaw na bahala" or "you pick", keep the defaults.

Round 1, the shape and the feel:

1. Format: A) vertical 9:16 for Reels, TikTok and Shorts (default), B) horizontal 16:9 for YouTube, C) both.
2. Pace: A) tight, cut every breath and pause, B) natural, keep short pauses (default).
3. Hook: A) big text in the first 3 seconds (default), B) no hook text.

Round 2, captions:

4. Caption style: A) word by word, B) a short phrase with the spoken word highlighted (default), C) key words in colored pills, D) full-sentence subtitles, E) no captions.
5. Position: A) upper third, clear of the app buttons (default), B) middle, C) lower third.
6. Colors: A) white with a shadow (default), B) their brand color (ask for it as #RRGGBB), C) a highlight color on key words (ask which).

Round 3, the finish:

7. Graphics: A) minimal, B) some motion cards (default), C) heavy motion graphics.
8. Color look: A) natural (default), B) warm, C) cool, D) black and white, E) teal and orange. The style also knows `vivid` and `matte` if they ask for something else.
9. Sound: A) voice only, B) light sound effects (default), C) sound effects plus a quiet music bed.
10. Language they speak in their videos: English, Tagalog, Taglish or another one. Taglish or unsure means `auto`.

## Saving the answers

Set each answer with `veecut style set KEY VALUE`, one command per key. Dotted keys reach the `[captions]` section.

```
veecut style set format vertical
veecut style set pace tight
veecut style set captions.mode highlight
veecut style set captions.position upper
veecut style set captions.highlight "#FFD400"
```

`veecut style show` prints the whole file. These are all the keys in `my-style.toml`:

| Key | Values |
| --- | --- |
| `format` | `vertical`, `horizontal`, `both` |
| `hook` | `true`, `false` |
| `pace` | `tight`, `natural` |
| `graphics` | `minimal`, `some`, `heavy` |
| `look` | `natural`, `warm`, `cool`, `black_white`, `teal_orange`, `vivid`, `matte` |
| `sound` | `voice_only`, `sfx`, `sfx_music` |
| `language` | `auto`, or a code such as `en` or `tl` |
| `captions.mode` | `word_pop`, `highlight`, `pills`, `subtitle`, `off` |
| `captions.position` | `upper`, `middle`, `lower`, or a number: the vertical center as a fraction of the height, like `0.3` |
| `captions.color` | `#RRGGBB` |
| `captions.highlight` | `#RRGGBB`, the color of the spoken or key words |
| `captions.size` | `small`, `medium`, `large`, or a number: the text height as a fraction of the frame height, like `0.05` |
| `captions.font` | a family from `veecut fonts` |
| `captions.uppercase` | `true`, `false` |

How the answers map: word by word is `word_pop`, the highlighted phrase is `highlight`, key-word pills is `pills`, full sentences is `subtitle`, none is `off`. White with a shadow is the default color (`#FFFFFF`). A brand color usually goes in `captions.highlight`; put it in `captions.color` only if they want every word in it. Sound effects plus music is `sfx_music`; the recipes then take the first track in `my-sounds/music/`, so ask the user to put one there. Sound effects come from `my-sounds/sfx/`.

The style applies to new edits. To change a project that already exists, use `set` ops on it (`reference/editor-api.md`).

## The 5-second sample

If there is a video in `inbox/`, show the user their choices on their own footage before you finish:

1. `veecut edit --no-render` builds a project from the newest inbox video without rendering the whole draft.
2. `veecut render --range 0-5` renders the first 5 seconds of it to the outbox.
3. Open that file for the user and ask if the captions, position and colors look right. If they want changes, run `veecut style set` for each one and repeat steps 1 and 2. A new `veecut edit` picks up the new style; the earlier sample project keeps the old one.

Then tell them in one line that their style is saved and that they can change it any time by saying so ("captions sa baba na lang").
