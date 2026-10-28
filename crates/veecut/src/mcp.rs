//! A Model Context Protocol server on stdio, so agents such as OpenCode can call VeeCut's
//! commands as tools. JSON-RPC 2.0, one message per line. Progress goes to stderr because stdout
//! carries the protocol.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use base64::Engine as _;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::commands::*;

const SUPPORTED: [&str; 4] = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];
const LATEST: &str = "2025-06-18";

#[derive(Deserialize, JsonSchema, Default)]
#[serde(default)]
struct UndoArgs {
    /// Redo instead of undo.
    redo: bool,
    #[serde(flatten)]
    project: ProjectArg,
}

struct Tool {
    name: &'static str,
    description: &'static str,
    schema: Value,
}

fn input_schema<T: JsonSchema>() -> Value {
    let mut v = serde_json::to_value(schemars::schema_for!(T)).unwrap_or_else(|_| json!({ "type": "object" }));
    if let Some(map) = v.as_object_mut() {
        map.remove("$schema");
        map.remove("title");
        map.entry("type").or_insert(json!("object"));
    }
    v
}

fn tools() -> Vec<Tool> {
    vec![
        Tool { name: "status", description: "What the current (or named) project holds: kind, length, tracks with item ids, assets, drafts, open notes. Start here.", schema: input_schema::<ProjectArg>() },
        Tool { name: "edit", description: "Edit recordings with the recipe for a kind of video (talking-head-short, talking-head-long, tutorial, podcast, clips, vlog, explainer, promo, music-video) and render a draft to outbox/. Without files it edits the newest video in inbox/. Takes minutes for long recordings.", schema: input_schema::<EditArgs>() },
        Tool { name: "ops", description: "Apply Editor API operations atomically, for example {\"op\":\"cut_range\",\"from\":2.0,\"to\":3.5} or {\"op\":\"set\",\"path\":\"/captions/size\",\"value\":0.055}. `ops` is one op, an array, or JSON lines. Times are timeline seconds. Call `schema` for every op.", schema: input_schema::<OpsArgs>() },
        Tool { name: "schema", description: "JSON schema of every Editor API operation.", schema: json!({ "type": "object", "properties": {} }) },
        Tool { name: "render", description: "Render a draft (outbox/<slug>-v<n>.mp4) or, with final, outbox/<slug>-final.mp4, then check size, length and loudness.", schema: input_schema::<RenderArgs>() },
        Tool { name: "frame", description: "One finished frame at a timeline time, as a PNG image you can look at.", schema: input_schema::<FrameArgs>() },
        Tool { name: "frames", description: "A contact sheet of frames across the edit, as a PNG image, with their times.", schema: input_schema::<FramesArgs>() },
        Tool { name: "transcript", description: "Sentences with their timeline and source times, the raw transcript, or the captions as SRT/VTT.", schema: input_schema::<TranscriptArgs>() },
        Tool { name: "note", description: "Pin the user's note to a moment; with rule=true also save it as a lasting rule in my-rules.md.", schema: input_schema::<NoteArgs>() },
        Tool { name: "undo", description: "Undo (or redo) the last change to the project.", schema: input_schema::<UndoArgs>() },
        Tool { name: "skills_search", description: "Search the skills catalog synced from awesome-claude-video-skills (refreshes itself when stale).", schema: input_schema::<SkillsSearchArgs>() },
        Tool { name: "skills_install", description: "Install a catalog repo's agent skills into this workspace. Ask the user first.", schema: input_schema::<SkillsInstallArgs>() },
    ]
}

fn parse<T: DeserializeOwned>(args: &Value) -> Result<T> {
    let args = if args.is_null() { json!({}) } else { args.clone() };
    serde_json::from_value(args).context("the arguments do not match the tool's schema")
}

