//! The project file: one JSON document per edit, shared by the CLI, the MCP server, the desktop
//! editor and coding agents. Times are seconds (f64) on the project timeline unless a field says
//! otherwise; positions and sizes are fractions of the canvas (0..1) so one project renders at any
//! output size.

use std::borrow::Cow;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

pub const PROJECT_VERSION: u32 = 1;
pub const PROJECT_FILE: &str = "project.json";

/// The kind of video being edited. Each kind has a recipe (pipeline) and a workflow skill.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VideoKind {
    /// Reels, TikTok, Shorts: vertical talking head under 3 minutes.
    #[default]
    TalkingHeadShort,
    /// YouTube or Facebook feed talking head, horizontal or 4:5.
    TalkingHeadLong,
    /// Screen recording tutorial or software demo.
    Tutorial,
    /// Podcast or interview with two or more speakers.
    Podcast,
    /// Long video cut into vertical short clips.
    Clips,
    /// Vlog or B-roll montage cut to music.
    Vlog,
    /// Faceless explainer: narration with text and images.
    Explainer,
    /// Product promo or launch film.
    Promo,
    /// Music or lyric video.
    MusicVideo,
}

impl VideoKind {
    pub const ALL: [VideoKind; 9] = [
        VideoKind::TalkingHeadShort,
        VideoKind::TalkingHeadLong,
        VideoKind::Tutorial,
        VideoKind::Podcast,
        VideoKind::Clips,
        VideoKind::Vlog,
        VideoKind::Explainer,
        VideoKind::Promo,
        VideoKind::MusicVideo,
    ];

    /// Kebab-case name used on the command line and in workflow file names.
    pub fn slug(self) -> &'static str {
        match self {
            VideoKind::TalkingHeadShort => "talking-head-short",
            VideoKind::TalkingHeadLong => "talking-head-long",
            VideoKind::Tutorial => "tutorial",
            VideoKind::Podcast => "podcast",
            VideoKind::Clips => "clips",
            VideoKind::Vlog => "vlog",
            VideoKind::Explainer => "explainer",
            VideoKind::Promo => "promo",
            VideoKind::MusicVideo => "music-video",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            VideoKind::TalkingHeadShort => "Talking head, short (Reels, TikTok, Shorts)",
            VideoKind::TalkingHeadLong => "Talking head, long (YouTube, Facebook feed)",
            VideoKind::Tutorial => "Screen recording tutorial",
            VideoKind::Podcast => "Podcast or interview",
            VideoKind::Clips => "Long video to short clips",
            VideoKind::Vlog => "Vlog or B-roll montage",
            VideoKind::Explainer => "Faceless explainer",
            VideoKind::Promo => "Product promo",
            VideoKind::MusicVideo => "Music or lyric video",
        }
    }

    /// Accepts the kebab-case slug or the snake_case JSON name.
    pub fn from_slug(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase().replace('_', "-");
        Self::ALL.into_iter().find(|k| k.slug() == s)
    }
}

impl fmt::Display for VideoKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.slug())
    }
}

/// An sRGB color written as `#RRGGBB` or `#RRGGBBAA` in JSON.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const WHITE: Color = Color::rgb(255, 255, 255);
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const YELLOW: Color = Color::rgb(0xFF, 0xD4, 0x00);

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color { r, g, b, a: 255 }
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Color { r, g, b, a }
    }

    pub fn with_alpha(self, a: u8) -> Self {
        Color { a, ..self }
    }

    pub fn parse(s: &str) -> Result<Self> {
        let hex = s.trim().trim_start_matches('#');
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16);
        let parsed = match hex.len() {
            6 => (byte(0), byte(2), byte(4), Ok(255)),
            8 => (byte(0), byte(2), byte(4), byte(6)),
            _ => bail!("color {s:?} must look like #RRGGBB or #RRGGBBAA"),
        };
        match parsed {
            (Ok(r), Ok(g), Ok(b), Ok(a)) => Ok(Color { r, g, b, a }),
            _ => bail!("color {s:?} has a character that is not hexadecimal"),
        }
    }
}

impl TryFrom<String> for Color {
    type Error = anyhow::Error;
    fn try_from(s: String) -> Result<Self> {
        Color::parse(&s)
    }
}

