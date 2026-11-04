//! VeeCut Studio's engine. Everything except the desktop UI and the command line lives here, so
//! the CLI, the MCP server, the desktop editor and tests all drive the same code.

pub mod agent;
pub mod analyze;
pub mod captions;
pub mod config;
pub mod footage;
pub mod graphics;
pub mod look;
pub mod matte;
pub mod media;
pub mod ops;
pub mod project;
pub mod qa;
pub mod recipes;
pub mod render;
pub mod sfx;
pub mod skills;
pub mod style;
pub mod transcribe;
pub mod transcript;
pub mod workspace;

pub use ops::{Applied, Op, apply, apply_all};
pub use project::Project;
pub use transcript::Transcript;
