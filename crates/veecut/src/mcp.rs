//! A Model Context Protocol server on stdio, so agents such as OpenCode can call VeeCut's
//! commands as tools. JSON-RPC 2.0, one message per line. Progress goes to stderr because stdout
//! carries the protocol.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::sync::LazyLock;

use crate::commands::*;

const SUPPORTED: [&str; 4] = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];
const LATEST: &str = "2025-06-18";

struct Tool {
    name: &'static str,
    description: &'static str,
    schema: Value,
}

fn input_schema<T: JsonSchema>() -> Value {
    let mut v = serde_json::to_value(schemars::schema_for!(T))
        .unwrap_or_else(|_| json!({ "type": "object" }));
    if let Some(map) = v.as_object_mut() {
        map.remove("$schema");
        map.remove("title");
        map.entry("type").or_insert(json!("object"));
    }
    v
}

fn tools() -> &'static [Tool] {
    static TOOLS: LazyLock<Vec<Tool>> = LazyLock::new(|| {
        vec![
            Tool {
                name: "status",
                description: "What the current (or named) project holds: kind, length, tracks with item ids, assets, drafts, open notes. Start here.",
                schema: input_schema::<ProjectArg>(),
            },
            Tool {
                name: "edit",
                description: "Edit recordings with the recipe for a kind of video (talking-head-short, talking-head-long, tutorial, podcast, clips, vlog, explainer, promo, music-video, documentary) and render a draft to outbox/. Without files it edits the newest video in inbox/. Explainer accepts audio or video narration, ordered B-roll, optional music and motion hold/push/glide (omit for recipe defaults). Documentary edits take B-roll files and find=true to search free, licence-tracked footage. Takes minutes for long recordings.",
                schema: input_schema::<EditArgs>(),
            },
            Tool {
                name: "ops",
                description: "Apply Editor API operations atomically, for example {\"op\":\"cut_range\",\"from\":2.0,\"to\":3.5} or {\"op\":\"set\",\"path\":\"/captions/size\",\"value\":0.055}. `ops` is one op, an array, or JSON lines. Times are timeline seconds. Call `schema` for every op.",
                schema: input_schema::<OpsArgs>(),
            },
            Tool {
                name: "add",
                description: "Bring files (B-roll footage, music, sound effects, images) into the project as assets; returns their ids for add_clip ops. Paths or names of files in inbox/.",
                schema: input_schema::<AddArgs>(),
            },
            Tool {
                name: "schema",
                description: "JSON schema of every Editor API operation.",
                schema: json!({ "type": "object", "properties": {} }),
            },
            Tool {
                name: "render",
                description: "Render a draft (outbox/<slug>-v<n>.mp4) or, with final, outbox/<slug>-final.mp4, then check size, length and loudness.",
                schema: input_schema::<RenderArgs>(),
            },
            Tool {
                name: "frame",
                description: "One finished frame at a timeline time, as a PNG image you can look at.",
                schema: input_schema::<FrameArgs>(),
            },
            Tool {
                name: "frames",
                description: "A contact sheet of frames across the edit, as a PNG image, with their times.",
                schema: input_schema::<FramesArgs>(),
            },
            Tool {
                name: "transcript",
                description: "Sentences with their timeline and source times, the raw transcript, or the captions as SRT/VTT.",
                schema: input_schema::<TranscriptArgs>(),
            },
            Tool {
                name: "transcript_search",
                description: "Find an exact spoken phrase on the kept timeline, ignoring case and punctuation. Returns every occurrence with word ids and source/timeline start and end times in seconds at millisecond precision. Use occurrence with cut_phrase when words repeat.",
                schema: input_schema::<TranscriptSearchArgs>(),
            },
            Tool {
                name: "cut_phrase",
                description: "Remove an exact spoken phrase through the Editor API and save one undo step. A repeated phrase needs occurrence (one-based, from transcript_search). Never uses fuzzy matching.",
                schema: input_schema::<CutPhraseArgs>(),
            },
            Tool {
                name: "cutout",
                description: "Cut the speaker out for at/dur seconds inside one main-track clip. Optional world picture and mega word sit behind it. Downloads Apache-2.0 MODNet on first use; saves one undo step.",
                schema: input_schema::<crate::cutout_cmd::CutoutArgs>(),
            },
            Tool {
                name: "captions",
                description: "Change caption mode or rebuild caption pages from transcript.json. Saves an undo step.",
                schema: input_schema::<CaptionsArgs>(),
            },
            Tool {
                name: "note",
                description: "Pin the user's note to a moment; with rule=true also save it as a lasting rule in my-rules.md.",
                schema: input_schema::<NoteArgs>(),
            },
            Tool {
                name: "undo",
                description: "Undo the last change to the project.",
                schema: input_schema::<ProjectArg>(),
            },
            Tool {
                name: "redo",
                description: "Restore the change replaced by the last undo.",
                schema: input_schema::<ProjectArg>(),
            },
            Tool {
                name: "skills_search",
                description: "Search the skills catalog synced from awesome-claude-video-skills (refreshes itself when stale).",
                schema: input_schema::<SkillsSearchArgs>(),
            },
            Tool {
                name: "skills_install",
                description: "Install a catalog repo's agent skills into this workspace. Ask the user first.",
                schema: input_schema::<SkillsInstallArgs>(),
            },
            Tool {
                name: "verify_words",
                description: "Transcribe a rendered video again (locally unless the user's config picked Groq) and compare it with the project's transcript: lists words that are missing or heard differently, with their timeline time, whether a cut is within 0.25 s and which sound or music clip starts there, plus a verdict. Takes about as long as transcription.",
                schema: input_schema::<crate::verify_cmd::VerifyWordsArgs>(),
            },
            Tool {
                name: "find_footage",
                description: "Search free, licence-tracked footage and pictures (NASA, Wikimedia Commons, Internet Archive, Openverse; Pexels with PEXELS_API_KEY) for an idea: the real thing a line is about, or a picture metaphor. Returns numbered candidates with licence and flags, plus a contact sheet PNG to look at. Then fetch_footage with the number.",
                schema: input_schema::<crate::footage_cmd::FindArgs>(),
            },
            Tool {
                name: "fetch_footage",
                description: "Download a candidate (its number from the last find_footage, or its id) into footage/ and add it to the project as an asset with its source, licence and credit. Returns the asset id for add_clip. Saves one undo step.",
                schema: input_schema::<crate::footage_cmd::FetchArgs>(),
            },
            Tool {
                name: "sources",
                description: "The project's rights table of found files: licence, author, page, flags. flag (with reason) marks a file for a rights check; credits returns description-ready credit lines. A final render refuses flagged files nobody approved. Approving is the user's decision only: show them the flags and ask them to run `veecut sources --approve <asset>` after checking the rights; never approve on your own.",
                schema: input_schema::<crate::footage_cmd::SourcesArgs>(),
            },
        ]
    });
    &TOOLS
}

