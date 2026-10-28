//! The user's style (my-style.toml) and lasting rules learned from notes (my-rules.md).

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::project::{Canvas, CaptionMode, Color, LookPreset, Project};

/// The choices from style setup, saved as `my-style.toml`. Missing keys take their defaults.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Style {
    pub format: Format,
    /// Big hook text in the first 3 seconds.
    pub hook: bool,
    pub pace: Pace,
    pub graphics: GraphicsLevel,
    /// Color look: natural, warm, cool, black_white, teal_orange, vivid or matte.
    pub look: LookPreset,
    pub sound: Sound,
    /// Spoken language as a code like `en` or `tl`, or `auto` to detect it.
    pub language: String,
    pub captions: CaptionPrefs,
}

/// Output shape. `both` renders a vertical and a horizontal version.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    #[default]
    Vertical,
    Horizontal,
    Both,
}

/// How hard silences and pauses are cut.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pace {
    /// Cut every breath and pause.
    Tight,
    /// Keep short pauses.
    #[default]
    Natural,
}

/// How much motion graphics a recipe adds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphicsLevel {
    Minimal,
    /// Some motion cards.
    #[default]
    Some,
    Heavy,
}

/// Sound design level.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sound {
    VoiceOnly,
    /// Light sound effects.
    #[default]
    Sfx,
    /// Sound effects plus a quiet music bed.
    SfxMusic,
}

/// Caption choices. `position` is `upper`, `middle`, `lower` or the vertical center as a
/// fraction of the frame height; `size` is `small`, `medium`, `large` or a fraction of it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptionPrefs {
    /// word_pop, highlight, pills, subtitle or off.
    pub mode: CaptionMode,
    pub position: Placement,
    pub color: Color,
    /// Color of the spoken or key words.
    pub highlight: Color,
    pub size: Size,
    pub font: String,
    pub uppercase: bool,
}

impl Default for CaptionPrefs {
    fn default() -> Self {
        CaptionPrefs {
            mode: CaptionMode::Highlight,
            // Upper third, as in free247editor: clear of the app buttons on Reels and TikTok.
            position: Placement::Named(Spot::Upper),
            color: Color::WHITE,
            highlight: Color::YELLOW,
            size: Size::Named(SizeName::Medium),
            font: "Montserrat".into(),
            uppercase: true,
        }
    }
}