impl From<Color> for String {
    fn from(c: Color) -> String {
        c.to_string()
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.a == 255 {
            write!(f, "#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
        } else {
            write!(
                f,
                "#{:02X}{:02X}{:02X}{:02X}",
                self.r, self.g, self.b, self.a
            )
        }
    }
}

impl JsonSchema for Color {
    fn schema_name() -> Cow<'static, str> {
        "Color".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?$",
            "description": "sRGB color, #RRGGBB or #RRGGBBAA"
        })
    }
}

/// A point in normalized coordinates: (0,0) is the top-left corner, (1,1) the bottom-right.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const CENTER: Point = Point { x: 0.5, y: 0.5 };
    pub const fn new(x: f32, y: f32) -> Self {
        Point { x, y }
    }
}

/// A rectangle in normalized canvas coordinates (top-left corner plus size).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const FULL: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };
}

/// Output frame size, frame rate and background color.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    #[serde(default = "default_background")]
    pub background: Color,
}

fn default_background() -> Color {
    Color::BLACK
}

impl Canvas {
    pub fn new(width: u32, height: u32) -> Self {
        Canvas {
            width,
            height,
            fps: 30.0,
            background: Color::BLACK,
        }
    }
    /// 9:16, 1080x1920: Reels, TikTok, Shorts.
    pub fn vertical() -> Self {
        Canvas::new(1080, 1920)
    }
    /// 16:9, 1920x1080: YouTube.
    pub fn landscape() -> Self {
        Canvas::new(1920, 1080)
    }
    /// 4:5, 1080x1350: Facebook and Instagram feed.
    pub fn portrait() -> Self {
        Canvas::new(1080, 1350)
    }
    /// 1:1, 1080x1080.
    pub fn square() -> Self {
        Canvas::new(1080, 1080)
    }

    /// Accepts `9:16`, `16:9`, `4:5`, `1:1`, or the words vertical, landscape/horizontal,
    /// portrait/feed and square.
    pub fn from_aspect(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "9:16" | "vertical" | "reel" | "reels" | "shorts" | "tiktok" => Some(Self::vertical()),
            "16:9" | "landscape" | "horizontal" | "youtube" => Some(Self::landscape()),
            "4:5" | "portrait" | "feed" | "facebook" => Some(Self::portrait()),
            "1:1" | "square" => Some(Self::square()),
            _ => None,
        }
    }

    pub fn aspect_name(&self) -> String {
        fn gcd(a: u32, b: u32) -> u32 {
            if b == 0 { a } else { gcd(b, a % b) }
        }
        let g = gcd(self.width, self.height).max(1);
        format!("{}:{}", self.width / g, self.height / g)
    }

    /// Duration of one frame in seconds.
    pub fn frame(&self) -> f64 {
        1.0 / self.fps
    }

    /// Snap a time to the nearest frame boundary.
    pub fn snap(&self, t: f64) -> f64 {
        (t * self.fps).round() / self.fps
    }
}

/// Color look applied to every video and image layer before graphics are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LookPreset {
    #[default]
    Natural,
    Warm,
    Cool,
    BlackWhite,
    TealOrange,
    Vivid,
    Matte,
}

/// A look preset plus fine adjustments. Adjustments run from -1 to 1 (vignette 0 to 1) and add
/// to whatever the preset does.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Look {
    #[serde(default)]
    pub preset: LookPreset,
    #[serde(default)]
    pub brightness: f32,
    #[serde(default)]
    pub contrast: f32,
    #[serde(default)]
    pub saturation: f32,
    #[serde(default)]
    pub temperature: f32,
    #[serde(default)]
    pub vignette: f32,
}

/// Final audio mix settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AudioMix {
    /// Integrated loudness target in LUFS. -14 suits Reels, TikTok, YouTube and Facebook.
    #[serde(default = "default_lufs")]
    pub target_lufs: f32,
    /// True-peak ceiling in dBTP.
    #[serde(default = "default_true_peak")]
    pub true_peak_db: f32,
    /// How many dB music marked `duck` drops while someone speaks.
    #[serde(default = "default_duck")]
    pub duck_db: f32,
    /// High-pass and gentle noise reduction on the main track's voice.
    #[serde(default = "yes")]
    pub voice_cleanup: bool,
}