fn parse<T: DeserializeOwned>(args: &Value) -> Result<T> {
    let args = if args.is_null() {
        json!({})
    } else {
        args.clone()
    };
    serde_json::from_value(args).context("invalid tool arguments; use the field names and types in tools/list, for example {\"project\":\"my-edit\"}")
}

fn project_call<T: DeserializeOwned>(
    workspace: Option<&Path>,
    args: &Value,
    run: impl FnOnce(&Ctx, T) -> Result<Out>,
) -> Result<Out> {
    let args = parse(args)?;
    run(&Ctx::open(workspace)?, args)
}

/// Rooted MCP authority: a file a tool reads as text must resolve, symlinks included, inside the
/// workspace. The command line keeps its explicit-path freedom.
fn mcp_read_path(ctx: &Ctx, path: &Path) -> Result<PathBuf> {
    let file = resolved(path)?;
    if !file.starts_with(resolved(&ctx.ws.root)?) {
        bail!(
            "{} is outside this workspace; MCP tools only use files inside it",
            path.display()
        );
    }
    Ok(file)
}

/// A file a tool writes: inside the workspace and never in inbox/, where originals stay unchanged.
fn mcp_write_path(ctx: &Ctx, path: &Path) -> Result<PathBuf> {
    let out = mcp_read_path(ctx, path)?;
    if out.starts_with(resolved(&ctx.ws.inbox())?) {
        bail!("MCP tools never write into inbox/; omit out to use the default place");
    }
    Ok(out)
}

