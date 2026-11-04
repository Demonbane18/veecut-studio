//! `veecut find`, `veecut fetch` and `veecut sources`: free, licence-tracked footage.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use veecut_engine::footage::{self, Candidate, MediaKind, Provider, SearchOptions};
use veecut_engine::ops::{self, Op};
use veecut_engine::project::{AssetRole, Source};
use veecut_engine::workspace::Workspace;

use crate::commands::{APPROVAL_IS_HUMAN, Ctx, Out, ProjectArg, rel};

#[derive(Args, Deserialize, JsonSchema, Default)]
pub struct FindArgs {
    /// What to look for: the real thing a line is about, or a picture idea like "dripping tap".
    pub idea: String,
    /// Pictures only.
    #[arg(long, conflicts_with = "videos")]
    #[serde(default)]
    pub images: bool,
    /// Videos only.
    #[arg(long)]
    #[serde(default)]
    pub videos: bool,
    /// Providers, comma separated: nasa, commons, archive, openverse, pexels. Default: every
    /// keyless one, plus pexels when PEXELS_API_KEY is set.
    #[arg(long, value_name = "LIST")]
    #[serde(default)]
    pub from: Option<String>,
    /// Results per provider, 1 to 20. Default: 4.
    #[arg(long)]
    #[serde(default)]
    pub count: Option<usize>,
}

#[derive(Args, Deserialize, JsonSchema, Default)]
pub struct FetchArgs {
    /// A number from the last `veecut find`, or a candidate id such as nasa:PIA01637.
    pub item: String,
    /// What it is for: broll, image or music. Default: broll for video, image for pictures.
    #[arg(long)]
    #[serde(default)]
    pub role: Option<String>,
    #[command(flatten)]
    #[serde(flatten)]
    pub project: ProjectArg,
}

#[derive(Args, Deserialize, JsonSchema, Default)]
#[serde(default)]
pub struct SourcesArgs {
    /// The user checked this flagged asset's rights and allows it in the final render. Needs an
    /// interactive terminal; not offered over MCP: approving is the user's decision.
    #[arg(long, value_name = "ASSET")]
    #[serde(skip)]
    pub approve: Option<String>,
    /// Flag an asset whose rights need a check; needs --reason.
    #[arg(long, value_name = "ASSET", requires = "reason")]
    pub flag: Option<String>,
    /// Why the flagged asset needs a check.
    #[arg(long)]
    pub reason: Option<String>,
    /// Credit lines for the found files the edit uses, ready for the video description.
    #[arg(long)]
    pub credits: bool,
    #[command(flatten)]
    #[serde(flatten)]
    pub project: ProjectArg,
}

/// The last search, so `veecut fetch 3` works by number.
#[derive(Serialize, Deserialize)]
struct LastFind {
    query: String,
    candidates: Vec<Candidate>,
    warnings: Vec<String>,
}

fn find_dir(ws: &Workspace) -> PathBuf {
    ws.root.join(".veecut").join("find")
}

impl Ctx {
    /// Searches the providers, writes `.veecut/find/last.json` and the contact sheet
    /// `.veecut/find/last.png`, and lists the numbered candidates.
    pub fn find(&self, args: FindArgs) -> Result<Out> {
        let mut opts = SearchOptions {
            kind: match (args.images, args.videos) {
                (true, false) => Some(MediaKind::Image),
                (false, true) => Some(MediaKind::Video),
                _ => None,
            },
            ..SearchOptions::default()
        };
        if let Some(list) = &args.from {
            opts.providers = list
                .split(',')
                .filter(|p| !p.trim().is_empty())
                .map(Provider::parse)
                .collect::<Result<_>>()?;
        }
        if let Some(n) = args.count {
            opts.per_provider = n.clamp(1, 20);
        }
        let result = footage::search(&args.idea, &opts)?;
        let dir = find_dir(&self.ws);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let sheet = dir.join("last.png");
        footage::contact_sheet(&result.candidates, &sheet)?;
        let last = LastFind {
            query: args.idea.trim().to_string(),
            candidates: result.candidates.clone(),
            warnings: result.warnings.clone(),
        };
        let file = dir.join("last.json");
        std::fs::write(&file, serde_json::to_string_pretty(&last)? + "\n")
            .with_context(|| format!("writing {}", file.display()))?;

        let mut counts = BTreeMap::new();
        for p in &opts.providers {
            counts.insert(p.as_str(), 0usize);
        }
        for c in &result.candidates {
            *counts.entry(c.provider.as_str()).or_default() += 1;
        }
        let mut text = format!(
            "Found {} for {:?}: {}.\n",
            result.candidates.len(),
            last.query,
            counts
                .iter()
                .map(|(p, n)| format!("{p} {n}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        for (i, c) in result.candidates.iter().enumerate() {
            let _ = writeln!(
                text,
                "{}. [{}] {} · {} · {:?}{} ({})",
                i + 1,
                describe(c),
                c.provider.as_str(),
                c.license,
                c.title,
                c.author
                    .as_deref()
                    .map(|a| format!(" by {a}"))
                    .unwrap_or_default(),
                c.id
            );
            if c.flag {
                let _ = writeln!(
                    text,
                    "   FLAGGED: {}",
                    c.flag_reason.as_deref().unwrap_or("check the rights")
                );
            }
        }
        for w in &result.warnings {
            let _ = writeln!(text, "Warning: {w}");
        }
        let _ = write!(
            text,
            "Contact sheet: {}\nNext: look at the sheet, then `veecut fetch <number>`. Flagged files need the user's rights check before a final render.",
            rel(&self.ws, &sheet)
        );
        let numbered: Vec<_> = result
            .candidates
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mut v = serde_json::to_value(c).unwrap_or_default();
                v["index"] = json!(i + 1);
                v
            })
            .collect();
        let mut out = Out::new(
            text,
            json!({
                "query": last.query,
                "candidates": numbered,
                "counts": counts,
                "warnings": result.warnings,
                "sheet": rel(&self.ws, &sheet),
            }),
        );
        out.png = Some(std::fs::read(&sheet)?);
        Ok(out)
    }