fn default_lufs() -> f32 {
    -14.0
}
fn default_true_peak() -> f32 {
    -1.0
}
fn default_duck() -> f32 {
    12.0
}
fn yes() -> bool {
    true
}
fn one() -> f64 {
    1.0
}
fn one_f32() -> f32 {
    1.0
}

impl Default for AudioMix {
    fn default() -> Self {
        AudioMix {
            target_lufs: default_lufs(),
            true_peak_db: default_true_peak(),
            duck_db: default_duck(),
            voice_cleanup: true,
        }
    }
}

/// How captions on the captions track are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CaptionMode {
    /// One word at a time, popping in.
    WordPop,
    /// A short phrase on screen; the word being spoken is highlighted.
    #[default]
    Highlight,
    /// A short phrase on screen; emphasis words sit in colored pills.
    Pills,
    /// Full-sentence subtitles, no animation.
    Subtitle,
    /// No captions.
    Off,
}

/// Project-wide caption style. Sizes are fractions of the canvas height.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CaptionStyle {
    #[serde(default)]
    pub mode: CaptionMode,
    /// Font family name; see `veecut fonts` for the bundled ones.
    #[serde(default = "default_caption_font")]
    pub font: String,
    /// Text height as a fraction of the canvas height.
    #[serde(default = "default_caption_size")]
    pub size: f32,
    #[serde(default = "default_caption_color")]
    pub color: Color,
    /// Color of the spoken word (highlight mode) or of emphasis words and pills.
    #[serde(default = "default_caption_highlight")]
    pub highlight: Color,
    /// Outline width as a fraction of the font size. 0 turns the outline off.
    #[serde(default = "default_caption_stroke")]
    pub stroke: f32,
    #[serde(default = "yes")]
    pub shadow: bool,
    /// Every caption sits in one dark box with a thin light edge, so it reads on black, white
    /// and busy footage alike (Herk Edit Kit's house look).
    #[serde(default)]
    pub boxed: bool,
    #[serde(default = "yes")]
    pub uppercase: bool,
    /// Vertical center of the caption block as a fraction of the canvas height
    /// (0.25 upper third, 0.5 middle, 0.75 lower third).
    #[serde(default = "default_caption_position")]
    pub position: f32,
    /// Most words shown at once (WordPop shows one regardless).
    #[serde(default = "default_caption_words")]
    pub max_words: u8,
    /// Most characters per line before wrapping.
    #[serde(default = "default_caption_chars")]
    pub max_chars: u16,
}

fn default_caption_font() -> String {
    "Montserrat".into()
}
fn default_caption_size() -> f32 {
    0.045
}
fn default_caption_color() -> Color {
    Color::WHITE
}
fn default_caption_highlight() -> Color {
    Color::YELLOW
}
fn default_caption_stroke() -> f32 {
    0.12
}
fn default_caption_position() -> f32 {
    0.70
}
fn default_caption_words() -> u8 {
    3
}
fn default_caption_chars() -> u16 {
    20
}

impl Default for CaptionStyle {
    fn default() -> Self {
        CaptionStyle {
            mode: CaptionMode::default(),
            font: default_caption_font(),
            size: default_caption_size(),
            color: default_caption_color(),
            highlight: default_caption_highlight(),
            stroke: default_caption_stroke(),
            shadow: true,
            boxed: false,
            uppercase: true,
            position: default_caption_position(),
            max_words: default_caption_words(),
            max_chars: default_caption_chars(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Video,
    Audio,
    Image,
}

/// What an asset is for. Recipes and the editor use it to pick tracks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssetRole {
    /// The main recording.
    #[default]
    Source,
    /// Cutaway footage shown over the main track.
    Broll,
    Music,
    Sfx,
    /// A separate voice-over or microphone track.
    Voice,
    Image,
    Logo,
}

/// A media file the project uses. The file is never modified.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Asset {
    pub id: String,
    /// Relative to the project folder, or absolute.
    pub path: PathBuf,
    pub kind: AssetKind,
    #[serde(default)]
    pub role: AssetRole,
    /// Seconds; 0 for images.
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default)]
    pub fps: f64,
    #[serde(default)]
    pub has_audio: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    /// The magnetic main track: clips play back to back with no gaps. Exactly one, first.
    Main,
    /// Video and images over the main track (B-roll, picture in picture, screenshots).
    Overlay,
    /// Titles, hooks, cards and callouts.
    Text,
    /// Word-timed captions.
    Captions,
    /// Music, sound effects, voice-over.
    Audio,
}

