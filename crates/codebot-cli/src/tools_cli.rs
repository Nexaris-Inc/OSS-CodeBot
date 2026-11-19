use crate::Args;
use codebot_core::{
    AgentEvent, ChatMessage, CommandEvent, CommandState, ToolAction, ToolConversation, ToolResult,
    ToolStatus,
};
use std::{
    io::{self, BufRead, Read, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};
pub(super) fn confirm(word: &str, cancel: &AtomicBool) -> Result<bool, String> {
    eprintln!("表示した内容を確認し、続ける場合は {word} と入力してください:");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let result = Read::take(io::stdin().lock(), 64)
            .read_line(&mut text)
            .map(|_| text);
        let _ = tx.send(result);
    });
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("ツール相談を中止しました".into());
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Ok(text)) => return Ok(text.trim() == word),
            Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("確認入力を読めません".into())
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}
pub(super) fn run(args: &Args) -> Result<(), String> {
    let config = codebot_core::Config::load(args.config.as_deref())?;
    let backend =
        codebot_core::Backend::from_id(&config.provider).ok_or("APIサービスを選択してください")?;
    let workspace = args.workspace.as_deref().unwrap();
    let skills = codebot_core::resolve_skills(&args.skills, Some(workspace))?;
    let mut messages = vec![ChatMessage::user(args.input.clone().unwrap())];
    codebot_core::prepend_skills(&config, &mut messages, &skills)?;
    // Wider input limit for the skill text; the total stays within the context budget.
    let mut limits = config.clone();
    limits.max_input_chars = limits
        .max_input_chars
        .saturating_add(codebot_core::skill_preamble(&skills).chars().count());
    let connectors = args
        .connectors
        .iter()
        .map(|name| {
            config
                .connectors
                .iter()
                .find(|c| &c.name == name)
                .cloned()
                .ok_or_else(|| {
                    format!("コネクター「{name}」が設定ファイルの [[connectors]] にありません")
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = cancel.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))
        .map_err(|_| "Ctrl+Cを準備できません")?;
    let mut tools = Vec::new();
    for connector in &connectors {
        eprintln!(
            "コネクター「{}」に接続してツールを確認しています…",
            connector.name
        );
        let listed = codebot_core::list_connector_tools(connector, Some(workspace), &cancel)
            .map_err(|e| format!("コネクター「{}」：{e}", connector.name))?;
        eprintln!(
            "  ツール {}件: {}",
            listed.len(),
            listed
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        tools.extend(listed);
    }
    for skill in &skills {
        eprintln!("スキル: {}（{}）", skill.name, skill.source.label());
    }
    let mut conversation = ToolConversation::with_connectors(
        &limits,
        backend,
        &config.api,
        None,
        &messages,
        workspace,
        &tools,
    )?;
    let resolved = config.api.resolved(backend);
    println!(
        "ツール相談の送信先：{} / {} / {}\n作業フォルダ：{}\n各操作と結果送信を個別に確認します。",
        resolved.base_url,
        resolved.model,
        resolved.protocol.label(),
        conversation.root().display()
    );
    loop {
        let reply = conversation.request(&cancel)?;
        for event in &reply.events {
            if let AgentEvent::Text(text) = event {
                println!("{text}");
            }
        }
        let Some(call) = reply.call() else {
            return Ok(());
        };
        println!(
            "ツール要求：{} / {}\n{}",
            call.name,
            call.id,
            serde_json::to_string_pretty(&call.arguments).unwrap()
        );
        let result = match conversation.connector_call(call) {
            Some(found) => {
                let (tool, arguments) = found?;
                let connector = connectors
                    .iter()
                    .find(|c| c.name == tool.connector)
                    .ok_or("コネクターの設定が見つかりません")?;
                execute_connector(connector, &tool, &arguments, conversation.root(), &cancel)?
            }
            None => execute_proposal(
                call,
                conversation.root(),
                args.tools_journal_dir.as_deref(),
                &cancel,
            )?,
        };
        println!(
            "AIへ送る結果：\n{}",
            serde_json::to_string_pretty(&result).unwrap()
        );
        if cancel.load(Ordering::Relaxed) {
            eprintln!("操作を停止しました。結果はAIへ送っていません。");
            return Ok(());
        }
        if !confirm("send", &cancel)? {
            eprintln!("結果はAIへ送らずに相談を終了しました。");
            return Ok(());
        }
        conversation.resolve(&call.id, &result)?;
    }
}

/// Show a connector tool call and run it only after the user types `run`.
fn execute_connector(
    connector: &codebot_core::ConnectorConfig,
    tool: &codebot_core::ConnectorTool,
    arguments: &serde_json::Value,
    root: &std::path::Path,
    cancel: &AtomicBool,
) -> Result<ToolResult, String> {
    println!(
        "コネクター「{}」のツール「{}」\n起動するプログラム：{} {}\n作業場所：{}\n追加の環境変数：{}\nタイムアウト：{}秒",
        connector.name,
        tool.name,
        connector.command,
        connector.args.join(" "),
        root.display(),
        if connector.env_names.is_empty() { "なし".into() } else { connector.env_names.join(", ") },
        connector.timeout_seconds
    );
    if !confirm("run", cancel)? {
        return Ok(ToolResult {
            status: ToolStatus::Rejected,
            summary: "利用者がコネクターツールの実行を拒否しました。実行していません。".into(),
        });
    }
    Ok(
        match codebot_core::call_connector_tool(
            connector,
            Some(root),
            &tool.name,
            arguments,
            cancel,
        ) {
            Ok(output) if output.is_error => ToolResult {
                status: ToolStatus::Failed,
                summary: format!("ツールがエラーを返しました。\n{}", output.text),
            },
            Ok(output) => ToolResult {
                status: ToolStatus::Succeeded,
                summary: if output.text.trim().is_empty() {
                    "ツールは成功しました（出力なし）。".into()
                } else {
                    output.text
                },
            },
            Err(_) if cancel.load(Ordering::Relaxed) => ToolResult {
                status: ToolStatus::Cancelled,
                summary: "利用者がコネクターツールの実行を停止しました。".into(),
            },
            Err(error) => ToolResult {
                status: ToolStatus::Failed,
                summary: error,
            },
        },
    )
}
pub(super) fn execute_proposal(
    call: &codebot_core::ToolCall,
    root: &std::path::Path,
    journal_dir: Option<&std::path::Path>,
    cancel: &AtomicBool,
) -> Result<ToolResult, String> {
    Ok(match call.action(root)? {
        ToolAction::Command(spec) => match codebot_core::prepare_command(&spec) {
            Ok(prepared) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(prepared.preview()).unwrap()
                );
                if !confirm("run", cancel)? {
                    ToolResult {
                        status: ToolStatus::Rejected,
                        summary: "利用者がコマンド提案を拒否しました。実行していません。".into(),
                    }
                } else {
                    let mut output = String::new();
                    let report = codebot_core::run_command(&prepared, cancel, |event| {
                        let text = match event {
                            CommandEvent::Stdout(text) => {
                                print!("{text}");
                                let _ = io::stdout().flush();
                                text
                            }
                            CommandEvent::Stderr(text) => {
                                eprint!("{text}");
                                text
                            }
                        };
                        output.push_str(&text);
                    });
                    let summary = format!(
                        "{}\n出力：\n{}",
                        serde_json::to_string(&report).unwrap(),
                        output
                    );
                    let summary = if summary.chars().count() > 12000 {
                        format!(
                            "{}\n（以降を省略）",
                            summary.chars().take(11980).collect::<String>()
                        )
                    } else {
                        summary
                    };
                    ToolResult {
                        status: if report.state == CommandState::Cancelled {
                            ToolStatus::Cancelled
                        } else if report.state == CommandState::Exited
                            && report.exit_code == Some(0)
                        {
                            ToolStatus::Succeeded
                        } else {
                            ToolStatus::Failed
                        },
                        summary,
                    }
                }
            }
            Err(error) => ToolResult {
                status: ToolStatus::Failed,
                summary: error,
            },
        },
        ToolAction::Changes(proposal) => match codebot_core::prepare_changes(root, &proposal) {
            Ok(prepared) => {
                for preview in prepared.previews() {
                    println!("{}", preview.diff);
                }
                let mut journal = codebot_core::new_change_journal_path()?;
                if let Some(dir) = journal_dir {
                    if !dir.is_absolute() {
                        return Err("変更記録の保存先は絶対パスで指定してください".into());
                    }
                    journal = dir.join(journal.file_name().unwrap());
                }
                println!("変更記録：{}", journal.display());
                if !confirm("apply", cancel)? {
                    ToolResult {
                        status: ToolStatus::Rejected,
                        summary: "利用者が変更案を拒否しました。適用していません。".into(),
                    }
                } else {
                    match codebot_core::apply_changes(&prepared,&journal) {
                            Ok(count)=>ToolResult {status:ToolStatus::Succeeded,summary:format!("利用者が確認した変更案を{count}ファイルへ適用しました。変更記録から取り消し可能です。")},
                            Err(error)=>ToolResult {status:ToolStatus::Failed,summary:error},
                        }
                }
            }
            Err(error) => ToolResult {
                status: ToolStatus::Failed,
                summary: error,
            },
        },
    })
}
