# Finding footage and keeping the rights straight

VeeCut searches free sources for pictures and footage, downloads the ones you pick into `footage/` and records where each came from and what its licence allows. The source list, the search habits and the traps below are ideas from NextPhase Editor V1's published notes (see CREDITS.md); the code is VeeCut's own.

Searching and downloading use the network. Tell the user before you do it and say which sources you searched.

## Providers

| Provider | What it has | Licence |
| --- | --- | --- |
| `nasa` | NASA Image and Video Library: space, launches, Earth, aircraft. Pictures and videos. | Public domain under NASA's media usage guidelines, unless a credit names someone else |
| `commons` | Wikimedia Commons: photos, maps, historic pictures, some video. Pictures download as a 2560-pixel version, videos as a 720p to 1080p copy when one exists. | Each file has its own |
| `archive` | Internet Archive: old films, newsreels, the Prelinger collection. Pictures and videos; the search only asks for items that carry a licence or sit in the Prelinger collection. | Each item has its own; Prelinger items are public domain |
| `openverse` | Openverse: openly licensed pictures gathered from Flickr, museums and other sites. Pictures only. Anonymous use is limited to 20 results a page and a small hourly quota. | Each picture has its own Creative Commons licence or public domain mark |
| `pexels` | Pexels: modern stock photos and videos. Only when `PEXELS_API_KEY` is in `.env`. | Pexels License |

Without `--from`, `veecut find` searches every keyless provider, plus Pexels when the key is set. The key is free at https://www.pexels.com/api/. The user adds it to `.env` themselves; never ask them to paste it into the chat.

## Commands

```
veecut find "rocket launch" --json                      every available provider, 4 results each
veecut find "dripping tap" --images --from openverse,commons --count 8 --json
veecut fetch 3 --json                                   number 3 from the last find
veecut fetch nasa:PIA01637 --json                       or a candidate id
veecut sources --json                                   the rights table
veecut sources --credits                                credit lines for the video description
veecut sources --flag asset-4 --reason "looks like a news photo"
```

- `veecut find "IDEA"` numbers the results and saves a contact sheet to `.veecut/find/last.png`: each thumbnail with its number (red when flagged), provider, kind, size, licence and flag reason. `--images` or `--videos` limits the kind; `--from` takes `nasa`, `commons`, `archive`, `openverse` and `pexels`, comma separated; `--count` is results per provider, 1 to 20, default 4. One failing provider becomes a warning; the others still answer.
- `veecut fetch N` downloads number N from the last find into `footage/<provider>-<id>.<ext>`, checks with ffprobe that it really is a picture or a video, writes `<file>.source.json` next to it and adds it to the project as an asset with its source record. Pictures become `image` assets and videos `broll`; `--role` overrides that with `broll`, `image` or `music`. It prints the asset id, the licence and the credit line. A file fetched before is reused.
- Downloads are capped at 300 MB for a video and 40 MB for a picture. VeeCut takes jpg, png and webp pictures and mp4, webm and ogv videos.
- `footage/` and `.veecut/find/` stay out of git. Files in `footage/` are never edited after download.
- MCP: `find_footage` (`idea`, `images`, `videos`, `from`, `count`) returns the contact sheet as an image; `fetch_footage` (`item`, `role`); `sources` (`flag`, `reason`, `credits`) has no approve option.

Then place the file. To swap a picture the recipe placed, patch that B-roll clip's `asset` and `src_in`; to add one, use `add_clip`:

```json
{"op":"patch","item":"clip-12","patch":{"asset":"asset-15","src_in":0}}
{"op":"add_clip","asset":"asset-15","track":"broll","start":5.0,"duration":1.2}
```

## Searching well

- One idea per query, and name the real thing first: "Apollo 11 launch", not "space history and rockets".
- For each line ask what real thing the speaker means and where its own picture lives. Names, companies and claims always get the real thing.
- Where the line allows, a picture metaphor works well: "leaked" can be a dripping tap, "nothing" a tumbleweed.
- Try another provider or fewer words when results are thin. NASA is strongest for space and science, Archive for old film, Commons for places, people and history, Openverse and Pexels for everyday objects.
- Prefer moving footage: in Editor V1's references 50 to 75 percent of the runtime moves. Stills get a push in the edit.
- Pick by looking. Open the contact sheet and choose by number; search words often match the wrong thing. After placing a video, check the moment with `veecut frame --at T` and move `src_in` if a better part of the clip fits the line.

## Rights