impl TrackKind {
    pub fn is_visual(self) -> bool {
        !matches!(self, TrackKind::Audio)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Track {
    pub id: String,
    pub kind: TrackKind,
    pub name: String,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub items: Vec<Item>,
}

/// Anything placed on a track.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Item {
    pub id: String,
    /// Timeline start in seconds. On the main track this is computed; edit the order instead.
    pub start: f64,
    /// Timeline duration in seconds.
    pub duration: f64,
    #[serde(flatten)]
    pub body: ItemBody,
}

impl Item {
    pub fn end(&self) -> f64 {
        self.start + self.duration
    }
    pub fn contains(&self, t: f64) -> bool {
        t >= self.start && t < self.end()
    }
    pub fn clip(&self) -> Option<&Clip> {
        match &self.body {
            ItemBody::Clip(c) => Some(c),
            _ => None,
        }
    }
    pub fn clip_mut(&mut self) -> Option<&mut Clip> {
        match &mut self.body {
            ItemBody::Clip(c) => Some(c),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ItemBody {
    Clip(Clip),
    Text(Text),
    Caption(Caption),
}

/// How a picture fills its frame when the aspect ratios differ.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// Fill the frame and crop the overflow around `focus`.
    #[default]
    Cover,
    /// Show the whole picture, letterboxed.
    Contain,
}

/// A camera move inside a clip: punch-in when `ease` is 0, a smooth push otherwise.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Zoom {
    /// Seconds from the clip's timeline start.
    pub at: f64,
    pub duration: f64,
    /// 1.0 is no zoom; 1.12 is a typical punch-in.
    pub scale: f32,
    /// Point to zoom toward, in source picture coordinates. Defaults to the clip's focus.
    #[serde(default)]
    pub focus: Option<Point>,
    /// Seconds to ramp into the zoom (and out of it at the end). 0 = instant punch-in.
    #[serde(default)]
    pub ease: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    /// Blend from the previous clip; the clips overlap by the duration.
    Crossfade,
    /// Dip through black.
    Dip,
    /// A quick white flash.
    Flash,
}

/// Transition from the previous main-track clip into this one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Transition {
    pub kind: TransitionKind,
    pub duration: f64,
}

/// Slow pan and zoom over a still image (or a video).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct KenBurns {
    pub from_scale: f32,
    pub to_scale: f32,
    #[serde(default)]
    pub from_focus: Option<Point>,
    #[serde(default)]
    pub to_focus: Option<Point>,
}

/// A piece of an asset placed on the timeline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Clip {
    pub asset: String,
    /// Where in the source file the clip starts, in seconds.
    #[serde(default)]
    pub src_in: f64,
    /// Playback speed. The clip covers `duration * speed` seconds of source.
    #[serde(default = "one")]
    pub speed: f64,
    /// Linear gain: 1.0 unchanged, 0.5 about -6 dB.
    #[serde(default = "one_f32")]
    pub volume: f32,
    #[serde(default)]
    pub muted: bool,
    #[serde(default = "one_f32")]
    pub opacity: f32,
    #[serde(default)]
    pub fit: Fit,
    /// Subject center in the source picture; cover crops and zooms keep it in frame.
    #[serde(default)]
    pub focus: Option<Point>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zooms: Vec<Zoom>,
    /// Placement on the canvas for overlay tracks. None fills the canvas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<Rect>,
    /// Corner radius of `frame`, as a fraction of the canvas width.
    #[serde(default)]
    pub radius: f32,
    #[serde(default)]
    pub fade_in: f64,
    #[serde(default)]
    pub fade_out: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition: Option<Transition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ken_burns: Option<KenBurns>,
    /// Audio tracks: lower this clip while someone speaks (music beds).
    #[serde(default)]
    pub duck: bool,
}

