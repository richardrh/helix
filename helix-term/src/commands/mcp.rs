//! Editor UI wiring for MCP servers (`:mcp*` commands, startup auto-connect).
//!
//! All network work runs on the job runtime. Errors coming from `crate::mcp`
//! are already sanitized; we only surface them as concise status messages.

use super::*;
use crate::job::{self, Callback, Jobs};
use helix_core::command_line::Args;
use helix_core::Transaction;

pub const DEFAULT_SERVER: &str = "mirage";

fn server_arg(args: &Args, idx: usize) -> String {
    args.get(idx)
        .map(|s| s.to_string())
        .unwrap_or_else(|| DEFAULT_SERVER.to_string())
}

fn status_cb(result: Result<String, String>) -> Callback {
    Callback::Editor(Box::new(move |editor: &mut Editor| match result {
        Ok(msg) => editor.set_status(msg),
        Err(err) => editor.set_error(err),
    }))
}

/// Open a new scratch buffer containing `text`. Never touches existing buffers.
pub fn open_scratch(editor: &mut Editor, text: String) {
    editor.new_file(Action::Replace);
    let (view, doc) = current!(editor);
    let tx = Transaction::change(doc.text(), [(0, 0, Some(text.into()))].into_iter());
    doc.apply(&tx, view.id);
}

fn scratch_cb(result: Result<String, String>) -> Callback {
    Callback::Editor(Box::new(move |editor: &mut Editor| match result {
        Ok(text) => open_scratch(editor, text),
        Err(err) => editor.set_error(err),
    }))
}

pub fn spawn_connect(jobs: &mut Jobs, name: String) {
    jobs.callback(async move {
        let res = crate::mcp::global()
            .connect(&name)
            .await
            .map(|summary| format!("MCP {name}: {summary}"))
            .map_err(|e| format!("MCP {name}: {e}"));
        Ok(status_cb(res))
    });
}

/// Background auto-connect of configured servers. Non-blocking; failures
/// only produce a status message.
pub fn autoconnect(jobs: &mut Jobs) {
    for name in crate::mcp::global().autoconnect_servers() {
        spawn_connect(jobs, name);
    }
}