    /// Downloads a candidate into footage/ and adds it to the project with its source.
    pub fn fetch(&self, args: FetchArgs, progress: &mut dyn FnMut(&str)) -> Result<Out> {
        let item = args.item.trim();
        let id = match item.parse::<usize>() {
            Ok(n) => {
                let file = find_dir(&self.ws).join("last.json");
                let last: LastFind = serde_json::from_slice(&std::fs::read(&file).with_context(
                    || "no earlier search to pick from; run `veecut find \"IDEA\"` first",
                )?)
                .context("the last search list is unreadable; run `veecut find` again")?;
                n.checked_sub(1)
                    .and_then(|i| last.candidates.get(i))
                    .with_context(|| {
                        format!(
                            "the last find ({:?}) has {} results, so there is no number {n}",
                            last.query,
                            last.candidates.len()
                        )
                    })?
                    .id
                    .clone()
            }
            Err(_) => item.to_string(),
        };
        let (dir, mut p) = self.project(&args.project)?;
        progress(&format!("Fetching {id} into footage/"));
        let fetched = footage::fetch(&self.ws, &id)?;
        let role = match &args.role {
            Some(r) => serde_json::from_value::<AssetRole>(json!(r.trim().to_lowercase()))
                .map_err(|_| anyhow!("unknown role {r:?}; use broll, image or music"))?,
            None if fetched.candidate.kind == MediaKind::Image => AssetRole::Image,
            None => AssetRole::Broll,
        };
        let asset = footage::add_to_project(&self.ws, &mut p, &dir, &fetched, role)?;
        self.ws.save_project(&dir, &p)?;
        let c = &fetched.candidate;
        let path = rel(&self.ws, &fetched.path);
        let mut text = format!(
            "{asset}: {path} ({}, {role:?})\nLicence: {}{}\nCredit: {}\n",
            describe(c),
            fetched.source.license,
            fetched
                .source
                .license_url
                .as_deref()
                .map(|u| format!(" ({u})"))
                .unwrap_or_default(),
            fetched.source.credit.as_deref().unwrap_or("")
        );
        let approved = p.sources.get(&asset).is_some_and(|s| s.approved);
        if fetched.source.flag && !approved {
            let _ = writeln!(
                text,
                "FLAGGED: {}. A final render refuses it until the user checks the rights and runs `veecut sources --approve {asset}`.",
                fetched
                    .source
                    .flag_reason
                    .as_deref()
                    .unwrap_or("check the rights")
            );
        }
        let _ = write!(
            text,
            "Place it with an op, for example {{\"op\":\"add_clip\",\"asset\":\"{asset}\",\"start\":5.0,\"duration\":1.2}}"
        );
        Ok(Out::new(
            text,
            json!({ "asset": asset, "path": path, "role": role, "candidate": c, "source": p.sources.get(&asset) }),
        ))
    }

