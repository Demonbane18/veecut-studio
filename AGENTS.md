# VeeCut Studio

Open-source AI video editor in Rust: coding agents and a desktop editor share one timeline, with workflows for each video type and a skills catalog synced from awesome-claude-video-skills.

## Working on the code

- Read `docs/architecture.md` first: it is the contract for modules, the CLI, MCP tools and the workspace layout. Change it together with the code.
- Crates: `crates/veecut-engine` (everything but UI and CLI), `crates/veecut-studio` (egui editor), `crates/veecut` (CLI and MCP server). Build with `cargo build --workspace`, test with `cargo test --workspace`, format with `cargo fmt --all`. Run `. "$HOME/.cargo/env"` first in a fresh shell.
- Rust draws every picture; ffmpeg only decodes, encodes and filters audio. Never depend on libass, freetype or `drawtext`.
- Every project change goes through `ops::apply`/`apply_all` (atomic and validated). Main-track edits ripple through every track.
- The agent skill lives in `crates/veecut-engine/skill/` and is embedded with `include_str!`; rebuild after editing it.
- Credit borrowed rules and ideas in `CREDITS.md` (free247editor, Herk Edit Kit, OpenCut ideas only, catalog skills with their licenses). Never use OpenCut's name or logo, or AI Automation Society brand assets.
- Test media is synthetic and stays out of git.

## Maintainer notes (only when `developer-vault/` exists)

Contributors without these folders and tools skip this section.

- `developer-vault/` holds the maintainer's private notes and Graphify map. It is an Obsidian vault with its own private repository, and this repository ignores it. In a linked worktree, use the main checkout's `developer-vault/`.
- At the start of a session, read `developer-vault/Home.md` and the newest entries in `developer-vault/Session Log.md`, then only the linked notes the task needs.
- Before the final summary, add one dated entry to `developer-vault/Session Log.md` with decisions, verified results, blockers and the next step, and update `developer-vault/Home.md` if the current state or next step changed. Then run `bun ~/.omp/agent/bin/project.ts sync` and report what it printed. It refreshes the code graph, exports it into the vault, commits and pushes the vault, and pushes this repository's committed work.

## Secrets

- Never write passwords, API keys, tokens, private keys or customer data into this repository or the vault.

## Version control

- Commit finished work in focused commits and push it. Never force-push or rewrite pushed history.

## graphify

This project has a knowledge graph at graphify-out/ with god nodes, community structure, and cross-file relationships.

When the user types `/graphify`, use the installed graphify skill or instructions before doing anything else.

Rules:
- When graphify-out/graph.json exists, run `graphify query "<question>"` before your first grep, search or source-file read for a codebase question, and put this rule in every subagent prompt that explores code. Read raw files only for lines the graph or an earlier read already located. Use `graphify path "<A>" "<B>"` for relationships and `graphify explain "<concept>"` for focused concepts. These return a scoped subgraph, usually much smaller than GRAPH_REPORT.md or raw grep output.
- Dirty graphify-out/ files are expected after hooks or incremental updates; dirty graph files are not a reason to skip graphify. Only skip graphify if the task is about stale or incorrect graph output, or the user explicitly says not to use it.
- If graphify-out/wiki/index.md exists, use it for broad navigation instead of raw source browsing.
- Read graphify-out/GRAPH_REPORT.md only for broad architecture review or when query/path/explain do not surface enough context.
- After modifying code, run `graphify update .` to keep the graph current (AST-only, no API cost).
- A committed graphify-out/graph.json is rewritten whole on every build, so a large diff to it in a pull request is expected. Do not flag it as churn or suggest skipping the graph rebuild or its hooks.