impl Clip {
    pub fn new(asset: impl Into<String>, src_in: f64) -> Self {
        Clip {
            asset: asset.into(),
            src_in,
            speed: 1.0,
            volume: 1.0,
            muted: false,
            opacity: 1.0,
            fit: Fit::Cover,
            focus: None,
            zooms: Vec::new(),
            frame: None,
            radius: 0.0,
            fade_in: 0.0,
            fade_out: 0.0,
            transition: None,
            ken_burns: None,
            duck: false,
        }
    }

    /// Source seconds covered by a clip of this timeline duration.
    pub fn src_len(&self, duration: f64) -> f64 {
        duration * self.speed
    }

    pub fn src_out(&self, duration: f64) -> f64 {
        self.src_in + self.src_len(duration)
    }

    /// Crossfade overlap with the previous main-track clip, in seconds.
    pub fn overlap(&self) -> f64 {
        match &self.transition {
            Some(Transition {
                kind: TransitionKind::Crossfade,
                duration,
            }) => duration.max(0.0),
            _ => 0.0,
        }
    }
}

/// Built-in looks for text items. The graphics engine owns how each one looks and moves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextPreset {
    /// Big text in the first seconds that tells viewers why to keep watching.
    Hook,
    /// Large centered title.
    Title,
    /// A rounded card with a short line or two.
    #[default]
    Card,
    /// Name and role in the lower third.
    LowerThird,
    /// A label with an arrow or box pointing at something on screen.
    Callout,
    /// A big number or step counter ("1", "STEP 2/5", "$0").
    Counter,
    /// A small pill-shaped tag.
    Label,
    /// A pull quote.
    Quote,
    /// Bullet list; one line per `\n`, revealed one by one.
    List,
    /// One or two words slammed full screen.
    Kinetic,
    /// End card call to action.
    Cta,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Anim {
    None,
    #[default]
    Fade,
    Pop,
    SlideUp,
    SlideDown,
    SlideLeft,
    SlideRight,
    Typewriter,
    Wipe,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    Left,
    #[default]
    Center,
    Right,
}

/// Optional overrides on top of a text preset. Sizes are fractions of the canvas height.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TextStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Color>,
    /// Second color: highlighted words (`*like this*` in the text), arrows, pills.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<Color>,
    /// Box or card color behind the text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<Color>,
    /// Outline width as a fraction of the font size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uppercase: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<Align>,
}

/// On-screen text. Wrap words in `*asterisks*` to draw them in the accent color.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Text {
    pub text: String,
    #[serde(default)]
    pub preset: TextPreset,
    #[serde(default, skip_serializing_if = "is_default_style")]
    pub style: TextStyle,
    /// Center of the text block. None uses the preset's place, kept inside platform safe zones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<Point>,
    #[serde(default)]
    pub anim_in: Anim,
    #[serde(default)]
    pub anim_out: Anim,
}

fn is_default_style(s: &TextStyle) -> bool {
    *s == TextStyle::default()
}

/// One page of captions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Caption {
    pub words: Vec<Word>,
}

/// A spoken word. Times are seconds from the caption item's start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
    /// Drawn in the highlight color or a pill: numbers, names, key ideas.
    #[serde(default)]
    pub emphasis: bool,
}

/// A note pinned to a moment, for example an editing note from the user.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Marker {
    pub at: f64,
    pub note: String,
    #[serde(default)]
    pub done: bool,
}

/// One edit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Project {
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub kind: VideoKind,
    pub canvas: Canvas,
    #[serde(default)]
    pub look: Look,
    #[serde(default)]
    pub audio: AudioMix,
    #[serde(default)]
    pub captions: CaptionStyle,
    #[serde(default)]
    pub assets: Vec<Asset>,
    /// `tracks[0]` is the main track. Visual tracks draw in order, so later tracks sit on top.
    pub tracks: Vec<Track>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<Marker>,
}