fn call(workspace: &Option<PathBuf>, name: &str, args: &Value) -> Result<Out> {
    let ws = workspace.as_deref();
    let mut progress = |msg: &str| {
        let _ = writeln!(std::io::stderr(), "veecut: {msg}");
    };
    match name {
        "status" => Ctx::open(ws)?.status(parse(args)?),
        "edit" => Ctx::open(ws)?.edit(parse(args)?, &mut progress),
        "ops" => {
            let mut args = args.clone();
            if let Some(ops) = args.get_mut("ops") {
                if !ops.is_string() {
                    *ops = Value::String(ops.to_string());
                }
            }
            Ctx::open(ws)?.ops(parse(&args)?)
        }
        "schema" => schema(),
        "render" => Ctx::open(ws)?.render(parse(args)?, &mut progress),
        "frame" => Ctx::open(ws)?.frame(parse(args)?),
        "frames" => Ctx::open(ws)?.frames(parse(args)?),
        "transcript" => Ctx::open(ws)?.transcript(parse(args)?),
        "note" => Ctx::open(ws)?.note(parse(args)?),
        "undo" => {
            let a: UndoArgs = parse(args)?;
            Ctx::open(ws)?.undo(a.project, a.redo)
        }
        "skills_search" => {
            let a: SkillsSearchArgs = parse(args)?;
            skills(ws, Some(SkillsCommand::Search { query: a.query, category: a.category }), &mut progress)
        }
        "skills_install" => {
            let a: SkillsInstallArgs = parse(args)?;
            skills(ws, Some(SkillsCommand::Install { repo: a.repo, force: a.force }), &mut progress)
        }
        _ => Err(anyhow!("unknown tool {name:?}")),
    }
}

fn tool_result(out: Result<Out>) -> Value {
    match out {
        Ok(out) => {
            let mut content = vec![json!({ "type": "text", "text": if out.text.is_empty() { "done".to_string() } else { out.text } })];
            if let Some(png) = out.png {
                content.push(json!({ "type": "image", "data": base64::engine::general_purpose::STANDARD.encode(png), "mimeType": "image/png" }));
            }
            let structured = if out.json.is_object() { out.json } else { json!({ "result": out.json }) };
            json!({ "content": content, "structuredContent": structured, "isError": false })
        }
        Err(e) => json!({ "content": [{ "type": "text", "text": format!("{e:#}") }], "isError": true }),
    }
}

fn handle(workspace: &Option<PathBuf>, method: &str, params: &Value) -> std::result::Result<Value, (i64, String)> {
    match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(LATEST);
            let version = if SUPPORTED.contains(&asked) { asked } else { LATEST };
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "veecut", "title": "VeeCut Studio", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "VeeCut edits the user's videos in this workspace. Call `status` first. `edit` runs a full recipe and renders a draft; `ops` makes precise changes; `render` makes the next draft; `frame`/`frames` let you look at the result. Never modify inbox/, never upload anything, and ask before paid services."
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({
            "tools": tools().into_iter().map(|t| json!({ "name": t.name, "description": t.description, "inputSchema": t.schema })).collect::<Vec<_>>()
        })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).ok_or((-32602, "tools/call needs a name".to_string()))?;
            let args = params.get("arguments").cloned().unwrap_or(Value::Null);
            Ok(tool_result(call(workspace, name, &args)))
        }
        "resources/list" => Ok(json!({ "resources": [] })),
        "prompts/list" => Ok(json!({ "prompts": [] })),
        _ => Err((-32601, format!("method not found: {method}"))),
    }
}

pub fn serve(workspace: Option<PathBuf>) -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Err(e) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("parse error: {e}") } })),
            Ok(msg) => {
                let id = msg.get("id").cloned();
                let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                // Notifications (no id) such as notifications/initialized get no reply.
                id.map(|id| match handle(&workspace, method, &params) {
                    Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                    Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
                })
            }
        };
        if let Some(reply) = reply {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_tools_and_errors() {
        let init = handle(&None, "initialize", &json!({ "protocolVersion": "2025-03-26" })).unwrap();
        assert_eq!(init["protocolVersion"], "2025-03-26");
        let unknown = handle(&None, "initialize", &json!({ "protocolVersion": "1999-01-01" })).unwrap();
        assert_eq!(unknown["protocolVersion"], LATEST);
        let list = handle(&None, "tools/list", &Value::Null).unwrap();
        let tools = list["tools"].as_array().unwrap();
        assert!(tools.iter().all(|t| t["inputSchema"]["type"] == "object" && t["inputSchema"].get("$schema").is_none()));
        assert_eq!(handle(&None, "nope", &Value::Null).unwrap_err().0, -32601);
        let schema = tool_result(call(&None, "schema", &Value::Null));
        assert_eq!(schema["isError"], false);
        let bad = tool_result(call(&None, "nope", &Value::Null));
        assert_eq!(bad["isError"], true);
    }
}
