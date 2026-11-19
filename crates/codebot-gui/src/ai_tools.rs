use codebot_core::{
    AgentReply, ApiConfig, ApiProtocol, Backend, ChatMessage, Config, ConnectorConfig,
    ConnectorTool, ToolAction, ToolCall, ToolConversation, ToolResult, ToolStatus,
};
use eframe::egui::{self, RichText};
use serde_json::Value;
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::JoinHandle,
};
struct Task {
    rx: Option<mpsc::Receiver<(ToolConversation, Result<AgentReply, String>)>>,
    cancel: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}
impl Drop for Task {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.rx = None;
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
const DONE: &str = "回答が完了しました。新しく相談できます。";

/// Everything needed to open the conversation once connector tools are listed.
struct Start {
    config: Config,
    backend: Backend,
    api: ApiConfig,
    key: String,
    messages: Vec<ChatMessage>,
}
/// Background work owned by the panel: listing connector tools before the
/// conversation starts, or running one confirmed connector tool call.
struct Job<T> {
    rx: mpsc::Receiver<T>,
    cancel: Arc<AtomicBool>,
}
impl<T> Drop for Job<T> {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
fn spawn<T: Send + 'static>(
    ctx: &egui::Context,
    work: impl FnOnce(&AtomicBool) -> T + Send + 'static,
) -> Job<T> {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancel.clone();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let _ = tx.send(work(&worker_cancel));
        ctx.request_repaint();
    });
    Job { rx, cancel }
}
fn ready<T>(job: &Option<Job<T>>) -> Option<Result<T, ()>> {
    job.as_ref().and_then(|job| match job.rx.try_recv() {
        Ok(value) => Some(Ok(value)),
        Err(mpsc::TryRecvError::Disconnected) => Some(Err(())),
        Err(mpsc::TryRecvError::Empty) => None,
    })
}