/// A named caption position or an exact fraction of the frame height.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Placement {
    Named(Spot),
    Exact(f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Spot {
    Upper,
    Middle,
    Lower,
}

impl Placement {
    /// Vertical center of the caption block as a fraction of the frame height.
    pub fn fraction(self) -> f32 {
        match self {
            Placement::Named(Spot::Upper) => 0.25,
            Placement::Named(Spot::Middle) => 0.5,
            Placement::Named(Spot::Lower) => 0.75,
            Placement::Exact(f) => f.clamp(0.05, 0.95),
        }
    }
}

/// A named caption size or an exact fraction of the frame height.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Size {
    Named(SizeName),
    Exact(f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SizeName {
    Small,
    Medium,
    Large,
}

impl Size {
    /// Text height as a fraction of the frame height.
    pub fn fraction(self) -> f32 {
        match self {
            Size::Named(SizeName::Small) => 0.035,
            Size::Named(SizeName::Medium) => 0.045,
            Size::Named(SizeName::Large) => 0.06,
            Size::Exact(f) => f.clamp(0.01, 0.2),
        }
    }
}

impl Default for Style {
    fn default() -> Self {
        Style {
            format: Format::default(),
            hook: true,
            pace: Pace::default(),
            graphics: GraphicsLevel::default(),
            look: LookPreset::default(),
            sound: Sound::default(),
            language: "auto".into(),
            captions: CaptionPrefs::default(),
        }
    }
}

const STYLE_HEADER: &str = "# Your VeeCut style. Every option is explained in\n# .agents/skills/veecut/reference/style-setup.md\n\n";

impl Style {
    /// Reads `my-style.toml`; defaults when the file does not exist.
    pub fn load(path: &Path) -> Result<Style> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).with_context(|| {
                format!("{} is not valid; fix the line named above", path.display())
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Style::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = STYLE_HEADER.to_string() + &toml::to_string_pretty(self)?;
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }

    /// Output canvases for the format: one, or vertical then horizontal for `both`.
    pub fn canvases(&self) -> Vec<Canvas> {
        match self.format {
            Format::Vertical => vec![Canvas::vertical()],
            Format::Horizontal => vec![Canvas::landscape()],
            Format::Both => vec![Canvas::vertical(), Canvas::landscape()],
        }
    }

    /// Sets the project's caption style, look and canvas size (the first of [`Style::canvases`]).
    /// Frame rate and background stay as they are.
    pub fn apply_to(&self, project: &mut Project) {
        let c = &self.captions;
        let style = &mut project.captions;
        style.mode = c.mode;
        style.position = c.position.fraction();
        style.color = c.color;
        style.highlight = c.highlight;
        style.size = c.size.fraction();
        style.font = c.font.clone();
        style.uppercase = c.uppercase;
        project.look.preset = self.look;
        let canvas = &self.canvases()[0];
        project.canvas.width = canvas.width;
        project.canvas.height = canvas.height;
    }

    pub fn pace(&self) -> Pace {
        self.pace
    }

    pub fn graphics(&self) -> GraphicsLevel {
        self.graphics
    }

    pub fn sound(&self) -> Sound {
        self.sound
    }

    pub fn hook(&self) -> bool {
        self.hook
    }
}

pub(crate) const RULES_HEADER: &str = "# My rules\n\nLasting rules VeeCut learned from your notes. They beat my-style.toml on every edit.\nEdit or delete any line.\n\n";

/// Appends `- <rule> ("<user words>", YYYY-MM-DD)` to `my-rules.md`, creating it when missing.
pub fn add_rule(path: &Path, rule: &str, user_words: &str) -> Result<()> {
    let one_line = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let rule = one_line(rule);
    if rule.is_empty() {
        bail!("the rule is empty; describe what to do on every edit");
    }
    let words = one_line(user_words).replace('"', "'");
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let mut text = String::new();
    if existing.is_empty() {
        text.push_str(RULES_HEADER);
    } else if !existing.ends_with('\n') {
        text.push('\n');
    }
    let date = chrono::Local::now().format("%Y-%m-%d");
    text.push_str(&format!("- {rule} (\"{words}\", {date})\n"));
    file.write_all(text.as_bytes())
        .with_context(|| format!("writing {}", path.display()))
}

/// The bullet lines of `my-rules.md` without their `- ` marker. Empty when the file is missing.
pub fn rules(path: &Path) -> Result<Vec<String>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    Ok(text
        .lines()
        .filter_map(|l| {
            l.trim_start()
                .strip_prefix("- ")
                .or_else(|| l.trim_start().strip_prefix("* "))
        })
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_reads_named_and_exact_choices() {
        let style: Style = toml::from_str(
            "format = \"both\"\npace = \"tight\"\nsound = \"voice_only\"\n[captions]\nposition = 0.4\nsize = \"large\"\ncolor = \"#FF0000\"\n",
        )
        .unwrap();
        assert_eq!(style.format, Format::Both);
        assert_eq!(style.pace(), Pace::Tight);
        assert_eq!(style.sound(), Sound::VoiceOnly);
        assert_eq!(style.graphics(), GraphicsLevel::Some);
        assert_eq!(style.captions.position.fraction(), 0.4);
        assert_eq!(style.captions.size.fraction(), 0.06);
        let mut project = Project::new("t", Default::default(), Canvas::landscape());
        style.apply_to(&mut project);
        assert_eq!((project.canvas.width, project.canvas.height), (1080, 1920));
        assert_eq!(project.captions.color, Color::rgb(255, 0, 0));
        let round: Style = toml::from_str(&toml::to_string_pretty(&style).unwrap()).unwrap();
        assert_eq!(round, style);
    }
}