Every candidate carries its licence. VeeCut allows these without a check: public domain, the public domain mark, CC0, CC BY, CC BY-SA and the Pexels License. These are flagged, so a person must check them before a final render:

| Flagged | Why |
| --- | --- |
| Non-commercial (NC) | No commercial use |
| No derivatives (ND) | No changes allowed, and VeeCut always re-colours, crops and cuts |
| GFDL only | It needs the whole licence text shipped with the video |
| Fair use or non-free | Fair use is decided case by case, never by a licence |
| Unknown or missing | Nobody knows who owns it |
| NASA files whose credit names someone else, or whose description mentions copyright | NASA's own media is public domain; other people's is not |
| Archive items with no licence outside the Prelinger collection | Nobody knows who owns it |

- `veecut edit --kind documentary --find` never picks a flagged file. `veecut fetch` downloads one when asked and prints FLAGGED with the reason.
- A final render (`veecut render --final`, or Export at final quality in Studio) refuses while the edit uses a flagged file nobody approved. Drafts still render.
- You never decide that a flagged file is fine. Show the user the file, its page and the reason. Only the user approves: they run `veecut sources --approve ASSET` after checking the rights, press Approve in Studio's Sources tab, or tell you they checked that file and want it approved. Otherwise replace the file.
- Flag anything that looks wrong even when its licence looks fine, such as a company's film, a news photo or someone's own clip: `veecut sources --flag ASSET --reason TEXT`. You may flag; only the user approves.
- Credit lines are ready for the video description: `veecut sources --credits`. CC BY and CC BY-SA lines say the work was changed ("re-coloured, cropped and cut"); Pexels lines read "Photo by NAME on Pexels".
- Each downloaded file keeps its own licence, recorded in the project and in its `.source.json`. Never upload or publish anything.

## Traps

- Search matches words, not meaning. Look at every picture before you keep it.
- Check pages, consent walls and bot checks can look like real files. VeeCut refuses HTML and files of the wrong kind, but a blank or wrong picture can still get through, so look at the frame.
- NASA links redirect and some ids hold spaces; VeeCut follows the redirects and checks the file type.
- Openverse runs out of anonymous searches after a while; try again later or use another provider.
- Commons diagrams and SVG files are skipped; only jpg, png and webp pictures download.
- A fetched file the project already uses keeps its asset id and any approval the user gave.

## Faceless and animated source packs

For narration-led shorts, use `workflows/explainer.md` with original narration and owner-created
or licensed visuals/music. Keep each asset's source page and required credit. A public download
or a code licence does not establish rights to reference art, voices or generated outputs.

### Source disclosure

Reviewed 2026-11-02; static inspection only, not a safety certification or runtime benchmark:

- [claude-animated-shorts](https://github.com/mromidev/claude-animated-shorts/tree/e3b817b6afc6cd413466d43f11ba31209e034bbf),
  commit `e3b817b6afc6cd413466d43f11ba31209e034bbf` (2026-11-01): MIT code/docs,
  but reference frames and contact sheets are private style-study material. Do not redistribute,
  publish or upload them to a model. Templates lack the surrounding app, package manifests,
  narration/timings and referenced GLBs. Remotion's separate licence has limited free eligibility.
- [Faceless Videos Skill v2 ZIP](https://drive.google.com/file/d/17_qG49TkOfKQK4_yhYOfaNG2dyb-sdKl/view),
  SHA-256 `cb411ea9152e429d2a838df7f2ab9780a08374bc8333b9911e17f1836e7c82e8`:
  `faceless-videos/LICENSES.md` gives informal use/remix/share permission for original work,
  not a named whole-pack SPDX licence. Example stills/maps/style sheets permit learning and
  own-video use; supplied music permits own-video use. Unrestricted asset redistribution and
  a complete rights chain are not established. No publication date or Git revision supplied.

VeeCut adopted generic narration-first beat planning and restrained camera-motion ideas,
without copying source text/code/media or bundling either pack. Its native pan/zoom does not
reproduce generated characters, 3D sets, depth/parallax or the Drive pack's five style engines.
Use separately rendered, rights-cleared clips as normal media if those effects are needed.

Do not install unknown root skills or run their scripts based on this review. Drive setup
executes a remote installer and downloads unpinned dependencies/models, its self-test deletes
its working folder, and browser capture workers can survive failures. Neither pack is required
for VeeCut. Local rendering does not mean network-free setup or free Claude/image-tool access;
optional ACE-Step has a roughly 15 GB installation and a stated 16 GB Apple Silicon requirement.
Use own recordings and configured local transcription when uploads are not approved.