impl Project {
    /// A project with the standard track layout: main, B-roll, text, captions, music, effects.
    pub fn new(name: impl Into<String>, kind: VideoKind, canvas: Canvas) -> Self {
        let track = |id: &str, kind: TrackKind, name: &str| Track {
            id: id.into(),
            kind,
            name: name.into(),
            muted: false,
            hidden: false,
            items: Vec::new(),
        };
        Project {
            version: PROJECT_VERSION,
            name: name.into(),
            kind,
            canvas,
            look: Look::default(),
            audio: AudioMix::default(),
            captions: CaptionStyle::default(),
            assets: Vec::new(),
            tracks: vec![
                track("main", TrackKind::Main, "Main"),
                track("broll", TrackKind::Overlay, "B-roll"),
                track("text", TrackKind::Text, "Text"),
                track("captions", TrackKind::Captions, "Captions"),
                track("music", TrackKind::Audio, "Music"),
                track("sfx", TrackKind::Audio, "Sound effects"),
            ],
            markers: Vec::new(),
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading project {}", path.display()))?;
        let mut project: Project = serde_json::from_str(&text)
            .with_context(|| format!("{} is not a valid VeeCut project", path.display()))?;
        if project.version > PROJECT_VERSION {
            bail!(
                "{} was saved by a newer VeeCut (project version {}); update VeeCut",
                path.display(),
                project.version
            );
        }
        project.version = PROJECT_VERSION;
        project.normalize();
        Ok(project)
    }

    /// Writes pretty JSON atomically (temp file plus rename) so a crash never leaves half a file.
    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(
            ".{}.tmp",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(PROJECT_FILE)
        ));
        std::fs::write(&tmp, json + "\n")?;
        std::fs::rename(&tmp, path).with_context(|| format!("saving {}", path.display()))?;
        Ok(())
    }

    /// End of the last item on any track.
    pub fn duration(&self) -> f64 {
        self.tracks
            .iter()
            .flat_map(|t| t.items.iter())
            .map(Item::end)
            .fold(0.0, f64::max)
    }

    pub fn main(&self) -> &Track {
        &self.tracks[0]
    }

    pub fn main_mut(&mut self) -> &mut Track {
        &mut self.tracks[0]
    }

    pub fn track(&self, id: &str) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn track_index(&self, id: &str) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    /// First track of a kind, by name when several exist (for example the "sfx" audio track).
    pub fn track_of_kind(&self, kind: TrackKind) -> Option<usize> {
        self.tracks.iter().position(|t| t.kind == kind)
    }

    /// (track index, item index) of an item id.
    pub fn find(&self, item_id: &str) -> Option<(usize, usize)> {
        self.tracks.iter().enumerate().find_map(|(ti, t)| {
            t.items
                .iter()
                .position(|i| i.id == item_id)
                .map(|ii| (ti, ii))
        })
    }

    pub fn item(&self, item_id: &str) -> Option<&Item> {
        self.find(item_id).map(|(t, i)| &self.tracks[t].items[i])
    }

    pub fn item_mut(&mut self, item_id: &str) -> Option<&mut Item> {
        self.find(item_id)
            .map(move |(t, i)| &mut self.tracks[t].items[i])
    }

    pub fn asset(&self, id: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.id == id)
    }

    /// Absolute path of an asset for a project stored in `project_dir`.
    pub fn asset_path(&self, project_dir: &Path, asset: &Asset) -> PathBuf {
        if asset.path.is_absolute() {
            asset.path.clone()
        } else {
            project_dir.join(&asset.path)
        }
    }

    /// A new id like `clip-7` that no asset, track or item uses yet.
    pub fn new_id(&self, prefix: &str) -> String {
        let taken = |id: &str| {
            self.assets.iter().any(|a| a.id == id)
                || self
                    .tracks
                    .iter()
                    .any(|t| t.id == id || t.items.iter().any(|i| i.id == id))
        };
        let mut n = 1 + self
            .tracks
            .iter()
            .flat_map(|t| t.items.iter().map(|i| i.id.as_str()).chain([t.id.as_str()]))
            .chain(self.assets.iter().map(|a| a.id.as_str()))
            .filter_map(|id| {
                id.strip_prefix(prefix)?
                    .strip_prefix('-')?
                    .parse::<u64>()
                    .ok()
            })
            .max()
            .unwrap_or(0);
        loop {
            let id = format!("{prefix}-{n}");
            if !taken(&id) {
                return id;
            }
            n += 1;
        }
    }

    /// Main-track clip covering timeline time `t`: (item index, item).
    pub fn main_at(&self, t: f64) -> Option<(usize, &Item)> {
        let items = &self.main().items;
        // With crossfades two clips overlap; the later one wins once it has started.
        items.iter().enumerate().rev().find(|(_, i)| i.contains(t))
    }

    /// Maps a time in a source file to the timeline through the main track, if that moment was
    /// kept. Used to place captions and markers recorded against the raw footage.
    pub fn source_to_timeline(&self, asset_id: &str, src_t: f64) -> Option<f64> {
        self.main().items.iter().find_map(|item| {
            let clip = item.clip()?;
            if clip.asset != asset_id {
                return None;
            }
            let src_out = clip.src_out(item.duration);
            (src_t >= clip.src_in && src_t < src_out)
                .then(|| item.start + (src_t - clip.src_in) / clip.speed)
        })
    }

    /// Re-establishes the invariants after an edit: main track back to back (crossfades
    /// overlap), other tracks sorted by start, no negative starts or durations, zooms sorted.
    pub fn normalize(&mut self) {
        let mut t = 0.0;
        let mut prev_duration = f64::INFINITY;
        for item in &mut self.tracks[0].items {
            let overlap = item
                .clip()
                .map(|c| {
                    c.overlap()
                        .min(prev_duration / 2.0)
                        .min(item.duration / 2.0)
                })
                .unwrap_or(0.0);
            item.start = (t - overlap).max(0.0);
            t = item.start + item.duration;
            prev_duration = item.duration;
        }
        for track in &mut self.tracks {
            for item in &mut track.items {
                item.start = item.start.max(0.0);
                item.duration = item.duration.max(0.0);
                if let ItemBody::Clip(c) = &mut item.body {
                    c.zooms.sort_by(|a, b| a.at.total_cmp(&b.at));
                }
            }
            if track.kind != TrackKind::Main {
                track.items.sort_by(|a, b| a.start.total_cmp(&b.start));
            }
        }
        self.markers.sort_by(|a, b| a.at.total_cmp(&b.at));
    }

    /// Checks everything an edit can break and explains the first problem in plain words.
    pub fn validate(&self) -> Result<()> {
        if self.version != PROJECT_VERSION {
            bail!("project version {} is not supported", self.version);
        }
        if self.canvas.width < 16
            || self.canvas.height < 16
            || self.canvas.width % 2 == 1
            || self.canvas.height % 2 == 1
        {
            bail!("canvas must be at least 16x16 with even width and height");
        }
        if !(1.0..=120.0).contains(&self.canvas.fps) {
            bail!("canvas fps {} must be between 1 and 120", self.canvas.fps);
        }
        if self.tracks.first().map(|t| t.kind) != Some(TrackKind::Main) {
            bail!("the first track must be the main track");
        }
        if self
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Main)
            .count()
            != 1
        {
            bail!("a project has exactly one main track");
        }
        let mut ids = std::collections::HashSet::new();
        for a in &self.assets {
            if !ids.insert(a.id.as_str()) {
                bail!("id {:?} is used twice", a.id);
            }
        }
        for track in &self.tracks {
            if !ids.insert(track.id.as_str()) {
                bail!("id {:?} is used twice", track.id);
            }
            for item in &track.items {
                if !ids.insert(item.id.as_str()) {
                    bail!("id {:?} is used twice", item.id);
                }
                if !(item.duration > 0.0) {
                    bail!("item {} must have a positive duration", item.id);
                }
                if !item.start.is_finite() {
                    bail!("item {} has an invalid start", item.id);
                }
                self.validate_body(track, item)?;
            }
        }
        Ok(())
    }

    fn validate_body(&self, track: &Track, item: &Item) -> Result<()> {
        let id = &item.id;
        match (&item.body, track.kind) {
            (ItemBody::Clip(c), TrackKind::Main | TrackKind::Overlay | TrackKind::Audio) => {
                let Some(asset) = self.asset(&c.asset) else {
                    bail!(
                        "clip {id} uses asset {:?}, which the project does not have",
                        c.asset
                    );
                };
                if track.kind == TrackKind::Audio && asset.kind == AssetKind::Image {
                    bail!("clip {id}: an image cannot go on an audio track");
                }
                if !(c.speed > 0.0 && c.speed <= 16.0) {
                    bail!(
                        "clip {id}: speed {} must be above 0 and at most 16",
                        c.speed
                    );
                }
                if c.src_in < 0.0 {
                    bail!("clip {id}: src_in cannot be negative");
                }
                if asset.kind != AssetKind::Image && asset.duration > 0.0 {
                    let over = c.src_out(item.duration) - asset.duration;
                    // One frame of slack: probed durations and frame rounding disagree slightly.
                    if over > 1.0 / self.canvas.fps + 1e-6 {
                        bail!(
                            "clip {id} runs {:.3}s past the end of {} ({:.3}s long)",
                            over,
                            asset.id,
                            asset.duration
                        );
                    }
                }
                for z in &c.zooms {
                    if !(z.scale >= 1.0 && z.scale <= 4.0) || z.duration <= 0.0 || z.at < 0.0 {
                        bail!(
                            "clip {id}: zoom at {}s needs scale 1..4 and a positive duration",
                            z.at
                        );
                    }
                }
                if let Some(f) = c.frame {
                    if f.w <= 0.0 || f.h <= 0.0 {
                        bail!("clip {id}: frame needs a positive width and height");
                    }
                }
                Ok(())
            }
            (ItemBody::Text(t), TrackKind::Text | TrackKind::Overlay) => {
                if t.text.trim().is_empty() {
                    bail!("text {id} is empty");
                }
                Ok(())
            }
            (ItemBody::Caption(c), TrackKind::Captions) => {
                if c.words.is_empty() {
                    bail!("caption {id} has no words");
                }
                Ok(())
            }
            (_, kind) => bail!("item {id} cannot go on a {kind:?} track"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(id: &str, duration: f64) -> Asset {
        Asset {
            id: id.into(),
            path: format!("{id}.mp4").into(),
            kind: AssetKind::Video,
            role: AssetRole::Source,
            duration,
            width: 1920,
            height: 1080,
            fps: 30.0,
            has_audio: true,
        }
    }

    fn clip_item(id: &str, src_in: f64, duration: f64) -> Item {
        Item {
            id: id.into(),
            start: 0.0,
            duration,
            body: ItemBody::Clip(Clip::new("a", src_in)),
        }
    }

    #[test]
    fn main_track_is_magnetic_and_crossfades_overlap() {
        let mut p = Project::new("t", VideoKind::TalkingHeadShort, Canvas::vertical());
        p.assets.push(asset("a", 60.0));
        p.main_mut().items = vec![clip_item("c1", 0.0, 4.0), clip_item("c2", 10.0, 2.0)];
        p.normalize();
        assert_eq!(p.main().items[1].start, 4.0);
        p.main_mut().items[1].clip_mut().unwrap().transition = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.5,
        });
        p.normalize();
        assert_eq!(p.main().items[1].start, 3.5);
        assert_eq!(p.duration(), 5.5);
        assert_eq!(p.source_to_timeline("a", 11.0), Some(4.5));
        assert_eq!(p.source_to_timeline("a", 5.0), None);
        p.validate().unwrap();
    }

    #[test]
    fn json_round_trip_and_new_ids() {
        let mut p = Project::new("t", VideoKind::Tutorial, Canvas::landscape());
        p.assets.push(asset("a", 60.0));
        p.main_mut().items.push(clip_item("clip-3", 0.0, 5.0));
        assert_eq!(p.new_id("clip"), "clip-4");
        let json = serde_json::to_string(&p).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
        assert!(Color::parse("#ffd400").is_ok() && Color::parse("ffd40").is_err());
        assert_eq!(
            VideoKind::from_slug("music_video"),
            Some(VideoKind::MusicVideo)
        );
    }

    #[test]
    fn validation_explains_problems() {
        let mut p = Project::new("t", VideoKind::TalkingHeadShort, Canvas::vertical());
        p.main_mut().items.push(clip_item("c1", 0.0, 5.0));
        let err = p.validate().unwrap_err().to_string();
        assert!(err.contains("does not have"), "{err}");
        p.assets.push(asset("a", 3.0));
        let err = p.validate().unwrap_err().to_string();
        assert!(err.contains("past the end"), "{err}");
    }
}