    /// The rights table; approves (a person at a terminal only) or flags through ops so undo
    /// works; credits. Agents run without a terminal, so they cannot approve.
    pub fn sources(&self, args: SourcesArgs) -> Result<Out> {
        if args.approve.is_some() && !std::io::stdin().is_terminal() {
            bail!(APPROVAL_IS_HUMAN);
        }
        let (dir, mut p) = self.project(&args.project)?;
        let mut changes = Vec::new();
        if let Some(asset) = &args.approve {
            let s = p.sources.get(asset).with_context(|| {
                format!("{asset} has no source record; `veecut sources` lists the found files")
            })?;
            let line = format!(
                "Approved {asset} ({}): the user checked its rights.",
                s.license
            );
            ops::apply(
                &mut p,
                &Op::Set {
                    path: format!("/sources/{asset}/approved"),
                    value: json!(true),
                },
            )?;
            changes.push(line);
        }
        if let Some(asset) = &args.flag {
            let reason = args
                .reason
                .as_deref()
                .map(str::trim)
                .filter(|r| !r.is_empty())
                .context("--flag needs --reason TEXT saying what must be checked")?;
            let a = p.asset(asset).with_context(|| {
                format!("no asset {asset:?} in this project; `veecut status` lists the assets")
            })?;
            let mut s = p.sources.get(asset).cloned().unwrap_or_else(|| Source {
                title: a
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                license: "unknown".into(),
                provider: "user".into(),
                ..Source::default()
            });
            s.flag = true;
            s.flag_reason = Some(reason.to_string());
            s.approved = false;
            let op = footage::source_op(&p, asset, &s)?;
            ops::apply(&mut p, &op)?;
            changes.push(format!("Flagged {asset}: {reason}"));
        }
        if !changes.is_empty() {
            self.ws.save_project(&dir, &p)?;
        }
        let used = footage::used_assets(&p);
        let blocking = footage::blocking_sources(&p);
        let mut text = String::new();
        for c in &changes {
            let _ = writeln!(text, "{c}");
        }
        let credits: Vec<String> = p
            .sources
            .iter()
            .filter(|(id, _)| used.contains(id.as_str()))
            .map(|(_, s)| {
                s.credit.clone().unwrap_or_else(|| {
                    let by = s
                        .author
                        .as_deref()
                        .map(|a| format!(" by {a}"))
                        .unwrap_or_default();
                    format!("\"{}\"{by}, {}: {}", s.title, s.license, s.url)
                })
            })
            .collect();
        if args.credits {
            if credits.is_empty() {
                let _ = writeln!(
                    text,
                    "The edit uses no found files, so there is nothing to credit."
                );
            } else {
                let _ = writeln!(text, "Credits:");
                for c in &credits {
                    let _ = writeln!(text, "{c}");
                }
            }
        } else if p.sources.is_empty() {
            let _ = writeln!(
                text,
                "No found files in this project yet; `veecut find \"IDEA\"` then `veecut fetch <number>`."
            );
        } else {
            for (id, s) in &p.sources {
                let status = if s.blocks_final() {
                    format!(
                        "FLAGGED: {}",
                        s.flag_reason.as_deref().unwrap_or("check the rights")
                    )
                } else if s.flag {
                    "approved by the user".to_string()
                } else {
                    "ok".to_string()
                };
                let _ = writeln!(
                    text,
                    "{id}  {}  {}  {:?}{}  {}  {status}\n    {}",
                    if s.provider.is_empty() {
                        "?"
                    } else {
                        &s.provider
                    },
                    s.license,
                    s.title,
                    s.author
                        .as_deref()
                        .map(|a| format!(" by {a}"))
                        .unwrap_or_default(),
                    if used.contains(id.as_str()) {
                        "in the edit"
                    } else {
                        "not used"
                    },
                    if s.url.is_empty() {
                        "(no source page)"
                    } else {
                        &s.url
                    }
                );
            }
        }
        if !blocking.is_empty() {
            let _ = write!(
                text,
                "A final render is blocked by {} file(s) until the user checks the rights and runs `veecut sources --approve <asset>`. Agents must not approve on their own.",
                blocking.len()
            );
        }
        let rows: Vec<_> = p
            .sources
            .iter()
            .map(|(id, s)| {
                let mut v = serde_json::to_value(s).unwrap_or_default();
                v["asset"] = json!(id);
                v["used"] = json!(used.contains(id.as_str()));
                v["blocks_final"] = json!(s.blocks_final());
                v
            })
            .collect();
        Ok(Out::new(
            text,
            json!({
                "sources": rows,
                "changes": changes,
                "credits": credits,
                "blocking": blocking.iter().map(|(id, _)| id).collect::<Vec<_>>(),
            }),
        ))
    }
}

/// "video 34 s 1920x1080" or "picture 5978x3977".
fn describe(c: &Candidate) -> String {
    let mut s = match c.kind {
        MediaKind::Video => "video".to_string(),
        MediaKind::Image => "picture".to_string(),
    };
    if c.duration > 0.0 {
        let _ = write!(s, " {:.1} s", c.duration);
    }
    if c.width > 0 && c.height > 0 {
        let _ = write!(s, " {}x{}", c.width, c.height);
    }
    s
}