#[derive(Default)]
pub struct ToolPanel {
    skill_names: Vec<String>,
    connectors: Vec<ConnectorConfig>,
    start: Option<Start>,
    listing: Option<Job<Result<Vec<ConnectorTool>, String>>>,
    connector_call: Option<(ConnectorConfig, ConnectorTool, Value)>,
    running: Option<Job<ToolResult>>,
    root: String,
    prompt: String,
    destination: String,
    conversation: Option<ToolConversation>,
    task: Option<Task>,
    replies: Vec<String>,
    call: Option<ToolCall>,
    result: Option<ToolResult>,
    confirm_result: bool,
    command: crate::command_panel::CommandPanel,
    changes: crate::change_panel::ChangePanel,
    kind: u8,
    message: Option<String>,
    markdown: egui_commonmark::CommonMarkCache,
    /// The conversation ended (final answer, error or stop); read by `take_finished`.
    finished: bool,
}
/// How a chat-started tool conversation ended.
pub struct Finished {
    /// The model's text replies, in order.
    pub replies: Vec<String>,
    /// Why it stopped early, if it did.
    pub error: Option<String>,
}
impl ToolPanel {
    /// Start from the chat: `messages` already carry history and skills.
    #[allow(clippy::too_many_arguments)]
    pub fn start_chat(
        &mut self,
        ctx: &egui::Context,
        root: &str,
        messages: Vec<ChatMessage>,
        connectors: Vec<ConnectorConfig>,
        config: &Config,
        backend: Backend,
        api: &ApiConfig,
        key: &str,
    ) -> Result<(), String> {
        if self.is_busy() {
            return Err("ツールの操作を処理中です".into());
        }
        *self = Self::default();
        self.root = root.to_owned();
        self.prompt = messages
            .last()
            .map(|m| m.content.clone())
            .unwrap_or_default();
        self.connectors = connectors.clone();
        let start = Start {
            config: config.clone(),
            backend,
            api: api.clone(),
            key: key.to_owned(),
            messages,
        };
        if connectors.is_empty() {
            return self.open(ctx, start, &[]);
        }
        let root = std::path::PathBuf::from(root);
        self.start = Some(start);
        self.listing = Some(spawn(ctx, move |cancel| {
            let mut tools = Vec::new();
            for connector in &connectors {
                let listed = codebot_core::list_connector_tools(connector, Some(&root), cancel)
                    .map_err(|e| format!("コネクター「{}」：{e}", connector.name))?;
                tools.extend(listed);
            }
            Ok(tools)
        }));
        Ok(())
    }
    /// Once the conversation has ended and nothing is running, hand back its outcome.
    pub fn take_finished(&mut self) -> Option<Finished> {
        if !self.finished || self.is_busy() {
            return None;
        }
        self.finished = false;
        let error = self.message.take().filter(|m| m != DONE);
        Some(Finished {
            replies: std::mem::take(&mut self.replies),
            error,
        })
    }
    /// Stop waiting for the model and end the conversation.
    pub fn end(&mut self) {
        if let Some(task) = &self.task {
            task.cancel.store(true, Ordering::Relaxed);
        }
        if let Some(job) = &self.listing {
            job.cancel.store(true, Ordering::Relaxed);
        }
        if self.task.is_none() && self.listing.is_none() && !self.is_busy() {
            self.message = Some("ツールの利用を終了しました。".into());
            self.finished = true;
        }
    }
    pub fn take_location(&mut self) -> Option<(String, String)> {
        if self.changes.take_settings_changed() {
            Some(self.changes.last_location().unwrap_or_default())
        } else {
            None
        }
    }
    pub fn is_busy(&self) -> bool {
        self.task.is_some()
            || self.listing.is_some()
            || self.running.is_some()
            || self.command.is_busy()
            || self.changes.is_busy()
    }
    pub fn shutdown(&mut self) {
        self.task = None;
        self.listing = None;
        self.running = None;
        self.command.shutdown();
    }
    pub fn set_initial(
        &mut self,
        root: Option<&str>,
        prompt: &str,
        skills: Vec<String>,
        connectors: Vec<ConnectorConfig>,
    ) {
        if self.conversation.is_none() && !self.is_busy() {
            self.root = root.unwrap_or("").into();
            self.prompt = prompt.into();
            self.skill_names = skills;
            self.connectors = connectors;
        }
    }
    /// Begin a consultation: add skills, list connector tools if any, then ask.
    fn begin(
        &mut self,
        ctx: &egui::Context,
        config: &Config,
        backend: Backend,
        api: &ApiConfig,
        key: &str,
    ) -> Result<(), String> {
        let skills = crate::library::resolve_skills(&self.skill_names, Some(&self.root))?;
        let preamble = codebot_core::skill_preamble(&skills);
        let mut config = config.clone();
        config.max_input_chars = config
            .max_input_chars
            .saturating_add(preamble.chars().count());
        let messages = vec![ChatMessage::user(format!("{preamble}{}", self.prompt))];
        let start = Start {
            config,
            backend,
            api: api.clone(),
            key: key.to_owned(),
            messages,
        };
        if self.connectors.is_empty() {
            return self.open(ctx, start, &[]);
        }
        let connectors = self.connectors.clone();
        let root = std::path::PathBuf::from(&self.root);
        self.start = Some(start);
        self.listing = Some(spawn(ctx, move |cancel| {
            let mut tools = Vec::new();
            for connector in &connectors {
                let listed = codebot_core::list_connector_tools(connector, Some(&root), cancel)
                    .map_err(|e| format!("コネクター「{}」：{e}", connector.name))?;
                tools.extend(listed);
            }
            Ok(tools)
        }));
        self.message = None;
        Ok(())
    }
    fn open(
        &mut self,
        ctx: &egui::Context,
        start: Start,
        tools: &[ConnectorTool],
    ) -> Result<(), String> {
        let conversation = ToolConversation::with_connectors(
            &start.config,
            start.backend,
            &start.api,
            Some(&start.key),
            &start.messages,
            Path::new(&self.root),
            tools,
        )?;
        self.root = conversation.root().to_string_lossy().into_owned();
        self.destination = format!(
            "{} / {} / {}",
            start.api.resolved(start.backend).base_url,
            start.api.model,
            start.api.resolved(start.backend).protocol.label()
        );
        self.request(ctx, conversation);
        Ok(())
    }
    fn request(&mut self, ctx: &egui::Context, conversation: ToolConversation) {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let ctx = ctx.clone();
        let join = std::thread::spawn(move || {
            let mut conversation = conversation;
            let result = conversation.request(&worker_cancel);
            let _ = tx.send((conversation, result));
            ctx.request_repaint();
        });
        self.task = Some(Task {
            rx: Some(rx),
            cancel,
            join: Some(join),
        });
        self.message = None;
        self.call = None;
        self.result = None;
        self.connector_call = None;
        self.confirm_result = false;
    }
    pub fn poll(&mut self, ctx: &egui::Context) -> Vec<String> {
        let roots = [self.command.poll(), self.changes.poll()]
            .into_iter()
            .flatten()
            .collect();
        if let Some(listed) = ready(&self.listing) {
            let cancelled = self
                .listing
                .take()
                .is_some_and(|job| job.cancel.load(Ordering::Relaxed));
            let start = self.start.take();
            match (listed, start) {
                _ if cancelled => {
                    self.message =
                        Some("コネクターへの接続を中止しました。AIには送信していません。".into());
                    self.finished = true;
                }
                (Ok(Ok(tools)), Some(start)) => {
                    if let Err(error) = self.open(ctx, start, &tools) {
                        self.message = Some(error);
                        self.finished = true;
                    }
                }
                (Ok(Err(error)), _) => {
                    self.message = Some(error);
                    self.finished = true;
                }
                _ => {
                    self.message = Some("コネクターへの接続が中断しました".into());
                    self.finished = true;
                }
            }
        }
        if let Some(done) = ready(&self.running) {
            let cancelled = self
                .running
                .take()
                .is_some_and(|job| job.cancel.load(Ordering::Relaxed));
            self.result = Some(match done {
                _ if cancelled => ToolResult {
                    status: ToolStatus::Cancelled,
                    summary: "利用者がコネクターツールの実行を停止しました。".into(),
                },
                Ok(result) => result,
                Err(()) => ToolResult {
                    status: ToolStatus::Failed,
                    summary: "コネクターツールの実行が中断しました。".into(),
                },
            });
            self.confirm_result = false;
        }
        let ready = self
            .task
            .as_ref()
            .and_then(|task| task.rx.as_ref())
            .and_then(|rx| match rx.try_recv() {
                Ok(value) => Some(Ok(value)),
                Err(mpsc::TryRecvError::Disconnected) => Some(Err("ツール相談が中断しました")),
                _ => None,
            });
        if let Some(ready) = ready {
            let task = self.task.take().unwrap();
            let cancelled = task.cancel.load(Ordering::Relaxed);
            drop(task);
            match ready {
                Ok((conversation, result)) => {
                    self.conversation = Some(conversation);
                    match result {
                        _ if cancelled => {
                            self.message =
                                Some("ツール相談を中止しました。提案は実行していません。".into());
                            self.finished = true;
                        }
                        Ok(reply) => {
                            let text = reply.text();
                            if !text.is_empty() {
                                self.replies.push(text);
                            }
                            let connector = reply.call().and_then(|call| {
                                self.conversation
                                    .as_ref()
                                    .and_then(|c| c.connector_call(call))
                            });
                            if let (Some(call), Some(found)) = (reply.call(), connector) {
                                self.call = Some(call.clone());
                                self.kind = 3;
                                match found {
                                    Ok((tool, arguments)) => {
                                        match self
                                            .connectors
                                            .iter()
                                            .find(|c| c.name == tool.connector)
                                        {
                                            Some(config) => {
                                                self.connector_call =
                                                    Some((config.clone(), tool, arguments))
                                            }
                                            None => {
                                                self.message =
                                                    Some("コネクターの設定が見つかりません".into())
                                            }
                                        }
                                    }
                                    Err(error) => self.message = Some(error),
                                }
                            } else if let Some(call) = reply.call() {
                                self.call = Some(call.clone());
                                match call.action(Path::new(&self.root)) {
                                    Ok(ToolAction::Command(spec)) => {
                                        self.kind = 1;
                                        if let Err(error) = self.command.prepare_tool(ctx, &spec) {
                                            self.message = Some(error);
                                        }
                                    }
                                    Ok(ToolAction::Changes(proposal)) => {
                                        self.kind = 2;
                                        match self.changes.set_proposal(
                                            Some(&self.root),
                                            &serde_json::to_string(&proposal).unwrap(),
                                        ) {
                                            Ok(()) => {
                                                self.changes.lock_root();
                                                self.changes.preview(ctx)
                                            }
                                            Err(error) => self.message = Some(error),
                                        }
                                    }
                                    Err(error) => self.message = Some(error),
                                }
                            } else {
                                self.message = Some(DONE.into());
                                self.finished = true;
                            }
                        }
                        Err(error) => {
                            self.message = Some(error);
                            self.finished = true;
                        }
                    }
                }
                Err(error) => {
                    self.message = Some(error.into());
                    self.finished = true;
                }
            }
        }
        if self.call.is_some() && self.result.is_none() {
            self.result = match self.kind {
                1 => self.command.tool_result(),
                2 => self.changes.tool_result(),
                _ => None,
            };
        }
        roots
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        backend: Backend,
        api: &ApiConfig,
        key: &str,
        config: &Config,
        can_mutate: bool,
    ) {
        ui.heading("AIに作業を相談");
        ui.add(egui::Label::new("変更案・コマンド・コネクターツールの提案を受け取ります。操作ごとに内容を確認し、結果をAIへ送る前にも確認します。最大4操作・8回の通信で終了します。").wrap());
        if !self.skill_names.is_empty() || !self.connectors.is_empty() {
            ui.horizontal_wrapped(|ui| {
                if !self.skill_names.is_empty() {
                    ui.label(
                        RichText::new(format!("スキル：{}", self.skill_names.join("、")))
                            .size(13.0),
                    );
                }
                if !self.connectors.is_empty() {
                    ui.label(
                        RichText::new(format!(
                            "コネクター：{}",
                            self.connectors
                                .iter()
                                .map(|c| c.name.as_str())
                                .collect::<Vec<_>>()
                                .join("、")
                        ))
                        .size(13.0),
                    );
                }
            });
        }
        self.context_ui(ui, backend, api, config);
        let busy = self.is_busy();
        if self.conversation.is_none() && self.task.is_none() {
            ui.add_enabled_ui(!busy, |ui| {
                ui.label("作業フォルダ");
                ui.add(crate::text_field(&mut self.root).desired_width(f32::INFINITY));
                ui.label("相談する内容");
                ui.add(
                    crate::text_area(&mut self.prompt)
                        .desired_rows(4)
                        .desired_width(f32::INFINITY),
                );
                ui.add(
                    egui::Label::new(format!(
                        "送信先：{} / {} / {}",
                        api.resolved(backend).base_url,
                        api.model,
                        api.resolved(backend).protocol.label()
                    ))
                    .wrap(),
                );
                if ui.button("内容をAIへ送って相談").clicked() {
                    if let Err(error) = self.begin(ui.ctx(), config, backend, api, key) {
                        self.message = Some(error);
                    }
                }
            });
        } else {
            ui.add(egui::Label::new(format!("作業フォルダ：{}", self.root)).wrap());
            ui.collapsing("最初の相談", |ui| {
                ui.add(egui::Label::new(&self.prompt).wrap());
            });
        }
        if !self.destination.is_empty() {
            ui.add(egui::Label::new(format!("この相談の送信先：{}", self.destination)).wrap());
        }
        self.progress_ui(ui, can_mutate);
        if ui
            .add_enabled(
                !busy && self.conversation.is_some(),
                egui::Button::new("相談を終了して新しく始める"),
            )
            .clicked()
        {
            self.conversation = None;
            self.call = None;
            self.connector_call = None;
            self.result = None;
            self.replies.clear();
            self.destination.clear();
            self.confirm_result = false;
            self.message = None;
            self.markdown = Default::default();
        }
        if let Some(message) = &self.message {
            ui.add(egui::Label::new(message).wrap());
        }
    }
    /// Replies, the pending proposal, the result confirmation and stop controls.
    pub fn progress_ui(&mut self, ui: &mut egui::Ui, can_mutate: bool) {
        let busy = self.is_busy();
        for (index, text) in self.replies.iter().enumerate() {
            let _ = crate::markdown::show(ui, &mut self.markdown, (usize::MAX - 2, index), text);
        }
        if let Some(call) = &self.call {
            ui.separator();
            ui.label(match self.kind {
                1 => "提案：コマンドの実行",
                2 => "提案：ファイルの変更",
                _ => "提案：コネクターツールの実行",
            });
            ui.collapsing("要求の詳細", |ui| {
                ui.add(egui::Label::new(serde_json::to_string_pretty(call).unwrap()).wrap());
            });
            if self.result.is_none() {
                match self.kind {
                    1 => {
                        let _ = self.command.ui(ui, can_mutate);
                    }
                    2 => self.changes.ui(ui, can_mutate),
                    _ => self.connector_ui(ui, can_mutate),
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("この提案を拒否"))
                    .clicked()
                {
                    self.result = Some(ToolResult {
                        status: ToolStatus::Rejected,
                        summary: "利用者がこの提案を拒否しました。実行していません。".into(),
                    });
                    self.confirm_result = false;
                }
            }
        }
        if let Some(result) = &self.result {
            ui.separator();
            ui.label("AIへ送る結果");
            egui::ScrollArea::vertical()
                .id_salt("tool-result")
                .min_scrolled_height(80.)
                .max_height(180.)
                .show(ui, |ui| {
                    ui.add(egui::Label::new(&result.summary).wrap());
                });
            ui.add_enabled(
                !busy,
                crate::checkbox::Checkbox::new(
                    &mut self.confirm_result,
                    "この結果を上記のAIへ送ることを確認しました",
                ),
            );
            if ui
                .add_enabled(
                    !busy && self.confirm_result,
                    egui::Button::new("確認した結果を送って続ける"),
                )
                .clicked()
            {
                if let (Some(mut conversation), Some(call)) = (self.conversation.take(), &self.call)
                {
                    match conversation.resolve(&call.id, result) {
                        Ok(()) => self.request(ui.ctx(), conversation),
                        Err(error) => {
                            self.message = Some(error);
                            self.conversation = Some(conversation);
                        }
                    }
                }
            }
        }
        if let Some(job) = &self.listing {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("コネクターに接続してツールを確認しています…");
                if ui.button("中止").clicked() {
                    job.cancel.store(true, Ordering::Relaxed);
                }
            });
        }
        if let Some(task) = &self.task {
            ui.label("AIへ問い合わせ中…");
            if ui.button("問い合わせを停止").clicked() {
                task.cancel.store(true, Ordering::Relaxed);
            }
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    /// Inline view under the chat messages.
    pub fn chat_ui(&mut self, ui: &mut egui::Ui, can_mutate: bool) {
        egui::Frame::new()
            .stroke(egui::Stroke::new(1.0_f32, crate::BORDER))
            .corner_radius(12.0)
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("ツールを使って作業中")
                            .size(14.0)
                            .family(egui::FontFamily::Name("ui-bold".into())),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("ツールの利用を終える").clicked() {
                            self.end();
                        }
                    });
                });
                ui.label(
                    RichText::new("操作ごとに内容を確認し、結果をAIへ送る前にも確認します。最大4操作・8回の通信です。")
                        .size(12.0)
                        .color(crate::MUTED),
                );
                self.progress_ui(ui, can_mutate);
                if let Some(message) = self.message.as_ref().filter(|m| m.as_str() != DONE) {
                    ui.add(egui::Label::new(message).wrap());
                }
            });
    }
    fn connector_ui(&mut self, ui: &mut egui::Ui, can_mutate: bool) {
        let Some((config, tool, arguments)) = &self.connector_call else {
            return;
        };
        ui.add(
            egui::Label::new(format!(
                "コネクター「{}」のツール「{}」を実行します。",
                config.name, tool.name
            ))
            .wrap(),
        );
        if !tool.description.is_empty() {
            ui.add(
                egui::Label::new(
                    RichText::new(&tool.description)
                        .size(13.0)
                        .color(crate::MUTED),
                )
                .wrap(),
            );
        }
        ui.label(RichText::new("引数").size(13.0));
        egui::ScrollArea::vertical()
            .id_salt("connector-arguments")
            .max_height(160.0)
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(serde_json::to_string_pretty(arguments).unwrap_or_default())
                            .monospace(),
                    )
                    .wrap(),
                );
            });
        let mut line = config.command.clone();
        for arg in &config.args {
            line.push(' ');
            line.push_str(arg);
        }
        ui.add(
            egui::Label::new(
                RichText::new(format!(
                    "起動するプログラム：{line}\n作業場所：{}　追加の環境変数：{}　タイムアウト：{}秒",
                    self.root,
                    if config.env_names.is_empty() { "なし".into() } else { config.env_names.join(", ") },
                    config.timeout_seconds
                ))
                .size(12.0)
                .color(crate::MUTED),
            )
            .wrap(),
        );
        if let Some(job) = &self.running {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("実行中…");
                if ui.button("停止").clicked() {
                    job.cancel.store(true, Ordering::Relaxed);
                }
            });
        } else if ui
            .add_enabled(can_mutate, egui::Button::new("このツールを実行"))
            .clicked()
        {
            let (config, tool, arguments) = (config.clone(), tool.clone(), arguments.clone());
            let root = std::path::PathBuf::from(&self.root);
            self.running =
                Some(spawn(
                    ui.ctx(),
                    move |cancel| match codebot_core::call_connector_tool(
                        &config,
                        Some(&root),
                        &tool.name,
                        &arguments,
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
                        Err(error) => ToolResult {
                            status: ToolStatus::Failed,
                            summary: error,
                        },
                    },
                ));
        }
    }
    /// Context breakdown for this consultation.
    fn context_ui(
        &mut self,
        ui: &mut egui::Ui,
        backend: Backend,
        api: &ApiConfig,
        config: &Config,
    ) {
        use crate::context_bar::{self, Segment};
        let budget = config.max_context_chars;
        let segments = if let Some(conversation) = &self.conversation {
            let tools = codebot_core::tool_definitions_chars(
                conversation.protocol(),
                &conversation.connector_tools(),
            );
            vec![
                Segment::new("ツール定義", tools, context_bar::TOOLS),
                Segment::new(
                    "相談の内容・回答・結果",
                    conversation.request_chars().saturating_sub(tools),
                    context_bar::HISTORY,
                ),
            ]
        } else {
            let protocol: ApiProtocol = api.resolved(backend).protocol;
            let skills = crate::library::resolve_skills(&self.skill_names, Some(&self.root))
                .unwrap_or_default();
            let tools = Segment::new(
                "ツール定義",
                codebot_core::tool_definitions_chars(protocol, &[]),
                context_bar::TOOLS,
            );
            vec![
                Segment::new(
                    "スキル",
                    codebot_core::skill_preamble(&skills).chars().count(),
                    context_bar::SKILLS,
                ),
                tools,
                Segment::new("相談内容", self.prompt.chars().count(), context_bar::INPUT),
            ]
        };
        egui::Frame::new()
            .stroke(egui::Stroke::new(1.0_f32, crate::BORDER))
            .corner_radius(12.0)
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                context_bar::detail(ui, "コンテキストの内訳", &segments, budget);
                if self.conversation.is_none() && !self.connectors.is_empty() {
                    ui.label(
                        RichText::new(
                            "コネクターのツール定義は、接続して一覧を取得した後に加わります。",
                        )
                        .size(11.0)
                        .color(crate::MUTED),
                    );
                }
            });
    }
}