fn validate_args(name: &str, args: &Value) -> Result<()> {
    let tool = tools()
        .iter()
        .find(|tool| tool.name == name)
        .ok_or_else(|| {
            anyhow!(
                "unknown tool {name:?}; call tools/list for available names, for example status"
            )
        })?;
    let object = if args.is_null() {
        None
    } else {
        Some(args.as_object().context(
            "tool arguments must be a JSON object, for example {\"project\":\"my-edit\"}; pass them inside tools/call.arguments",
        )?)
    };
    if let Some(properties) = tool.schema["properties"].as_object() {
        if let Some(object) = object {
            for key in object.keys() {
                if !properties.contains_key(key) {
                    bail!(
                        "unknown argument {key:?} for {name}; use fields: {}. Call tools/list for this tool's schema",
                        properties
                            .keys()
                            .map(String::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
            }
        }
    }
    if let Some(required) = tool.schema["required"].as_array() {
        for key in required.iter().filter_map(Value::as_str) {
            if !object.is_some_and(|object| object.contains_key(key)) {
                bail!(
                    "missing argument {key:?} for {name}; call tools/list for its schema, for example transcript_search takes {{\"phrase\":\"hello there\",\"project\":\"my-edit\"}}"
                );
            }
        }
    }
    Ok(())
}

fn call(workspace: &Option<PathBuf>, name: &str, args: &Value) -> Result<Out> {
    validate_args(name, args)?;
    let ws = workspace.as_deref();
    let mut progress = |msg: &str| {
        let _ = writeln!(std::io::stderr(), "veecut: {msg}");
    };
    match name {
        "status" => project_call(ws, args, Ctx::status),
        "add" => project_call(ws, args, Ctx::add),
        "edit" => project_call(ws, args, |ctx, mut args: EditArgs| {
            let copy = args
                .copy
                .as_deref()
                .filter(|copy| Path::new(copy).is_file())
                .map(|copy| mcp_read_path(ctx, Path::new(copy)))
                .transpose()?;
            if let Some(file) = copy {
                args.copy = Some(file.display().to_string());
            }
            ctx.edit(args, &mut progress)
        }),
        "cutout" => project_call(ws, args, |ctx, args| ctx.cutout(args, &mut progress)),
        "ops" => {
            let mut args = args.clone();
            if let Some(ops) = args.get_mut("ops") {
                if !ops.is_string() {
                    *ops = Value::String(ops.to_string());
                }
            }
            // JSON only: never a file or standard input, which carries this protocol.
            project_call(ws, &args, |ctx, args: OpsArgs| {
                let ops = args.ops.context(
                    "give the ops as JSON, for example {\"ops\":{\"op\":\"cut_range\",\"from\":2.0,\"to\":3.5}}; call schema for every op",
                )?;
                ctx.apply_ops(&ops, &args.project)
            })
        }
        "schema" => schema(),
        "render" => project_call(ws, args, |ctx, mut args: RenderArgs| {
            args.out = args.out.map(|o| mcp_write_path(ctx, &o)).transpose()?;
            ctx.render(args, &mut progress)
        }),
        "frame" => project_call(ws, args, |ctx, mut args: FrameArgs| {
            args.out = args.out.map(|o| mcp_write_path(ctx, &o)).transpose()?;
            ctx.frame(args)
        }),
        "frames" => project_call(ws, args, |ctx, mut args: FramesArgs| {
            args.out = args.out.map(|o| mcp_write_path(ctx, &o)).transpose()?;
            ctx.frames(args)
        }),
        "transcript" => project_call(ws, args, Ctx::transcript),
        "transcript_search" => project_call(ws, args, Ctx::transcript_search),
        "cut_phrase" => project_call(ws, args, Ctx::cut_phrase),
        "captions" => project_call(ws, args, Ctx::captions),
        "note" => project_call(ws, args, Ctx::note),
        "undo" => project_call(ws, args, |ctx, args| ctx.undo(args, false)),
        "redo" => project_call(ws, args, |ctx, args| ctx.undo(args, true)),
        "skills_search" => {
            let a: SkillsSearchArgs = parse(args)?;
            skills(
                ws,
                Some(SkillsCommand::Search {
                    query: a.query,
                    category: a.category,
                }),
                &mut progress,
            )
        }
        "skills_install" => {
            let a: SkillsInstallArgs = parse(args)?;
            skills(
                ws,
                Some(SkillsCommand::Install {
                    repo: a.repo,
                    force: a.force,
                }),
                &mut progress,
            )
        }
        "verify_words" => project_call(ws, args, |ctx, args| ctx.verify_words(args, &mut progress)),
        "find_footage" => project_call(ws, args, Ctx::find),
        "fetch_footage" => project_call(ws, args, |ctx, args| ctx.fetch(args, &mut progress)),
        "sources" => project_call(ws, args, Ctx::sources),
        _ => Err(anyhow!("unknown tool {name:?}")),
    }
}

fn tool_result(out: Result<Out>) -> Value {
    match out {
        Ok(out) => {
            let mut content = vec![
                json!({ "type": "text", "text": if out.text.is_empty() { "done".to_string() } else { out.text } }),
            ];
            if let Some(png) = out.png {
                content.push(json!({ "type": "image", "data": base64::engine::general_purpose::STANDARD.encode(png), "mimeType": "image/png" }));
            }
            let structured = if out.json.is_object() {
                out.json
            } else {
                json!({ "result": out.json })
            };
            json!({ "content": content, "structuredContent": structured, "isError": false })
        }
        Err(e) => {
            json!({ "content": [{ "type": "text", "text": format!("{e:#}") }], "isError": true })
        }
    }
}

fn handle(
    workspace: &Option<PathBuf>,
    method: &str,
    params: &Value,
) -> std::result::Result<Value, (i64, String)> {
    match method {
        "initialize" => {
            let asked = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(LATEST);
            let version = if SUPPORTED.contains(&asked) {
                asked
            } else {
                LATEST
            };
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "veecut", "title": "VeeCut Studio", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "VeeCut edits the user's videos in this workspace. Call `status` first. `edit` runs a full recipe and renders a draft; `ops` makes precise changes; `render` makes the next draft; `frame`/`frames` let you look at the result. Never modify inbox/, never upload anything, and ask before paid services."
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({
            "tools": tools().iter().map(|t| json!({ "name": t.name, "description": t.description, "inputSchema": t.schema })).collect::<Vec<_>>()
        })),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or((-32602, "tools/call needs a string name, for example {\"name\":\"status\",\"arguments\":{\"project\":\"my-edit\"}}".to_string()))?;
            if let Some(object) = params.as_object() {
                if let Some(key) = object
                    .keys()
                    .find(|key| !["name", "arguments", "_meta"].contains(&key.as_str()))
                {
                    return Err((
                        -32602,
                        format!(
                            "unknown tools/call parameter {key:?}; put tool fields inside arguments, for example {{\"name\":\"status\",\"arguments\":{{\"project\":\"my-edit\"}}}}"
                        ),
                    ));
                }
            }
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
            Err(e) => Some(
                json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("parse error: {e}") } }),
            ),
            Ok(msg) => {
                let id = msg.get("id").cloned();
                let method = msg
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
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
    use veecut_engine::project::Project;

    #[test]
    fn mcp_edit_motion_uses_shared_optional_enum_and_rejects_wrong_kind() {
        use veecut_engine::recipes::ExplainerMotion;
        let omitted: EditArgs = serde_json::from_value(json!({"kind": "explainer"})).unwrap();
        assert_eq!(omitted.motion, None);
        for motion in ExplainerMotion::ALL {
            let args: EditArgs =
                serde_json::from_value(json!({"kind": "explainer", "motion": motion.slug()}))
                    .unwrap();
            assert_eq!(args.motion, Some(motion));
        }
        assert!(serde_json::from_value::<EditArgs>(json!({"motion": "spin"})).is_err());
        let schema = input_schema::<EditArgs>();
        assert!(schema["properties"].get("motion").is_some());
        let (_temp, ctx, _) = crate::commands::tests::speech_project();
        let result = handle(
            &Some(ctx.ws.root.clone()),
            "tools/call",
            &json!({
                "name": "edit", "arguments": {"kind": "documentary", "motion": "hold"}
            }),
        )
        .unwrap();
        assert_eq!(result["isError"], true, "{result}");
        let error = result["content"][0]["text"].as_str().unwrap();
        assert!(
            error.contains("motion") && error.contains("explainer"),
            "{error}"
        );
        assert_eq!(ctx.ws.list_projects().unwrap().len(), 1);
    }

    #[test]
    fn rooted_mcp_cannot_mutate_another_workspace() {
        let (_temp, ctx, dir) = crate::commands::tests::speech_project();
        let (_other_temp, _other, other_dir) = crate::commands::tests::speech_project();
        let before = std::fs::read(dir.join("project.json")).unwrap();
        let other_before = std::fs::read(other_dir.join("project.json")).unwrap();
        for project in [other_dir.clone(), other_dir.join("project.json")] {
            let result = handle(
                &Some(ctx.ws.root.clone()),
                "tools/call",
                &json!({
                    "name": "cut_phrase", "arguments": {"project": project, "phrase": "keep"}
                }),
            )
            .unwrap();
            assert_eq!(result["isError"], true, "{result}");
            assert!(
                result["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("another workspace")
            );
        }
        assert_eq!(before, std::fs::read(dir.join("project.json")).unwrap());
        assert_eq!(
            other_before,
            std::fs::read(other_dir.join("project.json")).unwrap()
        );
        assert!(!dir.join(".history").exists() && !other_dir.join(".history").exists());
    }

    #[test]
    fn project_selectors_and_phrase_calls_share_engine_and_history() {
        let (_temp, ctx, dir) = crate::commands::tests::speech_project();
        let workspace = Some(ctx.ws.root.clone());
        for project in [
            "my-edit".to_string(),
            "projects/my-edit".into(),
            "projects/my-edit/project.json".into(),
            dir.display().to_string(),
            dir.join("project.json").display().to_string(),
        ] {
            let status = handle(
                &workspace,
                "tools/call",
                &json!({
                    "name": "status", "arguments": {"project": project}
                }),
            )
            .unwrap();
            assert_eq!(status["isError"], false, "{status}");
            assert_eq!(status["structuredContent"]["name"], "My edit");
            let search = handle(&workspace, "tools/call", &json!({
                "name": "transcript_search", "arguments": {"project": project, "phrase": "HELLO THERE"}
            })).unwrap();
            assert_eq!(search["isError"], false, "{search}");
            assert_eq!(
                search["structuredContent"]["matches"]
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
            assert_eq!(
                search["structuredContent"]["matches"][1]["timeline_start"],
                5.123
            );
        }
        let before = std::fs::read(dir.join("project.json")).unwrap();
        let ambiguous = handle(
            &workspace,
            "tools/call",
            &json!({
                "name": "cut_phrase", "arguments": {"project": "my-edit", "phrase": "hello there"}
            }),
        )
        .unwrap();
        assert_eq!(ambiguous["isError"], true, "{ambiguous}");
        assert!(
            ambiguous["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("occurrence")
        );
        assert_eq!(before, std::fs::read(dir.join("project.json")).unwrap());
        assert!(!dir.join(".history").exists());
        let cut = handle(&workspace, "tools/call", &json!({
            "name": "cut_phrase",
            "arguments": {"project": "projects/my-edit/project.json", "phrase": "hello there", "occurrence": 2}
        })).unwrap();
        assert_eq!(cut["isError"], false, "{cut}");
        assert_eq!(cut["structuredContent"]["cut"]["occurrence"], 2);
        assert_eq!(std::fs::read_dir(dir.join(".history")).unwrap().count(), 1);
        let changed = std::fs::read(dir.join("project.json")).unwrap();
        assert_ne!(before, changed);
        assert!(call(&workspace, "undo", &json!({"project": "my-edit"})).is_ok());
        assert_eq!(before, std::fs::read(dir.join("project.json")).unwrap());
        assert!(call(&workspace, "redo", &json!({"project": "my-edit"})).is_ok());
        assert_eq!(changed, std::fs::read(dir.join("project.json")).unwrap());
        let captions = call(
            &workspace,
            "captions",
            &json!({"project": "my-edit", "mode": "subtitle"}),
        )
        .unwrap();
        assert_eq!(captions.json["mode"], "subtitle");
    }

    #[test]
    fn schemas_and_malformed_calls_explain_current_argument_names() {
        let frame = tools().iter().find(|tool| tool.name == "frame").unwrap();
        assert_eq!(frame.schema["properties"]["at"]["default"], 0.0);
        assert!(
            !frame.schema["required"]
                .as_array()
                .is_some_and(|fields| fields.contains(&json!("at")))
        );
        let cut = tools()
            .iter()
            .find(|tool| tool.name == "cut_phrase")
            .unwrap();
        assert!(cut.schema["properties"].get("phrase").is_some());
        assert!(cut.schema["properties"].get("occurrence").is_some());
        assert!(cut.schema["properties"].get("all").is_none());
        let undo = tools().iter().find(|tool| tool.name == "undo").unwrap();
        assert!(undo.schema["properties"].get("redo").is_none());
        for (name, args, expected) in [
            ("status", json!({"slug": "my-edit"}), "project"),
            ("frame", json!({"at": "later"}), "tools/list"),
            ("transcript_search", json!({}), "phrase"),
            (
                "cut_phrase",
                json!({"phrase": "hello", "all": true}),
                "unknown argument",
            ),
            ("note", json!({}), "note"),
            ("skills_search", json!({}), "query"),
            ("status", json!("my-edit"), "JSON object"),
            ("undo", json!({"redo": true}), "unknown argument"),
        ] {
            let error = call(&None, name, &args).err().unwrap();
            assert!(format!("{error:#}").contains(expected), "{name}: {error:#}");
        }
        let missing_name = handle(&None, "tools/call", &json!({"arguments": {}})).unwrap_err();
        assert_eq!(missing_name.0, -32602);
        assert!(missing_name.1.contains("\"name\":\"status\""));
        let misplaced = handle(
            &None,
            "tools/call",
            &json!({
                "name": "cut_phrase", "project": "my-edit", "arguments": {"phrase": "keep"}
            }),
        )
        .unwrap_err();
        assert_eq!(misplaced.0, -32602);
        assert!(misplaced.1.contains("inside arguments"));
    }

    #[test]
    fn handshake_tools_and_errors() {
        let init = handle(
            &None,
            "initialize",
            &json!({ "protocolVersion": "2025-03-26" }),
        )
        .unwrap();
        assert_eq!(init["protocolVersion"], "2025-03-26");
        let unknown = handle(
            &None,
            "initialize",
            &json!({ "protocolVersion": "1999-01-01" }),
        )
        .unwrap();
        assert_eq!(unknown["protocolVersion"], LATEST);
        let list = handle(&None, "tools/list", &Value::Null).unwrap();
        let tools = list["tools"].as_array().unwrap();
        assert!(
            tools.iter().all(|t| t["inputSchema"]["type"] == "object"
                && t["inputSchema"].get("$schema").is_none())
        );
        assert_eq!(handle(&None, "nope", &Value::Null).unwrap_err().0, -32601);
        let schema = tool_result(call(&None, "schema", &Value::Null));
        assert_eq!(schema["isError"], false);
        let bad = tool_result(call(&None, "nope", &Value::Null));
        assert_eq!(bad["isError"], true);
    }

    #[test]
    fn mcp_ops_take_json_only_and_errors_never_echo_input() {
        let (_temp, ctx, dir) = crate::commands::tests::speech_project();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "AUDIT_SECRET=synthetic-marker\n").unwrap();
        let before = std::fs::read(dir.join("project.json")).unwrap();
        let workspace = Some(ctx.ws.root.clone());
        for (arguments, expected) in [
            (
                json!({"project": "my-edit", "file": secret}),
                "unknown argument",
            ),
            (
                json!({"project": "my-edit", "ops": "AUDIT_SECRET=synthetic-marker"}),
                "line 1",
            ),
            (
                json!({"project": "my-edit", "ops": "{\"op\":\"cut_range\",\"from\":1,\"to\":2}\nAUDIT_SECRET=synthetic-marker"}),
                "line 2",
            ),
        ] {
            let result = handle(
                &workspace,
                "tools/call",
                &json!({"name": "ops", "arguments": arguments}),
            )
            .unwrap();
            assert_eq!(result["isError"], true, "{result}");
            let text = result.to_string();
            assert!(!text.contains("synthetic-marker"), "{text}");
            assert!(text.contains(expected), "{text}");
        }
        assert_eq!(before, std::fs::read(dir.join("project.json")).unwrap());
    }

    #[test]
    fn rooted_mcp_outputs_stay_inside_the_workspace() {
        let (_temp, ctx, dir) = crate::commands::tests::speech_project();
        for folder in ["inbox", "outbox", "projects/my-edit/.cache"] {
            std::fs::create_dir_all(ctx.ws.root.join(folder)).unwrap();
        }
        let other = tempfile::tempdir().unwrap();
        let victim = other.path().join("project.json");
        std::fs::write(&victim, b"{}").unwrap();
        let workspace = Some(ctx.ws.root.clone());
        let mut outs = vec![victim.clone()];
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(other.path(), dir.join(".cache/escape")).unwrap();
            outs.push(dir.join(".cache/escape/project.json"));
        }
        for out in outs {
            for (name, arguments) in [
                (
                    "frame",
                    json!({"project": "my-edit", "at": 20.0, "out": out}),
                ),
                ("render", json!({"project": "my-edit", "out": out})),
            ] {
                let result = handle(
                    &workspace,
                    "tools/call",
                    &json!({"name": name, "arguments": arguments}),
                )
                .unwrap();
                assert_eq!(result["isError"], true, "{result}");
                let text = result["content"][0]["text"].as_str().unwrap();
                assert!(text.contains("outside this workspace"), "{name}: {text}");
            }
        }
        assert_eq!(std::fs::read(&victim).unwrap(), b"{}");
        let inbox = ctx.ws.inbox().join("frame.png");
        let refused = handle(
            &workspace,
            "tools/call",
            &json!({
                "name": "frame", "arguments": {"project": "my-edit", "at": 20.0, "out": inbox}
            }),
        )
        .unwrap();
        assert_eq!(refused["isError"], true, "{refused}");
        assert!(!inbox.exists());
        let inside = dir.join(".cache/check.png");
        let saved = handle(
            &workspace,
            "tools/call",
            &json!({
                "name": "frame", "arguments": {"project": "my-edit", "at": 20.0, "out": inside}
            }),
        )
        .unwrap();
        assert_eq!(saved["isError"], false, "{saved}");
        assert!(inside.is_file());
    }

    #[test]
    fn ops_cannot_approve_unflag_or_replace_sources_from_cli_or_mcp() {
        let (_temp, ctx, dir) = crate::commands::tests::speech_project();
        let file = dir.join("project.json");
        let mut project = Project::load(&file).unwrap();
        project.sources.insert(
            "speech".into(),
            serde_json::from_value(json!({
                "url": "https://example.org/speech", "license": "CC BY-NC 2.0",
                "flag": true, "flag_reason": "non-commercial"
            }))
            .unwrap(),
        );
        project.save(&file).unwrap();
        let mut approved = serde_json::to_value(&project).unwrap();
        approved["sources"]["speech"]["approved"] = json!(true);
        let before = std::fs::read(&file).unwrap();
        let workspace = Some(ctx.ws.root.clone());
        for op in [
            json!({"op": "set", "path": "/sources/speech/approved", "value": true}),
            json!({"op": "set", "path": "/sources/speech/flag", "value": false}),
            json!([{"op": "set", "path": "/sources/speech/flag_reason", "value": "fine"}]),
            json!({"op": "set", "path": "/sources/speech", "value": {"url": "https://example.org", "license": "CC0"}}),
            json!({"op": "set", "path": "/sources", "value": {}}),
            json!({"op": "set", "path": "", "value": approved}),
            json!({"op": "set", "path": "/", "value": approved}),
        ] {
            let cli = ctx
                .ops(OpsArgs {
                    ops: Some(op.to_string()),
                    ..Default::default()
                })
                .err()
                .unwrap();
            assert_eq!(format!("{cli:#}"), APPROVAL_IS_HUMAN, "{op}");
            let mcp = handle(
                &workspace,
                "tools/call",
                &json!({"name": "ops", "arguments": {"project": "my-edit", "ops": op}}),
            )
            .unwrap();
            assert_eq!(mcp["isError"], true, "{mcp}");
            assert_eq!(mcp["content"][0]["text"], APPROVAL_IS_HUMAN, "{mcp}");
        }
        assert_eq!(before, std::fs::read(&file).unwrap());
        let credit = json!({"op": "set", "path": "/sources/speech/credit", "value": "Clip by A"});
        call(
            &workspace,
            "ops",
            &json!({"project": "my-edit", "ops": credit}),
        )
        .unwrap();
        let saved = Project::load(&file).unwrap();
        assert_eq!(saved.sources["speech"].credit.as_deref(), Some("Clip by A"));
        assert!(saved.sources["speech"].blocks_final());
    }
}