pub fn format_status() -> String {
    let status = crate::mcp::global().status();
    if status.is_empty() {
        return "MCP: no servers configured (see :mcp-config)".into();
    }
    status
        .iter()
        .map(|s| {
            let state = if s.connected { "connected" } else { "disconnected" };
            if s.detail.is_empty() {
                format!("{}: {state}", s.name)
            } else {
                format!("{}: {state} ({})", s.name, s.detail)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn mcp_connect(cx: &mut compositor::Context, args: Args, ev: PromptEvent) -> anyhow::Result<()> {
    if ev != PromptEvent::Validate {
        return Ok(());
    }
    let name = server_arg(&args, 0);
    cx.editor.set_status(format!("MCP {name}: connecting..."));
    spawn_connect(cx.jobs, name);
    Ok(())
}

pub fn mcp_disconnect(cx: &mut compositor::Context, args: Args, ev: PromptEvent) -> anyhow::Result<()> {
    if ev != PromptEvent::Validate {
        return Ok(());
    }
    let name = server_arg(&args, 0);
    cx.jobs.callback(async move {
        let res = crate::mcp::global()
            .disconnect(&name)
            .await
            .map(|_| format!("MCP {name}: disconnected"))
            .map_err(|e| format!("MCP {name}: {e}"));
        Ok(status_cb(res))
    });
    Ok(())
}

pub fn mcp_status(cx: &mut compositor::Context, _args: Args, ev: PromptEvent) -> anyhow::Result<()> {
    if ev != PromptEvent::Validate {
        return Ok(());
    }
    cx.editor.set_status(format_status());
    Ok(())
}

fn tools_job(jobs: &mut Jobs, name: String) {
    jobs.callback(async move {
        let res = crate::mcp::global()
            .list_tools(&name)
            .await
            .map(|tools| {
                let mut out = format!("# MCP tools on '{name}' ({})\n\n", tools.len());
                for t in tools {
                    out.push_str(&format!("- {}: {}\n", t.name, t.description));
                }
                out.push_str(&format!("\nInvoke with :mcp-call {name} <tool> <json-args>\n"));
                out
            })
            .map_err(|e| format!("MCP {name}: {e}"));
        Ok(scratch_cb(res))
    });
}

pub fn mcp_tools(cx: &mut compositor::Context, args: Args, ev: PromptEvent) -> anyhow::Result<()> {
    if ev != PromptEvent::Validate {
        return Ok(());
    }
    tools_job(cx.jobs, server_arg(&args, 0));
    Ok(())
}

/// Explicit user invocation only: `:mcp-call <server> <tool> [json]`.
pub fn mcp_call(cx: &mut compositor::Context, args: Args, ev: PromptEvent) -> anyhow::Result<()> {
    if ev != PromptEvent::Validate {
        return Ok(());
    }
    let server = args.first().map(|s| s.to_string()).ok_or_else(|| anyhow!("usage: :mcp-call <server> <tool> [json]"))?;
    let tool = args.get(1).map(|s| s.to_string()).ok_or_else(|| anyhow!("usage: :mcp-call <server> <tool> [json]"))?;
    let raw = args.get(2).unwrap_or("").to_string();
    let json: Value = if raw.trim().is_empty() {
        Value::Object(Default::default())
    } else {
        serde_json::from_str(&raw).map_err(|_| anyhow!("MCP: arguments must be valid JSON"))?
    };
    cx.editor.set_status(format!("MCP {server}: calling {tool}..."));
    cx.jobs.callback(async move {
        let res = crate::mcp::global()
            .call_tool(&server, &tool, json)
            .await
            .map(|text| format!("# MCP {server}/{tool}\n\n{text}\n"))
            .map_err(|e| format!("MCP {server}/{tool}: {e}"));
        Ok(scratch_cb(res))
    });
    Ok(())
}

/// Open the trusted user mcp.toml, creating it from examples if absent.
pub fn open_config(editor: &mut Editor) -> anyhow::Result<()> {
    let path = crate::mcp::config_path();
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // create_new: never overwrite an existing file.
        use std::io::Write as _;
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => f.write_all(crate::mcp::EXAMPLE_CONFIG.as_bytes())?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    editor.open(&path, Action::Replace)?;
    Ok(())
}

pub fn mcp_config(cx: &mut compositor::Context, _args: Args, ev: PromptEvent) -> anyhow::Result<()> {
    if ev != PromptEvent::Validate {
        return Ok(());
    }
    open_config(cx.editor)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagerAction {
    ConnectMirage,
    Status,
    ListTools,
    EditConfig,
    Disconnect,
}

pub const MANAGER_ACTIONS: &[(ManagerAction, &str)] = &[
    (ManagerAction::ConnectMirage, "Connect Mirage"),
    (ManagerAction::Status, "View status"),
    (ManagerAction::ListTools, "List tools (Mirage)"),
    (ManagerAction::EditConfig, "Add / edit servers (mcp.toml)"),
    (ManagerAction::Disconnect, "Disconnect Mirage"),
];

/// `:mcp` manager picker.
pub fn mcp_manager(cx: &mut compositor::Context, _args: Args, ev: PromptEvent) -> anyhow::Result<()> {
    if ev != PromptEvent::Validate {
        return Ok(());
    }
    cx.jobs.callback(async move {
        let call: job::Callback = Callback::EditorCompositor(Box::new(
            move |_editor: &mut Editor, compositor: &mut Compositor| {
                let columns = [ui::PickerColumn::new(
                    "MCP",
                    |item: &(ManagerAction, &'static str), _| item.1.into(),
                )];
                let picker = ui::Picker::new(
                    columns,
                    0,
                    MANAGER_ACTIONS.iter().copied(),
                    (),
                    move |cx, (action, _), _| match action {
                        ManagerAction::ConnectMirage => {
                            cx.editor.set_status("MCP mirage: connecting...");
                            spawn_connect(cx.jobs, DEFAULT_SERVER.to_string());
                        }
                        ManagerAction::Status => cx.editor.set_status(format_status()),
                        ManagerAction::ListTools => tools_job(cx.jobs, DEFAULT_SERVER.to_string()),
                        ManagerAction::EditConfig => {
                            if let Err(e) = open_config(cx.editor) {
                                cx.editor.set_error(format!("MCP config: {e}"));
                            }
                        }
                        ManagerAction::Disconnect => {
                            let name = DEFAULT_SERVER.to_string();
                            cx.jobs.callback(async move {
                                let res = crate::mcp::global()
                                    .disconnect(&name)
                                    .await
                                    .map(|_| format!("MCP {name}: disconnected"))
                                    .map_err(|e| format!("MCP {name}: {e}"));
                                Ok(status_cb(res))
                            });
                        }
                    },
                );
                compositor.push(Box::new(overlaid(picker)))
            },
        ));
        Ok(call)
    });
    Ok(())
}
