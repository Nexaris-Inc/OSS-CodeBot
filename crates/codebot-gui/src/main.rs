use codebot_core::{
    stream_messages, validate_messages, ApiConfig, ApiProtocol, Backend, ChatMessage, Config,
    StreamEnd, StreamEvent,
};
use eframe::egui::{self, Color32, RichText};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
};
mod ai_tools;
mod api_diagnostics;
mod change_panel;
mod chat_storage;
mod checkbox;
mod theme;
pub(crate) use theme::*;
mod command_panel;
mod context_bar;
mod git_panel;
mod github_panel;
mod library;
mod local_diagnostics;
mod local_models;
mod markdown;
mod model_cache;
mod path_picker;
mod plugins;
mod ui_preferences;
mod workspace_panel;
use local_models::LocalModels;
use ui_preferences::{apply_font, FontChoice, UiPreferences};

// Material Design 3 color roles (light scheme generated around the #005FB8 brand primary).
/// Shape scale: full (pill) buttons, large panes, extra-large composer.
const SHAPE_FULL: u8 = 255;
const SHAPE_LARGE: f32 = 16.0;
const SHAPE_EXTRA_LARGE: f32 = 28.0;
/// Loading indicators appear only when startup work takes longer than this.
const LOADING_DELAY: std::time::Duration = std::time::Duration::from_millis(300);
/// Cross-fade from the loading state to the restored content.
const REVEAL_MS: f32 = 140.0;
const MEASURE: f32 = 680.0;
const COMPOSER_WIDTH: f32 = 740.0;

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Session {
    label: String,
    messages: Vec<(bool, String)>,
    draft: String,
    attachments: Vec<codebot_core::Attachment>,
    analysis_action: Option<codebot_core::AnalysisAction>,
    workspace_root: Option<String>,
    history: Vec<ChatMessage>,
    #[serde(skip)]
    history_enabled: bool,
    context_key: Option<String>,
    #[serde(skip)]
    in_flight_input: Option<String>,
    #[serde(skip)]
    retry_input: Option<String>,
    #[serde(skip)]
    notice: Option<String>,
    #[serde(skip)]
    stream_index: Option<usize>,
    generation_incomplete: bool,
    /// Skill names attached to this chat.
    skills: Vec<String>,
    /// Connector names attached to this chat.
    connectors: Vec<String>,
    /// Let the model propose changes, commands and connector calls in this chat.
    use_tools: bool,
}
impl Session {
    fn request_messages(
        &mut self,
        config: &Config,
        destination: &str,
        rules: bool,
    ) -> Result<Vec<ChatMessage>, String> {
        let changed = self
            .context_key
            .as_deref()
            .is_some_and(|key| key != destination);
        if changed && self.history_enabled && !self.history.is_empty() && !rules {
            self.history_enabled = false;
            return Err("送信先またはモデルが変わりました。新しい送信先へ過去の会話を送る場合は「会話履歴を含める」を再度有効にしてください。下書きは保持しています。".into());
        }
        let mut messages = if self.history_enabled && !rules {
            self.history.clone()
        } else {
            vec![]
        };
        let input = self
            .analysis_action
            .map(|action| format!("{}\n\n利用者の依頼:\n{}", action.prompt(), self.draft))
            .unwrap_or_else(|| self.draft.clone());
        messages.push(ChatMessage::user(codebot_core::compose_input(
            &input,
            &self.attachments,
        )?));
        validate_messages(config, &messages)?;
        if changed && !self.history_enabled {
            self.history.clear();
        }
        self.context_key = Some(destination.to_owned());
        Ok(messages)
    }
    fn title(&self) -> String {
        if !self.label.trim().is_empty() {
            return self.label.trim().to_owned();
        }
        self.messages
            .first()
            .map(|(_, s)| {
                let first = s.trim().lines().next().unwrap_or(s).trim();
                let title: String = first.chars().take(24).collect();
                if first.chars().count() > 24 {
                    format!("{title}…")
                } else {
                    title
                }
            })
            .unwrap_or_else(|| "新しいチャット".into())
    }
    fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        query.is_empty()
            || self.title().to_lowercase().contains(&query)
            || self
                .messages
                .iter()
                .any(|(_, text)| text.to_lowercase().contains(&query))
    }
}
struct App {
    sessions: Vec<Session>,
    active: usize,
    config_path: String,
    backend: Backend,
    api_configs: BTreeMap<Backend, ApiConfig>,
    api_key: String,
    api_diagnostics: api_diagnostics::ApiDiagnostics,
    local: LocalModels,
    font: FontChoice,
    preferences_path: Option<PathBuf>,
    preferences_error: Option<String>,
    settings: bool,
    sidebar: bool,
    copied: Option<(usize, usize)>,
    markdown_cache: egui_commonmark::CommonMarkCache,
    pending: Option<(usize, mpsc::Receiver<StreamEvent>)>,
    request_cancel: Option<Arc<AtomicBool>>,
    archive_last_saved: std::time::Instant,
    save_chats: bool,
    sessions_dirty: bool,
    archive_blocked: bool,
    storage_error: Option<String>,
    search: String,
    delete_confirm: bool,
    export_path: String,
    export_overwrite: bool,
    export_message: Option<String>,
    archive_path_override: String,
    archive_path_edit: String,
    workspace_panel: workspace_panel::WorkspacePanel,
    files_open: bool,
    change_panel: change_panel::ChangePanel,
    changes_open: bool,
    commands_open: bool,
    git_open: bool,
    github_open: bool,
    tools_open: bool,
    plugins_open: bool,
    library_open: bool,
    library_panel: library::LibraryPanel,
    connectors: Vec<codebot_core::ConnectorConfig>,
    /// (config path, max_context_chars, max_input_chars) for the context meter.
    budget_cache: Option<(String, usize, usize)>,
    /// Workspace files mentioned in the active draft: (root + draft key, mentions).
    mention_cache: Option<(String, Vec<codebot_core::PathMention>)>,
    /// Inline diff previews of proposals in answers, by (session, message).
    diff_cache: BTreeMap<(usize, usize), DiffPreview>,
    /// A tool conversation started from a chat: (session index, engine).
    chat_tool: Option<(usize, Box<ai_tools::ToolPanel>)>,
    /// Saved chats being read at startup, and when reading began.
    startup_load: Option<StartupLoad>,
    /// Start of the cross-fade after a visible loading state.
    reveal: Option<std::time::Instant>,
    /// Stop the skeleton pulse, progress bar motion and cross-fade.
    reduce_motion: bool,
    theme: theme::ThemeChoice,
    plugin_panel: plugins::PluginPanel,
    tool_panel: ai_tools::ToolPanel,
    github_panel: github_panel::GithubPanel,
    git_panel: git_panel::GitPanel,
    command_panel: command_panel::CommandPanel,
}
impl Drop for App {
    fn drop(&mut self) {
        self.stop_generation();
        self.command_panel.shutdown();
        self.git_panel.shutdown();
        self.github_panel.shutdown();
        self.tool_panel.shutdown();
        self.plugin_panel.shutdown();
        self.library_panel.shutdown();
        if let Some((_, panel)) = &mut self.chat_tool {
            panel.shutdown();
        }
        self.pending = None;
        self.flush_chats();
    }
}
impl Default for App {
    fn default() -> Self {
        Self {
            sessions: vec![Session::default()],
            active: 0,
            config_path: String::new(),
            backend: Backend::Rules,
            api_configs: BTreeMap::new(),
            api_key: String::new(),
            api_diagnostics: api_diagnostics::ApiDiagnostics::default(),
            local: LocalModels::default(),
            font: FontChoice::default(),
            preferences_path: None,
            preferences_error: None,
            settings: false,
            sidebar: true,
            copied: None,
            markdown_cache: egui_commonmark::CommonMarkCache::default(),
            pending: None,
            request_cancel: None,
            archive_last_saved: std::time::Instant::now(),
            save_chats: false,
            sessions_dirty: false,
            archive_blocked: false,
            storage_error: None,
            search: String::new(),
            delete_confirm: false,
            export_path: String::new(),
            export_overwrite: false,
            export_message: None,
            archive_path_override: String::new(),
            archive_path_edit: String::new(),
            workspace_panel: workspace_panel::WorkspacePanel::default(),
            files_open: false,
            change_panel: change_panel::ChangePanel::default(),
            changes_open: false,
            commands_open: false,
            git_open: false,
            github_open: false,
            tools_open: false,
            plugins_open: false,
            library_open: false,
            library_panel: library::LibraryPanel::default(),
            connectors: Vec::new(),
            budget_cache: None,
            mention_cache: None,
            diff_cache: BTreeMap::new(),
            chat_tool: None,
            startup_load: None,
            reveal: None,
            reduce_motion: false,
            theme: theme::ThemeChoice::default(),
            plugin_panel: plugins::PluginPanel::default(),
            tool_panel: ai_tools::ToolPanel::default(),
            github_panel: github_panel::GithubPanel::default(),
            git_panel: git_panel::GitPanel::default(),
            command_panel: command_panel::CommandPanel::default(),
        }
    }
}
impl App {
    fn archive_path(&self) -> Option<PathBuf> {
        if !self.archive_path_override.trim().is_empty() {
            let path = PathBuf::from(self.archive_path_override.trim());
            return path.is_absolute().then_some(path);
        }
        self.preferences_path
            .as_ref()
            .map(|path| path.with_file_name("chats.json"))
    }
    fn restore_chats(&mut self, merge: bool) {
        let result = load_archive(self.archive_path());
        self.apply_restored(result, merge);
    }
    /// Read saved chats on a background thread at startup. Saving and the chat
    /// UI stay blocked until the result arrives, so the archive is never
    /// overwritten with the empty startup session.
    fn start_restore(&mut self, ctx: &egui::Context) {
        let path = self.archive_path();
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(load_archive(path));
            ctx.request_repaint();
        });
        self.archive_blocked = true;
        self.startup_load = Some((rx, std::time::Instant::now()));
    }
    /// Apply the startup load when it is ready. Returns true while still loading.
    fn poll_restore(&mut self) -> bool {
        let Some((rx, since)) = &self.startup_load else {
            return false;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return true,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("チャットの読み込みが中断しました。元ファイルは保持しています".into())
            }
        };
        // Cross-fade only when the loading state was actually visible.
        if since.elapsed() >= LOADING_DELAY && !self.reduce_motion {
            self.reveal = Some(std::time::Instant::now());
        }
        self.startup_load = None;
        self.apply_restored(result, false);
        false
    }
    fn apply_restored(
        &mut self,
        result: Result<Option<chat_storage::Archive>, String>,
        merge: bool,
    ) {
        self.diff_cache.clear();
        match result {
            Ok(Some(mut archive)) => {
                for session in &mut archive.sessions {
                    if session.generation_incomplete {
                        if session.messages.last().is_none_or(|(user, _)| *user) {
                            session.messages.push((false, String::new()));
                        }
                        session.messages.last_mut().unwrap().1.push_str("\n\n---\n生成中にアプリが終了しました。途中の回答は会話履歴へ送信しません。");
                        session.generation_incomplete = false;
                    }
                }
                if merge
                    && self.sessions.iter().any(|s| {
                        !s.messages.is_empty() || !s.draft.is_empty() || !s.label.is_empty()
                    })
                {
                    let old_active = self.active;
                    for (index, session) in self.sessions.drain(..).enumerate() {
                        let position = archive.sessions.iter().position(|saved| {
                            saved.label == session.label
                                && saved.messages == session.messages
                                && saved.draft == session.draft
                                && saved.history == session.history
                                && saved.context_key == session.context_key
                                && saved.attachments == session.attachments
                                && saved.analysis_action == session.analysis_action
                                && saved.workspace_root == session.workspace_root
                        });
                        let position = match position {
                            Some(position) => position,
                            None => {
                                archive.sessions.push(session);
                                archive.sessions.len() - 1
                            }
                        };
                        if index == old_active {
                            self.active = position;
                        }
                    }
                } else {
                    self.active = archive.active;
                }
                self.sessions = archive.sessions;
                self.archive_blocked = false;
                self.storage_error = None;
                self.sessions_dirty = merge;
            }
            Ok(None) => {
                self.archive_blocked = false;
                self.storage_error = None;
                self.sessions_dirty = true;
            }
            Err(error) => {
                self.archive_blocked = true;
                self.storage_error = Some(error);
            }
        }
    }
    fn flush_chats(&mut self) {
        if !self.save_chats || !self.sessions_dirty || self.archive_blocked {
            return;
        }
        if self.pending.is_some()
            && self.archive_last_saved.elapsed() < std::time::Duration::from_millis(500)
        {
            return;
        }
        self.archive_last_saved = std::time::Instant::now();
        self.sessions_dirty = false;
        self.storage_error = self
            .archive_path()
            .ok_or_else(|| "チャット保存先を取得できません".to_owned())
            .and_then(|path| chat_storage::save(&path, &self.sessions, self.active))
            .err();
    }
    fn delete_active_chat(&mut self) {
        if self.pending.is_some() {
            return;
        }
        if self
            .chat_tool
            .as_ref()
            .is_some_and(|(_, panel)| panel.is_busy())
        {
            return;
        }
        // Message keys shift when a chat is removed.
        self.diff_cache.clear();
        match &mut self.chat_tool {
            Some((index, _)) if *index == self.active => self.chat_tool = None,
            Some((index, _)) if *index > self.active => *index -= 1,
            _ => {}
        }
        self.sessions.remove(self.active);
        if self.sessions.is_empty() {
            self.sessions.push(Session::default());
        }
        self.active = self.active.min(self.sessions.len() - 1);
        self.delete_confirm = false;
        self.copied = None;
        self.sessions_dirty = true;
    }
    fn chat_settings(&mut self, ui: &mut egui::Ui) {
        ui.label("保存ファイルのフルパス（空欄はOS標準のchats.json）");
        ui.add(crate::text_field(&mut self.archive_path_edit));
        if ui
            .add_enabled(self.pending.is_none(), egui::Button::new("保存先を適用"))
            .clicked()
        {
            if self.archive_path_edit.trim().is_empty()
                || Path::new(self.archive_path_edit.trim()).is_absolute()
            {
                self.archive_path_override = self.archive_path_edit.trim().to_owned();
                self.save_preferences();
                if self.save_chats {
                    self.restore_chats(true);
                }
            } else {
                self.storage_error = Some("保存先には絶対パスを指定してください".into());
            }
        }
        if ui
            .add_enabled(
                self.pending.is_none(),
                crate::checkbox::Checkbox::new(
                    &mut self.save_chats,
                    "チャットと下書きをこのPCに保存する",
                ),
            )
            .changed()
        {
            self.save_preferences();
            if self.save_chats {
                self.restore_chats(true);
            }
        }
        ui.label("保存は任意です。無効にしても既存の保存ファイルは保持します。復元した会話の送信には再度チェックが必要です。");
        if let Some(path) = self.archive_path() {
            ui.label(format!("保存先: {}", path.display()));
        }
        if let Some(error) = &self.storage_error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if ui
            .add_enabled(
                self.pending.is_none(),
                egui::Button::new("保存・再読み込みを再試行"),
            )
            .clicked()
        {
            if self.archive_blocked {
                self.restore_chats(true);
            } else {
                self.sessions_dirty = true;
                self.flush_chats();
            }
        }
        ui.add_space(8.0);
        ui.label("このチャットの名前");
        if ui
            .add(crate::text_field(&mut self.sessions[self.active].label))
            .changed()
        {
            self.sessions_dirty = true;
        }
        if ui
            .add_enabled(
                self.pending.is_none(),
                egui::Button::new("このチャットを削除…"),
            )
            .clicked()
        {
            self.delete_confirm = true;
        }
        if self.delete_confirm {
            ui.label(format!(
                "「{}」を削除しますか？",
                self.sessions[self.active].title()
            ));
            ui.horizontal(|ui| {
                if ui.button("削除する").clicked() {
                    self.delete_active_chat();
                }
                if ui.button("キャンセル").clicked() {
                    self.delete_confirm = false;
                }
            });
        }
        ui.add_space(8.0);
        ui.label("書き出し先のファイルパス");
        ui.add(crate::text_field(&mut self.export_path));
        ui.add(crate::checkbox::Checkbox::new(
            &mut self.export_overwrite,
            "書き出し先の既存ファイルの上書きを許可する",
        ));
        ui.horizontal(|ui| {
            for (label, markdown) in [("Markdownで書き出す", true), ("JSONで書き出す", false)]
            {
                if ui
                    .add_enabled(
                        !self.export_path.trim().is_empty(),
                        egui::Button::new(label),
                    )
                    .clicked()
                {
                    let path = Path::new(self.export_path.trim());
                    // Exports must never replace the live archive or application settings.
                    let protected = [self.archive_path(), self.preferences_path.clone()]
                        .into_iter()
                        .flatten()
                        .any(|p| {
                            p == path
                                || p.canonicalize()
                                    .ok()
                                    .zip(path.canonicalize().ok())
                                    .is_some_and(|(a, b)| a == b)
                        });
                    let result = if protected {
                        Err(
                            "アプリの保存・設定ファイルを上書きする書き出し先は指定できません"
                                .into(),
                        )
                    } else {
                        chat_storage::export(
                            path,
                            &self.sessions[self.active],
                            markdown,
                            self.export_overwrite,
                        )
                    };
                    self.export_message = Some(match result {
                        Ok(()) => "チャットを書き出しました".into(),
                        Err(error) => error,
                    });
                }
            }
        });
        if let Some(message) = &self.export_message {
            ui.label(message);
        }
    }
    fn load(ctx: &egui::Context) -> Self {
        let mut app = Self::default();
        app.preferences_path =
            codebot_core::default_config_path().map(|p| p.with_file_name("gui-preferences.toml"));
        if let Some(path) = &app.preferences_path {
            match UiPreferences::load(path) {
                Ok(preferences) => {
                    app.font = preferences.font;
                    app.backend = preferences.backend;
                    app.api_configs = preferences.api_configs;
                    app.local.config = preferences.local;
                    app.github_panel.config = preferences.github;
                    app.connectors = preferences.connectors;
                    app.reduce_motion = preferences.reduce_motion;
                    app.theme = preferences.theme;
                    app.save_chats = preferences.save_chats;
                    app.archive_path_override = preferences.chat_archive_path;
                    app.archive_path_edit = app.archive_path_override.clone();
                    app.change_panel.restore_location(
                        preferences.last_change_root,
                        preferences.last_change_journal,
                    );
                }
                Err(e) => app.preferences_error = Some(e),
            }
        }
        if app.save_chats {
            app.start_restore(ctx);
        }
        apply_font(ctx, app.font);
        app
    }
    fn save_font_choice(&mut self, ctx: &egui::Context) {
        apply_font(ctx, self.font);
        self.save_preferences();
    }
    fn save_preferences(&mut self) {
        let (last_change_root, last_change_journal) =
            self.change_panel.last_location().unwrap_or_default();
        self.preferences_error = match &self.preferences_path {
            Some(path) => UiPreferences {
                font: self.font,
                backend: self.backend,
                api_configs: self.api_configs.clone(),
                local: self.local.config.clone(),
                save_chats: self.save_chats,
                chat_archive_path: self.archive_path_override.clone(),
                last_change_root,
                last_change_journal,
                github: self.github_panel.config.clone(),
                connectors: self.connectors.clone(),
                reduce_motion: self.reduce_motion,
                theme: self.theme,
            }
            .save(path)
            .err(),
            None => Some("設定の保存先を取得できません。この起動中のみ反映されます。".into()),
        };
    }
    /// Startup loading state: the shell's surfaces at once, and after
    /// `LOADING_DELAY` a skeleton of the chat layout with a progress bar.
    fn loading_ui(&mut self, ctx: &egui::Context) {
        let elapsed = self
            .startup_load
            .as_ref()
            .map_or(std::time::Duration::ZERO, |(_, since)| since.elapsed());
        let visible = elapsed >= LOADING_DELAY;
        if !visible {
            ctx.request_repaint_after(LOADING_DELAY - elapsed);
        } else if self.reduce_motion {
            // The background thread also requests a repaint when it finishes.
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        } else {
            ctx.request_repaint();
        }
        let seconds = elapsed.as_secs_f32();
        // Skeleton blocks breathe between 55% and 100% every 0.9 s.
        let alpha = if self.reduce_motion {
            1.0
        } else {
            0.775 + 0.225 * (seconds * std::f32::consts::TAU / 0.9).cos()
        };
        let block = SKELETON().gamma_multiply(alpha);
        let narrow = ctx.screen_rect().width() < 900.0;
        if self.sidebar && !narrow {
            egui::SidePanel::left("sidebar")
                .exact_width(224.0)
                .resizable(false)
                .show_separator_line(false)
                .frame(egui::Frame::new().fill(SHELL()).inner_margin(egui::Margin {
                    left: 12,
                    right: 12,
                    top: 16,
                    bottom: 16,
                }))
                .show(ctx, |ui| {
                    if !visible {
                        return;
                    }
                    let rect = ui.max_rect();
                    let p = ui.painter();
                    let mut y = rect.top() + 6.0;
                    let mut bar = |x: f32, w: f32, h: f32, r: f32, gap: f32| {
                        p.rect_filled(
                            egui::Rect::from_min_size(
                                egui::pos2(rect.left() + x, y),
                                egui::vec2(w, h),
                            ),
                            r,
                            block,
                        );
                        y += h + gap;
                    };
                    bar(8.0, 96.0, 22.0, 6.0, 26.0); // brand
                    bar(0.0, 160.0, 52.0, SHAPE_LARGE, 24.0); // new chat
                    bar(0.0, rect.width(), 40.0, 20.0, 28.0); // search
                    for width in [rect.width(), 150.0, 170.0] {
                        bar(0.0, width, 36.0, 18.0, 8.0); // chats
                    }
                });
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(SHELL()).inner_margin(8.0))
            .show(ctx, |ui| {
                let area = ui.max_rect();
                ui.painter().rect_filled(area, SHAPE_LARGE, SURFACE());
                if !visible {
                    return;
                }
                // Keep the band inside the pane's rounded corners.
                let p = ui
                    .painter()
                    .with_clip_rect(area.shrink2(egui::vec2(SHAPE_LARGE, 0.0)));
                // Progress: a 3px band along the pane's top edge.
                let track = egui::Rect::from_min_size(area.min, egui::vec2(area.width(), 3.0));
                if self.reduce_motion {
                    p.rect_filled(track, 0.0, SELECTED());
                } else {
                    let phase = (seconds / 1.1).fract();
                    let width = area.width() * 0.35;
                    let left = area.left() - width + phase * (area.width() + width);
                    p.rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(left, area.top()),
                            egui::vec2(width, 3.0),
                        ),
                        1.5,
                        ACCENT(),
                    );
                }
                let inner = area.shrink(20.0);
                let composer_width = (inner.width() - 32.0).clamp(200.0, COMPOSER_WIDTH);
                let left = inner.center().x - composer_width / 2.0;
                let top = inner.top() + 56.0 + ((inner.height() - 56.0) * 0.27).max(12.0);
                let shapes = [
                    (
                        egui::pos2(inner.left(), inner.top() + 10.0),
                        egui::vec2(140.0, 16.0),
                        6.0,
                    ),
                    (egui::pos2(left, top), egui::vec2(220.0, 28.0), 8.0),
                    (egui::pos2(left, top + 44.0), egui::vec2(300.0, 14.0), 6.0),
                    (
                        egui::pos2(left, top + 86.0),
                        egui::vec2(composer_width, 72.0),
                        SHAPE_EXTRA_LARGE,
                    ),
                ];
                for (min, size, radius) in shapes {
                    p.rect_filled(egui::Rect::from_min_size(min, size), radius, block);
                }
                // Text for assistive technology and for the reduced-motion case.
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                        egui::pos2(left, top + 172.0),
                        egui::vec2(composer_width, 24.0),
                    )),
                    |ui| {
                        ui.label(
                            RichText::new("保存したチャットを読み込んでいます…")
                                .size(12.0)
                                .color(MUTED()),
                        );
                    },
                );
            });
    }
    /// Close every full-page view and return to the chat.
    fn close_pages(&mut self) {
        self.settings = false;
        self.files_open = false;
        self.changes_open = false;
        self.commands_open = false;
        self.git_open = false;
        self.github_open = false;
        self.tools_open = false;
        self.plugins_open = false;
        self.library_open = false;
    }
    fn new_chat(&mut self) {
        self.sessions.push(Session::default());
        self.active = self.sessions.len() - 1;
        self.settings = false;
        self.files_open = false;
        self.changes_open = false;
        self.commands_open = false;
        self.git_open = false;
        self.github_open = false;
        self.tools_open = false;
        self.plugins_open = false;
        self.library_open = false;
        self.delete_confirm = false;
        self.sessions_dirty = true;
    }
    fn send(&mut self, ctx: &egui::Context) {
        if self.change_panel.is_busy()
            || self.command_panel.is_busy()
            || self.git_panel.is_busy()
            || self.github_panel.is_busy()
            || self.tool_panel.is_busy()
            || self.plugin_panel.is_busy()
        {
            self.sessions[self.active].notice =
                Some("実行中の処理が終わってから送信してください。下書きは保持しています。".into());
            return;
        }
        if self.pending.is_some() || self.sessions[self.active].draft.trim().is_empty() {
            return;
        }
        let local_connection = if self.backend == Backend::Local {
            match self.local.connection() {
                Ok(c) => Some(c),
                Err(e) => {
                    self.local.message = Some(e);
                    self.settings = true;
                    return;
                }
            }
        } else {
            None
        };
        let path = self.config_path.trim();
        let config = match Config::load(if path.is_empty() {
            None
        } else {
            Some(Path::new(path))
        }) {
            Ok(c) => c,
            Err(e) => {
                self.sessions[self.active].notice = Some(e);
                return;
            }
        };
        let destination = self.destination_key();
        let mut messages = match self.sessions[self.active].request_messages(
            &config,
            &destination,
            self.backend == Backend::Rules,
        ) {
            Ok(messages) => messages,
            Err(e) => {
                self.sessions[self.active].notice = Some(e);
                return;
            }
        };
        // The history keeps the user's own input; skill instructions are added to
        // this request only.
        let request_input = messages.last().unwrap().content.clone();
        if self.backend != Backend::Rules {
            if let Err(e) = apply_skills(&self.sessions[self.active], &config, &mut messages) {
                self.sessions[self.active].notice = Some(e);
                return;
            }
        }
        let tool_mode = self.tools_available() && self.sessions[self.active].use_tools;
        if tool_mode {
            if let Err(e) = self.start_chat_tool(ctx, &config, messages.clone()) {
                self.sessions[self.active].notice = Some(e);
                return;
            }
        }
        self.sessions[self.active].notice = None;
        let input = std::mem::take(&mut self.sessions[self.active].draft);
        self.sessions[self.active].in_flight_input = Some(request_input);
        self.sessions_dirty = true;
        let attachments = std::mem::take(&mut self.sessions[self.active].attachments);
        self.sessions[self.active].analysis_action = None;
        let input = if attachments.is_empty() {
            input
        } else {
            format!(
                "{}\n\n添付: {}",
                input,
                attachments
                    .iter()
                    .map(|a| format!("{}（{}–{}行）", a.path, a.first_line, a.last_line))
                    .collect::<Vec<_>>()
                    .join("、")
            )
        };
        self.sessions[self.active]
            .messages
            .push((true, input.clone()));
        if tool_mode {
            return;
        }
        let backend = self.backend;
        let api = self
            .api_configs
            .get(&backend)
            .cloned()
            .unwrap_or_else(|| backend.defaults());
        let api_key = self.api_key.clone();
        let (tx, rx) = mpsc::sync_channel(64);
        let cancel = Arc::new(AtomicBool::new(false));
        self.request_cancel = Some(cancel.clone());
        self.sessions[self.active].stream_index = None;
        self.sessions[self.active].generation_incomplete = true;
        self.pending = Some((self.active, rx));
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let emit = |event| {
                if tx.send(event).is_err() {
                    cancel.store(true, Ordering::Relaxed);
                }
                ctx.request_repaint();
            };
            if let Some(connection) = local_connection {
                connection.stream_messages(&config, &messages, &cancel, emit);
            } else {
                stream_messages(
                    &config,
                    backend,
                    &api,
                    Some(&api_key),
                    &messages,
                    &cancel,
                    emit,
                );
            }
        });
    }
    fn stop_generation(&mut self) {
        if let Some(cancel) = &self.request_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }
    fn poll(&mut self) {
        for _ in 0..256 {
            let Some((id, event)) =
                self.pending
                    .as_ref()
                    .and_then(|(id, rx)| match rx.try_recv() {
                        Ok(event) => Some((*id, event)),
                        Err(mpsc::TryRecvError::Disconnected) => Some((
                            *id,
                            StreamEvent::Finished(StreamEnd::Failed("処理が中断されました".into())),
                        )),
                        Err(mpsc::TryRecvError::Empty) => None,
                    })
            else {
                break;
            };
            let stopped = self
                .request_cancel
                .as_ref()
                .is_some_and(|c| c.load(Ordering::Relaxed));
            let session = &mut self.sessions[id];
            match event {
                StreamEvent::Delta(text) if !stopped => {
                    let index = *session.stream_index.get_or_insert_with(|| {
                        session.messages.push((false, String::new()));
                        session.messages.len() - 1
                    });
                    session.messages[index].1.push_str(&text);
                }
                StreamEvent::Delta(_) => continue,
                StreamEvent::Finished(end) => {
                    session.generation_incomplete = false;
                    let end = if stopped { StreamEnd::Cancelled } else { end };
                    let index = session.stream_index.take().unwrap_or_else(|| {
                        session.messages.push((false, String::new()));
                        session.messages.len() - 1
                    });
                    match end {
                        StreamEnd::Complete => {
                            session.retry_input = None;
                            if let Some(input) = session.in_flight_input.take() {
                                session.history.extend([
                                    ChatMessage::user(input),
                                    ChatMessage::assistant(session.messages[index].1.clone()),
                                ]);
                            }
                            eprintln!("INFO request completed");
                        }
                        status => {
                            session.retry_input = session.in_flight_input.take();
                            let notice = match status {
                                StreamEnd::Cancelled => {
                                    "生成を停止しました。途中の回答は会話履歴へ送信しません。"
                                        .into()
                                }
                                StreamEnd::Failed(error) => format!(
                                    "{error}\n設定と入力を確認して、もう一度送信してください。"
                                ),
                                _ => unreachable!(),
                            };
                            session.messages[index]
                                .1
                                .push_str(&format!("\n\n---\n{notice}"));
                            eprintln!("INFO request ended without a complete answer");
                        }
                    }
                    self.pending = None;
                    self.request_cancel = None;
                }
            }
            self.sessions_dirty = true;
        }
    }
    fn destination_key(&self) -> String {
        if self.backend == Backend::Local {
            return format!(
                "local:{:?}:{}",
                self.local.config.engine, self.local.config.model_path
            );
        }
        let api = self
            .api_configs
            .get(&self.backend)
            .cloned()
            .unwrap_or_else(|| self.backend.defaults())
            .resolved(self.backend);
        format!(
            "{}:{:?}:{}:{}",
            self.backend.id(),
            api.protocol,
            api.base_url,
            api.model
        )
    }
    /// Configured character budgets: (max_context_chars, max_input_chars).
    fn budgets(&mut self) -> (usize, usize) {
        let path = self.config_path.trim().to_owned();
        if self.budget_cache.as_ref().map(|(p, _, _)| p) != Some(&path) {
            let config = Config::load(if path.is_empty() {
                None
            } else {
                Some(Path::new(&path))
            })
            .unwrap_or_default();
            self.budget_cache = Some((path, config.max_context_chars, config.max_input_chars));
        }
        let (_, context, input) = self.budget_cache.as_ref().unwrap();
        (*context, *input)
    }
    /// What the next chat request would contain, by category.
    fn context_segments(&mut self) -> (Vec<context_bar::Segment>, usize) {
        let (budget, _) = self.budgets();
        let rules = self.backend == Backend::Rules;
        let workspace = self.sessions[self.active].workspace_root.clone();
        self.library_panel.ensure_loaded(workspace.as_deref());
        let session = &self.sessions[self.active];
        let skills: Vec<_> = self
            .library_panel
            .skills
            .iter()
            .filter(|s| session.skills.contains(&s.name))
            .cloned()
            .collect();
        let skill_chars = codebot_core::skill_preamble(&skills).chars().count();
        let history_chars: usize = session
            .history
            .iter()
            .map(|m| m.content.chars().count())
            .sum();
        let input = session
            .analysis_action
            .map(|action| format!("{}\n\n利用者の依頼:\n{}", action.prompt(), session.draft))
            .unwrap_or_else(|| session.draft.clone());
        let input_chars = input.trim().chars().count();
        let attachment_chars = codebot_core::compose_input(&input, &session.attachments)
            .map(|t| t.chars().count().saturating_sub(input_chars))
            .unwrap_or(0);
        let mut skill = context_bar::Segment::new("スキル", skill_chars, context_bar::SKILLS());
        if rules && skill_chars > 0 {
            skill = skill.note("ローカル診断では未使用");
        }
        let mut history =
            context_bar::Segment::new("会話履歴", history_chars, context_bar::HISTORY());
        if rules {
            history = history.note("ローカル診断では未使用");
        } else if !session.history_enabled {
            history = history.note("履歴送信オフ");
        }
        let mut segments = vec![skill, history];
        if session.use_tools && !matches!(self.backend, Backend::Rules | Backend::Local) {
            let api = self
                .api_configs
                .get(&self.backend)
                .cloned()
                .unwrap_or_else(|| self.backend.defaults());
            let tools = context_bar::Segment::new(
                "ツール定義",
                codebot_core::tool_definitions_chars(api.resolved(self.backend).protocol, &[]),
                context_bar::TOOLS(),
            );
            segments.push(if session.connectors.is_empty() {
                tools
            } else {
                tools.note("コネクター分は接続後に追加")
            });
        }
        (
            vec![
                segments.remove(0),
                segments.remove(0),
                context_bar::Segment::new(
                    "添付ファイル",
                    attachment_chars,
                    context_bar::ATTACHMENTS(),
                ),
                context_bar::Segment::new("入力", input_chars, context_bar::INPUT()),
            ]
            .into_iter()
            .chain(segments)
            .collect(),
            budget,
        )
    }
    /// Tools need an API destination (not local diagnostics or local AI).
    fn tools_available(&self) -> bool {
        !matches!(self.backend, Backend::Rules | Backend::Local)
    }
    fn start_chat_tool(
        &mut self,
        ctx: &egui::Context,
        config: &Config,
        messages: Vec<ChatMessage>,
    ) -> Result<(), String> {
        let session = &self.sessions[self.active];
        let root = session
            .workspace_root
            .clone()
            .ok_or("ツールを使うには作業フォルダを選んでください（ヘッダーの「ファイル」）")?;
        // The request was already validated with skills added; allow its size here.
        let mut limits = config.clone();
        limits.max_input_chars = limits
            .max_input_chars
            .max(messages.last().map_or(0, |m| m.content.chars().count()));
        let connectors = self
            .connectors
            .iter()
            .filter(|c| session.connectors.contains(&c.name))
            .cloned()
            .collect();
        let api = self
            .api_configs
            .get(&self.backend)
            .cloned()
            .unwrap_or_else(|| self.backend.defaults());
        let mut panel = Box::<ai_tools::ToolPanel>::default();
        panel.start_chat(
            ctx,
            &root,
            messages,
            connectors,
            &limits,
            self.backend,
            &api,
            &self.api_key,
        )?;
        self.chat_tool = Some((self.active, panel));
        Ok(())
    }
    /// Poll the chat tool conversation; write its outcome into the chat when it ends.
    fn poll_chat_tool(&mut self, ctx: &egui::Context) -> Vec<String> {
        let Some((index, panel)) = &mut self.chat_tool else {
            return Vec::new();
        };
        let roots = panel.poll(ctx);
        let index = *index;
        if let Some(done) = panel.take_finished() {
            self.chat_tool = None;
            if let Some(session) = self.sessions.get_mut(index) {
                let mut text = done.replies.join("\n\n");
                if let Some(error) = &done.error {
                    if !text.is_empty() {
                        text.push_str("\n\n---\n");
                    }
                    text.push_str(error);
                }
                if text.trim().is_empty() {
                    text = "（回答はありませんでした）".into();
                }
                match (done.error, session.in_flight_input.take()) {
                    (None, Some(input)) => {
                        session.retry_input = None;
                        session.history.extend([
                            ChatMessage::user(input),
                            ChatMessage::assistant(text.clone()),
                        ]);
                    }
                    (Some(_), input) => session.retry_input = input,
                    _ => {}
                }
                session.messages.push((false, text));
                self.sessions_dirty = true;
            }
        }
        roots
    }
    /// Files from the chat's workspace that the draft mentions and that are not attached.
    fn mentioned_files(&mut self) -> Vec<codebot_core::PathMention> {
        let session = &self.sessions[self.active];
        let Some(root) = session.workspace_root.clone() else {
            return Vec::new();
        };
        let key = format!("{root}\0{}", session.draft);
        if self.mention_cache.as_ref().map(|(k, _)| k) != Some(&key) {
            let mentions = if session.draft.contains(['/', '\\', '.']) {
                codebot_core::Workspace::open(Path::new(&root))
                    .and_then(|w| w.mentions(&session.draft, 4))
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            self.mention_cache = Some((key, mentions));
        }
        let attached: Vec<&str> = session
            .attachments
            .iter()
            .map(|a| a.path.as_str())
            .collect();
        self.mention_cache
            .as_ref()
            .map(|(_, m)| {
                m.iter()
                    .filter(|m| !attached.contains(&m.path.as_str()))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
    /// Suggest attaching files the draft mentions, e.g. from a pasted stack trace.
    fn file_suggestions(&mut self, ui: &mut egui::Ui) {
        let mentions = self.mentioned_files();
        if mentions.is_empty() {
            return;
        }
        let mut open = None;
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new("入力にあるファイル：")
                    .size(12.0)
                    .color(MUTED()),
            );
            for mention in &mentions {
                let label = match mention.line {
                    Some(line) => format!("{}:{line}", mention.path),
                    None => mention.path.clone(),
                };
                if chip(ui, "開く", &label, ATTACHMENTS(), false)
                    .on_hover_text("参照ファイル画面で前後の行をプレビューし、添付できます")
                    .clicked()
                {
                    open = Some(mention.clone());
                }
            }
        });
        if let Some(mention) = open {
            let root = self.sessions[self.active]
                .workspace_root
                .clone()
                .unwrap_or_default();
            self.workspace_panel
                .open_file(&root, &mention.path, mention.line);
            self.close_pages();
            self.files_open = true;
        }
        ui.add_space(4.0);
    }
    /// (used, budget) when the next request uses 80% or more of the budget.
    fn context_warning(&mut self) -> Option<(usize, usize)> {
        if self.backend == Backend::Rules {
            return None;
        }
        let (segments, budget) = self.context_segments();
        let used = context_bar::used(&segments);
        (used * 10 >= budget * 8).then_some((used, budget))
    }
    /// Chips for skills and connectors attached to this chat.
    fn attached_chips(&mut self, ui: &mut egui::Ui) {
        let session = &mut self.sessions[self.active];
        if session.skills.is_empty() && session.connectors.is_empty() {
            return;
        }
        let mut remove_skill = None;
        let mut remove_connector = None;
        ui.horizontal_wrapped(|ui| {
            for name in &session.skills {
                if chip(ui, "スキル", name, context_bar::SKILLS(), true)
                    .on_hover_text("送信時にスキルの指示を依頼の前に付けます")
                    .clicked()
                {
                    remove_skill = Some(name.clone());
                }
            }
            for name in &session.connectors {
                if chip(ui, "コネクター", name, context_bar::TOOLS(), true)
                    .on_hover_text("「実行 → AIに作業を相談」でツールをAIに提示します")
                    .clicked()
                {
                    remove_connector = Some(name.clone());
                }
            }
        });
        if let Some(name) = remove_skill {
            session.skills.retain(|n| *n != name);
            self.sessions_dirty = true;
        }
        if let Some(name) = remove_connector {
            session.connectors.retain(|n| *n != name);
            self.sessions_dirty = true;
        }
        ui.add_space(4.0);
    }
    /// Popup from the composer's add button: toggle library items for this chat.
    fn attach_menu(&mut self, ui: &mut egui::Ui, button: &egui::Response) {
        let id = ui.make_persistent_id(("attach-menu", self.active));
        if button.clicked() {
            let workspace = self.sessions[self.active].workspace_root.clone();
            self.library_panel.reload(workspace.as_deref());
            ui.memory_mut(|m| m.toggle_popup(id));
        }
        let above = !self.sessions[self.active].messages.is_empty();
        let mut open_library = false;
        egui::popup::popup_above_or_below_widget(
            ui,
            id,
            button,
            if above {
                egui::AboveOrBelow::Above
            } else {
                egui::AboveOrBelow::Below
            },
            egui::PopupCloseBehavior::CloseOnClickOutside,
            |ui| {
                ui.set_min_width(260.0);
                let session = &mut self.sessions[self.active];
                ui.label(RichText::new("スキル").size(12.0).color(MUTED()));
                if self.library_panel.skills.is_empty() {
                    ui.label(
                        RichText::new("ライブラリにスキルがありません")
                            .size(13.0)
                            .color(MUTED()),
                    );
                }
                for skill in &self.library_panel.skills {
                    let mut on = session.skills.contains(&skill.name);
                    let response = ui
                        .add(checkbox::Checkbox::new(&mut on, &skill.name))
                        .on_hover_text(&skill.description);
                    if response.changed() {
                        if on && session.skills.len() < codebot_core::MAX_ATTACHED_SKILLS {
                            session.skills.push(skill.name.clone());
                        } else {
                            session.skills.retain(|n| n != &skill.name);
                        }
                        self.sessions_dirty = true;
                    }
                }
                ui.separator();
                ui.label(RichText::new("コネクター").size(12.0).color(MUTED()));
                if self.connectors.is_empty() {
                    ui.label(
                        RichText::new("登録済みのコネクターがありません")
                            .size(13.0)
                            .color(MUTED()),
                    );
                }
                for connector in &self.connectors {
                    let mut on = session.connectors.contains(&connector.name);
                    if ui
                        .add(checkbox::Checkbox::new(&mut on, &connector.name))
                        .changed()
                    {
                        if on && session.connectors.len() < codebot_core::MAX_ATTACHED_CONNECTORS {
                            session.connectors.push(connector.name.clone());
                        } else {
                            session.connectors.retain(|n| n != &connector.name);
                        }
                        self.sessions_dirty = true;
                    }
                }
                ui.separator();
                if ui.button("ライブラリを管理…").clicked() {
                    open_library = true;
                }
            },
        );
        if open_library {
            ui.memory_mut(|m| m.close_popup());
            self.close_pages();
            self.library_open = true;
        }
    }
    fn composer(&mut self, ui: &mut egui::Ui) {
        let mut remove = None;
        for (index, attachment) in self.sessions[self.active].attachments.iter().enumerate() {
            // Material input chip.
            egui::Frame::new()
                .stroke(egui::Stroke::new(1.0_f32, BORDER()))
                .corner_radius(8.0)
                .inner_margin(egui::Margin {
                    left: 12,
                    right: 4,
                    top: 2,
                    bottom: 2,
                })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "添付: {}（{}–{}行）",
                                attachment.path, attachment.first_line, attachment.last_line
                            ))
                            .size(14.0)
                            .color(MUTED()),
                        );
                        if ui.small_button("外す").clicked() {
                            remove = Some(index);
                        }
                    });
                });
        }
        if let Some(index) = remove {
            self.sessions[self.active].attachments.remove(index);
            self.sessions_dirty = true;
        }
        self.attached_chips(ui);
        self.file_suggestions(ui);
        let enabled = self.pending.is_none()
            && !self.change_panel.is_busy()
            && !self.command_panel.is_busy()
            && !self.git_panel.is_busy()
            && !self.github_panel.is_busy()
            && !self.tool_panel.is_busy()
            && !self.plugin_panel.is_busy()
            && self.chat_tool.is_none()
            && !self.sessions[self.active].draft.trim().is_empty();
        let input_height = 2.0 * ui.text_style_height(&egui::TextStyle::Body);
        egui::Frame::new()
            .fill(RAISED())
            .corner_radius(SHAPE_EXTRA_LARGE)
            .inner_margin(egui::Margin::symmetric(18, 16))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let add = icon_button(ui, Icon::New, "スキル・コネクターを追加", true, false);
                    self.attach_menu(ui, &add);
                    let text_width =
                        (ui.available_width() - 40.0 - ui.spacing().item_spacing.x).max(100.0);
                    ui.allocate_ui_with_layout(
                        egui::vec2(text_width, input_height),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt(("composer-scroll", self.active))
                                .max_height(input_height)
                                .min_scrolled_height(input_height)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    let response = ui.add(
                                        crate::text_area(&mut self.sessions[self.active].draft)
                                            .id_salt(("draft", self.active))
                                            .hint_text(
                                                RichText::new("メッセージを入力").color(MUTED()),
                                            )
                                            .font(egui::TextStyle::Body)
                                            .desired_width(f32::INFINITY)
                                            .desired_rows(2)
                                            .margin(egui::Vec2::ZERO)
                                            .frame(false),
                                    );
                                    response.widget_info(|| {
                                        egui::WidgetInfo::labeled(
                                            egui::WidgetType::TextEdit,
                                            true,
                                            "メッセージ",
                                        )
                                    });
                                    if response.changed() {
                                        self.sessions_dirty = true;
                                    }
                                });
                        },
                    );
                    if self.pending.is_some() {
                        if icon_button(ui, Icon::Stop, "生成を停止", true, true).clicked() {
                            self.stop_generation();
                        }
                    } else if icon_button(ui, Icon::Send, "送信", enabled, true).clicked() {
                        self.send(ui.ctx());
                    }
                });
            });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let notice = if self.pending.is_some() {
                if self
                    .request_cancel
                    .as_ref()
                    .is_some_and(|c| c.load(Ordering::Relaxed))
                {
                    "生成を停止しています…"
                } else {
                    "生成中…"
                }
                .to_string()
            } else if self.backend == Backend::Rules {
                "ローカル診断。入力内容は外部に送信されません。".into()
            } else if self.backend == Backend::Local {
                "ローカルAI。このPC上で推論します。".into()
            } else {
                format!(
                    "{}へ{}を送信します。",
                    self.backend.label(),
                    if self.sessions[self.active].history_enabled {
                        "入力と会話履歴"
                    } else {
                        "今回の入力だけ"
                    }
                )
            };
            ui.label(RichText::new(notice).size(12.0).color(MUTED()));
            let (segments, budget) = self.context_segments();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.available_width() > 220.0 {
                    ui.label(
                        RichText::new(if cfg!(target_os = "macos") {
                            "⌘ Enter"
                        } else {
                            "Ctrl + Enter"
                        })
                        .size(11.0)
                        .color(MUTED()),
                    );
                    ui.add_space(8.0);
                }
                if ui.available_width() > 110.0 {
                    context_bar::compact(ui, &segments, budget);
                }
            });
        });
        if let Some((used, budget)) = self.context_warning() {
            let pending = self.pending.is_some();
            let session = &mut self.sessions[self.active];
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(if used > budget {
                        "送信できる量（max_context_chars）を超えています。"
                    } else {
                        "コンテキストが上限の80%を超えました。"
                    })
                    .size(12.0)
                    .color(if used > budget { ERROR() } else { MUTED() }),
                );
                if session.history_enabled && session.history.len() >= 2 {
                    let others = used.saturating_sub(history_chars(&session.history));
                    let allowed = (budget * 6 / 10).saturating_sub(others);
                    if ui
                        .add_enabled(!pending, egui::Button::new("古い履歴を外す"))
                        .on_hover_text("送信用の履歴から古い往復を外し、使用量を60%以下にします。画面の会話は残ります。")
                        .clicked()
                    {
                        let removed = trim_history(&mut session.history, allowed);
                        session.notice = Some(format!(
                            "送信用の履歴から古い{removed}往復を外しました。画面の会話は残っています。"
                        ));
                        self.sessions_dirty = true;
                    }
                } else if !session.attachments.is_empty() {
                    ui.label(RichText::new("添付する行の範囲を狭めると減らせます。").size(12.0).color(MUTED()));
                }
            });
        }
        if self.backend != Backend::Rules {
            let destination = self.destination_key();
            ui.horizontal(|ui| {
                let session = &mut self.sessions[self.active];
                if ui
                    .add_enabled(
                        self.pending.is_none(),
                        crate::checkbox::Checkbox::new(
                            &mut session.history_enabled,
                            "会話履歴を含める",
                        ),
                    )
                    .changed()
                    && session.history_enabled
                {
                    session.context_key = Some(destination);
                    session.notice = None;
                }
                if !matches!(self.backend, Backend::Local)
                    && ui
                        .add_enabled(
                            self.pending.is_none() && self.chat_tool.is_none(),
                            crate::checkbox::Checkbox::new(&mut session.use_tools, "ツールを使う"),
                        )
                        .on_hover_text("変更案・コマンド・追加したコネクターのツールをAIが提案します。実行前と結果送信前に確認します。作業フォルダが必要です。")
                        .changed()
                {
                    self.sessions_dirty = true;
                }
                if ui
                    .add_enabled(
                        self.pending.is_none(),
                        egui::Button::new("送信用の履歴を消去"),
                    )
                    .clicked()
                {
                    session.history.clear();
                    session.notice = Some(
                        "送信用の履歴を消去しました。画面の会話と下書きは保持しています。".into(),
                    );
                }
                ui.label(
                    RichText::new(format!(
                        "{}文字",
                        session
                            .history
                            .iter()
                            .map(|m| m.content.chars().count())
                            .sum::<usize>()
                    ))
                    .size(12.0)
                    .color(MUTED()),
                );
            });
        }
        if let Some(notice) = &self.sessions[self.active].notice {
            ui.label(RichText::new(notice).size(12.0).color(MUTED()));
        }
    }
    fn conversation(&mut self, ui: &mut egui::Ui) {
        let can_mutate = !self.change_panel.is_busy()
            && !self.command_panel.is_busy()
            && !self.git_panel.is_busy()
            && !self.github_panel.is_busy()
            && !self.tool_panel.is_busy()
            && !self.plugin_panel.is_busy();
        let mut reference = None;
        let mut retry = false;
        let mut proposal_open = None;
        egui::ScrollArea::vertical()
            .id_salt(("chat", self.active))
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                for (index, (user, message)) in
                    self.sessions[self.active].messages.iter().enumerate()
                {
                    if *user {
                        ui.add_space(18.0);
                        let width = (ui.available_width() * 0.88).min(560.0);
                        ui.horizontal(|ui| {
                            ui.add_space((ui.available_width() - width - 10.0).max(0.0));
                            egui::Frame::new()
                                .fill(PRIMARY_CONTAINER())
                                .corner_radius(egui::CornerRadius {
                                    nw: 20,
                                    ne: 20,
                                    sw: 20,
                                    se: 4,
                                })
                                .inner_margin(egui::Margin::symmetric(18, 14))
                                .show(ui, |ui| {
                                    ui.set_width((width - 36.0).max(50.0));
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(message)
                                                .line_height(Some(22.0))
                                                .color(ON_PRIMARY_CONTAINER()),
                                        )
                                        .wrap()
                                        .selectable(true),
                                    );
                                });
                        });
                    } else {
                        ui.add_space(30.0);
                        ui.label(
                            RichText::new("CodeBot")
                                .size(14.0)
                                .family(egui::FontFamily::Name("ui-bold".into())),
                        );
                        ui.add_space(10.0);
                        // The provider's mode is already visible beneath the composer.
                        let body = message
                            .strip_prefix("ルールベース診断（AI未使用）\n")
                            .unwrap_or(message);
                        if let Some(uri) =
                            markdown::show(ui, &mut self.markdown_cache, (self.active, index), body)
                        {
                            reference = Some(uri);
                        }
                        if body.contains("\"changes\"") && body.contains("\"version\"") {
                            let key = (self.active, index);
                            let root = self.sessions[self.active].workspace_root.clone();
                            egui::CollapsingHeader::new("変更案の差分を表示")
                                .id_salt(("inline-diff", key))
                                .show(ui, |ui| {
                                    let Some(root) = root else {
                                        ui.label(RichText::new("作業フォルダを選ぶと差分を表示できます（「ファイル」で選択）。").size(13.0).color(MUTED()));
                                        return;
                                    };
                                    let result = self.diff_cache.entry(key).or_insert_with(|| {
                                        codebot_core::ChangeProposal::parse(body).and_then(|proposal| {
                                            codebot_core::prepare_changes(Path::new(&root), &proposal)
                                                .map(|prepared| prepared.previews())
                                        })
                                    });
                                    match result {
                                        Ok(previews) => {
                                            for preview in previews.iter() {
                                                ui.label(RichText::new(&preview.path).monospace().size(13.0));
                                                egui::ScrollArea::both()
                                                    .id_salt(("inline-diff-body", key, &preview.path))
                                                    .max_height(280.0)
                                                    .show(ui, |ui| {
                                                        ui.add(egui::Label::new(change_panel::diff_job(&preview.diff)).extend());
                                                    });
                                            }
                                        }
                                        Err(error) => {
                                            ui.colored_label(ERROR(), error.as_str());
                                        }
                                    }
                                    if ui.small_button("ファイルの現状で再計算").clicked() {
                                        self.diff_cache.remove(&key);
                                    }
                                });
                        }
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if icon_button(ui, Icon::Copy, "回答をコピー", true, false).clicked()
                            {
                                ui.ctx().copy_text(message.clone());
                                self.copied = Some((self.active, index));
                            }
                            if self.copied == Some((self.active, index)) {
                                ui.label(RichText::new("コピーしました").size(12.0).color(MUTED()));
                            }
                            if body.contains("\"changes\"")
                                && body.contains("\"version\"")
                                && ui.button("変更案を確認").clicked()
                            {
                                proposal_open = Some(body.to_owned());
                            }
                            if index + 1 == self.sessions[self.active].messages.len()
                                && self.sessions[self.active].retry_input.is_some()
                                && ui
                                    .add_enabled(
                                        self.pending.is_none(),
                                        egui::Button::new("再試行"),
                                    )
                                    .clicked()
                            {
                                retry = true;
                            }
                        });
                    }
                    ui.add_space(14.0);
                }
                if let Some((index, panel)) = &mut self.chat_tool {
                    if *index == self.active {
                        ui.add_space(16.0);
                        panel.chat_ui(ui, can_mutate);
                    }
                }
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|(id, _)| *id == self.active)
                {
                    ui.add_space(20.0);
                    ui.label(RichText::new("生成中…").color(MUTED()));
                }
            });
        if let Some(uri) = reference {
            if let Some(root) = &self.sessions[self.active].workspace_root {
                self.workspace_panel.open_location(root, &uri);
                self.files_open = true;
            } else {
                self.sessions[self.active].notice =
                    Some("参照リンクに対応する作業フォルダが選択されていません".into());
            }
        }
        if retry {
            let session = &mut self.sessions[self.active];
            if session.draft.trim().is_empty() && session.attachments.is_empty() {
                session.draft = session.retry_input.clone().unwrap_or_default();
                self.send(ui.ctx());
            } else {
                session.notice = Some(
                    "未送信の下書きがあります。送信または退避してから再試行してください。".into(),
                );
            }
        }
        if let Some(proposal) = proposal_open {
            match self.change_panel.set_proposal(
                self.sessions[self.active].workspace_root.as_deref(),
                &proposal,
            ) {
                Ok(()) => {
                    self.change_panel.preview(ui.ctx());
                    self.changes_open = true;
                    self.files_open = false;
                    self.settings = false;
                }
                Err(error) => self.sessions[self.active].notice = Some(error),
            }
        }
    }
    fn processing_settings(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new("処理方式")
                .size(14.0)
                .family(egui::FontFamily::Name("ui-bold".into())),
        );
        let previous = self.backend;
        ui.add_enabled_ui(self.pending.is_none(), |ui| {
            egui::ComboBox::from_id_salt("backend")
                .selected_text(self.backend.label())
                .height(400.0)
                .width(260.0)
                .show_ui(ui, |ui| {
                    for backend in Backend::ALL {
                        ui.selectable_value(&mut self.backend, backend, backend.label());
                    }
                });
        });
        if self.backend != previous {
            self.api_diagnostics.cancel();
            self.api_key.clear();
            if previous == Backend::Local {
                self.local.stop();
            }
            if !matches!(self.backend, Backend::Rules | Backend::Local) {
                self.api_configs
                    .entry(self.backend)
                    .or_insert_with(|| self.backend.defaults());
            }
            self.save_preferences();
        }
        if self.backend == Backend::Local {
            if self.local.ui(ui, self.pending.is_some()) {
                self.save_preferences();
            }
            if let Some(error) = &self.preferences_error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            return;
        }
        if self.backend == Backend::Rules {
            ui.label(
                RichText::new("APIキー不要。入力は外部に送信されません。")
                    .size(13.0)
                    .color(MUTED()),
            );
            return;
        }
        let api = self
            .api_configs
            .entry(self.backend)
            .or_insert_with(|| self.backend.defaults());
        ui.add_space(12.0);
        let model = ui.label("モデルID（必須）");
        ui.add(
            crate::text_field(&mut api.model)
                .desired_width(f32::INFINITY)
                .hint_text("契約先で利用可能なモデルID"),
        )
        .labelled_by(model.id);
        if matches!(self.backend, Backend::Custom | Backend::Openai) {
            ui.add_space(8.0);
            egui::ComboBox::from_id_salt("protocol")
                .selected_text(api.protocol.label())
                .width(260.0)
                .show_ui(ui, |ui| {
                    for protocol in [
                        ApiProtocol::Openai,
                        ApiProtocol::Responses,
                        ApiProtocol::Gemini,
                        ApiProtocol::Claude,
                    ] {
                        if self.backend == Backend::Openai
                            && !matches!(protocol, ApiProtocol::Openai | ApiProtocol::Responses)
                        {
                            continue;
                        }
                        ui.selectable_value(&mut api.protocol, protocol, protocol.label());
                    }
                });
        }
        ui.add_space(8.0);
        let endpoint = ui.label("APIベースURL（地域・契約に合わせて変更可能）");
        let edited = ui
            .add(crate::text_field(&mut api.base_url).desired_width(f32::INFINITY))
            .labelled_by(endpoint.id);
        if edited.changed() {
            self.api_key.clear();
        }
        ui.label(
            RichText::new(match api.protocol {
                ApiProtocol::Openai => "末尾に /chat/completions を自動で追加します。",
                ApiProtocol::Responses => {
                    "末尾に /responses を自動で追加します。store=falseで送信します。"
                }
                ApiProtocol::Claude => "末尾に /messages を自動で追加します。",
                ApiProtocol::Gemini => {
                    "末尾に /models/{モデルID}:generateContent を自動で追加します。"
                }
            })
            .size(12.0)
            .color(MUTED()),
        );
        ui.add_space(8.0);
        let key = ui.label("APIキー（保存されません）");
        ui.add(
            crate::text_field(&mut self.api_key)
                .password(true)
                .desired_width(f32::INFINITY)
                .hint_text("この起動中だけ使用。未入力なら環境変数を参照"),
        )
        .labelled_by(key.id);
        let env = ui.label("APIキーの環境変数名");
        ui.add(crate::text_field(&mut api.api_key_env).desired_width(f32::INFINITY))
            .labelled_by(env.id);
        if self.backend == Backend::Custom {
            ui.add(crate::checkbox::Checkbox::new(
                &mut api.anonymous,
                "キーなし接続を許可（ローカルAPIなど）",
            ));
        }
        ui.horizontal(|ui| {
            ui.label("出力トークン上限");
            ui.add(egui::DragValue::new(&mut api.max_output_tokens).range(1..=128_000));
            ui.label("タイムアウト（秒）");
            ui.add(egui::DragValue::new(&mut api.timeout_seconds).range(1..=300));
        });
        ui.add_space(8.0);
        ui.label(RichText::new("送信時は今回の入力と選択した添付を上記URLに送ります。履歴送信を有効にした場合は過去の会話も含みます。利用料金は各サービスに従います。").size(12.0).color(MUTED()));
        self.api_diagnostics
            .ui(ui, self.backend, api, &self.api_key, self.pending.is_none());
        if ui.button("API設定を保存").clicked() {
            self.save_preferences();
        }
        if let Some(error) = &self.preferences_error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
    }
    fn settings_page(&mut self, ui: &mut egui::Ui) {
        ui.add_space(32.0);
        ui.label(RichText::new("設定").size(26.0));
        ui.add_space(28.0);
        ui.collapsing("チャットの保存と管理", |ui| {
            self.chat_settings(ui)
        });
        ui.add_space(16.0);
        self.processing_settings(ui);
        ui.add_space(28.0);
        ui.label(
            RichText::new("フォント")
                .size(14.0)
                .family(egui::FontFamily::Name("ui-bold".into())),
        );
        let previous = self.font;
        ui.add(crate::checkbox::Radio::new(
            &mut self.font,
            FontChoice::LineSeedJp,
            "LINE Seed JP — Regular 400（標準）",
        ));
        ui.add(crate::checkbox::Radio::new(
            &mut self.font,
            FontChoice::BizUdpGothic,
            "BIZ UDPゴシック",
        ));
        if self.font != previous {
            self.save_font_choice(ui.ctx());
        }
        if let Some(error) = &self.preferences_error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        } else {
            ui.label(
                RichText::new("すぐに反映され、次回起動にも引き継がれます。")
                    .size(13.0)
                    .color(MUTED()),
            );
        }
        ui.add_space(28.0);
        ui.label(
            RichText::new("表示")
                .size(14.0)
                .family(egui::FontFamily::Name("ui-bold".into())),
        );
        let previous = self.theme;
        ui.horizontal_wrapped(|ui| {
            for (choice, label) in [
                (theme::ThemeChoice::Light, "ライト（標準）"),
                (theme::ThemeChoice::Dark, "ダーク"),
                (theme::ThemeChoice::System, "OSに合わせる"),
            ] {
                ui.add(crate::checkbox::Radio::new(&mut self.theme, choice, label));
            }
        });
        if self.theme != previous {
            theme::apply(ui.ctx(), theme::wants_dark(ui.ctx(), self.theme));
            self.save_preferences();
        }
        if ui
            .add(crate::checkbox::Checkbox::new(
                &mut self.reduce_motion,
                "動きを減らす",
            ))
            .changed()
        {
            self.save_preferences();
        }
        ui.label(
            RichText::new(
                "起動時の読み込み表示で、明滅・進捗バーの移動・切り替え時のフェードを止めます。",
            )
            .size(13.0)
            .color(MUTED()),
        );
        ui.add_space(28.0);
        let label = ui.label(
            RichText::new("設定ファイル")
                .size(14.0)
                .family(egui::FontFamily::Name("ui-bold".into())),
        );
        ui.label(
            RichText::new("使用するTOMLファイルのパスを指定します。")
                .size(13.0)
                .color(MUTED()),
        );
        ui.add_space(8.0);
        ui.add(
            crate::text_field(&mut self.config_path)
                .desired_width(f32::INFINITY)
                .hint_text("OS標準の設定を使用")
                .margin(egui::vec2(12.0, 10.0)),
        )
        .labelled_by(label.id);
        ui.add_space(8.0);
        ui.label(RichText::new("空欄の場合はOS標準の設定、または既定値を使います。\n変更は次の送信から適用されます。").size(13.0).color(MUTED()));
        ui.add_space(28.0);
        if ui.button("チャットに戻る").clicked() {
            self.settings = false;
        }
    }
}
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        // "OSに合わせる" follows the OS preference while the app runs.
        let dark = theme::wants_dark(ctx, self.theme);
        if dark != theme::is_dark() {
            theme::apply(ctx, dark);
        }
        if self.poll_restore() {
            self.loading_ui(ctx);
            return;
        }
        let reveal = match self.reveal {
            Some(start) => {
                let t = start.elapsed().as_secs_f32() * 1000.0 / REVEAL_MS;
                if t >= 1.0 {
                    self.reveal = None;
                    1.0
                } else {
                    ctx.request_repaint();
                    egui::emath::easing::cubic_out(t)
                }
            }
            None => 1.0,
        };
        self.poll();
        self.github_panel.poll();
        self.library_panel.poll();
        for root in [
            self.change_panel.poll(),
            self.command_panel.poll(),
            self.git_panel.poll(),
        ]
        .into_iter()
        .flatten()
        .chain(self.tool_panel.poll(ctx))
        .chain(self.plugin_panel.poll(ctx))
        .chain(self.poll_chat_tool(ctx))
        {
            for session in &mut self.sessions {
                if session.workspace_root.as_deref() == Some(&root) {
                    session.history.clear();
                    session.attachments.clear();
                    session.analysis_action = None;
                    session.history_enabled = false;
                    session.retry_input = None;
                    session.notice = Some("ファイル操作・コマンドを実行しました。参照ファイルを選び直してください。画面の会話と下書きは保持しています。".into());
                }
            }
            self.sessions_dirty = true;
        }
        if let Some((root, journal)) = self.tool_panel.take_location() {
            self.change_panel.remember_location(root, journal);
            self.save_preferences();
        }
        if let Some((root, journal)) = self.plugin_panel.take_location() {
            self.change_panel.remember_location(root, journal);
            self.save_preferences();
        }
        if self.local.poll() {
            self.save_preferences();
        }
        if self.settings
            && !ctx.memory(|m| m.any_popup_open())
            && ctx.input(|i| i.key_pressed(egui::Key::Escape))
        {
            self.settings = false;
        }
        // Consume the shortcut before TextEdit to avoid inserting a newline on submit.
        if !self.settings
            && !self.files_open
            && !self.changes_open
            && !self.commands_open
            && !self.git_open
            && !self.github_open
            && !self.tools_open
            && !self.plugins_open
            && !self.library_open
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter))
        {
            self.send(ctx);
        }
        let narrow = ctx.screen_rect().width() < 900.0;
        if self.sidebar && !narrow {
            egui::SidePanel::left("sidebar")
                .exact_width(224.0)
                .resizable(false)
                .show_separator_line(false)
                .frame(egui::Frame::new().fill(SHELL()).inner_margin(egui::Margin {
                    left: 12,
                    right: 12,
                    top: 16,
                    bottom: 16,
                }))
                .show(ctx, |ui| {
                    ui.multiply_opacity(reveal);
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new("CodeBot")
                                .size(22.0)
                                .family(egui::FontFamily::Name("ui-bold".into())),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if icon_button(ui, Icon::Panel, "サイドバーを閉じる", true, false)
                                .clicked()
                            {
                                self.sidebar = false;
                            }
                        });
                    });
                    ui.add_space(20.0);
                    if new_chat_button(ui).clicked() {
                        self.new_chat();
                    }
                    ui.add_space(24.0);
                    search_bar(ui, &mut self.search);
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        ui.label(RichText::new("チャット").size(14.0).color(MUTED()));
                    });
                    ui.add_space(2.0);
                    egui::ScrollArea::vertical()
                        .max_height((ui.available_height() - 106.0).max(40.0))
                        .show(ui, |ui| {
                            for (index, session) in self.sessions.iter().enumerate() {
                                if !session.matches(&self.search) {
                                    continue;
                                }
                                let response = drawer_item(
                                    ui,
                                    &session.title(),
                                    index == self.active && !self.settings,
                                );
                                response.widget_info(|| {
                                    egui::WidgetInfo::selected(
                                        egui::WidgetType::SelectableLabel,
                                        true,
                                        index == self.active,
                                        session.title(),
                                    )
                                });
                                if response.clicked() {
                                    self.active = index;
                                    self.settings = false;
                                    self.files_open = false;
                                    self.changes_open = false;
                                    self.commands_open = false;
                                    self.git_open = false;
                                    self.github_open = false;
                                    self.tools_open = false;
                                    self.plugins_open = false;
                                    self.library_open = false;
                                    self.delete_confirm = false;
                                    self.sessions_dirty = true;
                                }
                            }
                        });
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                        ui.horizontal(|ui| {
                            ui.add_space(16.0);
                            ui.label(
                                RichText::new(if self.save_chats {
                                    "チャットをこのPCに保存します"
                                } else {
                                    "履歴は終了時に消去されます"
                                })
                                .size(12.0)
                                .color(MUTED()),
                            );
                        });
                        ui.add_space(8.0);
                        if drawer_item(ui, "設定", self.settings).clicked() {
                            self.settings = true;
                            self.files_open = false;
                            self.changes_open = false;
                            self.commands_open = false;
                            self.git_open = false;
                            self.github_open = false;
                            self.tools_open = false;
                            self.plugins_open = false;
                            self.library_open = false;
                        }
                    });
                });
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(SHELL()).inner_margin(8.0))
            .show(ctx, |ui| {
                let area = ui.max_rect();
                ui.painter().rect_filled(area, SHAPE_LARGE, SURFACE());
                ui.scope_builder(egui::UiBuilder::new().max_rect(area.shrink(20.0)), |ui| {
                    ui.multiply_opacity(reveal);
                    ui.horizontal(|ui| {
                        if !self.sidebar
                            && !narrow
                            && icon_button(ui, Icon::Panel, "サイドバーを開く", true, false)
                                .clicked()
                        {
                            self.sidebar = true;
                        }
                        if narrow {
                            ui.menu_button("チャット", |ui| {
                                if ui.button("新しいチャット").clicked() {
                                    self.new_chat();
                                    ui.close_menu();
                                }
                                ui.add(
                                    crate::text_field(&mut self.search)
                                        .hint_text("チャットを検索"),
                                );
                                for (i, s) in self.sessions.iter().enumerate() {
                                    if !s.matches(&self.search) {
                                        continue;
                                    }
                                    if ui.selectable_label(i == self.active, s.title()).clicked() {
                                        self.active = i;
                                        self.settings = false;
                                        self.files_open = false;
                                        self.changes_open = false; self.commands_open = false; self.git_open=false; self.github_open=false; self.tools_open=false;self.plugins_open=false; self.library_open = false;
                                        self.delete_confirm = false;
                                        self.sessions_dirty = true;
                                        ui.close_menu();
                                    }
                                }
                            });
                        }
                        let title = if self.library_open {"ライブラリ".into()} else if self.plugins_open {"拡張機能".into()} else if self.tools_open {"AIに作業を相談".into()} else if self.settings { "設定".into() } else if self.github_open { "GitHub".into() } else if self.git_open { "Git".into() } else if self.commands_open { "コマンドの実行".into() } else if self.changes_open { "変更の確認".into() }
                            else if self.files_open { "参照ファイル".into() } else { self.sessions[self.active].title() };
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if icon_button(ui, Icon::Settings, "設定", true, false).clicked() {
                                self.files_open = false;
                                self.changes_open = false; self.commands_open = false; self.git_open=false; self.github_open=false; self.tools_open=false;self.plugins_open=false; self.library_open = false;
                                self.settings = !self.settings;
                            }
                            if icon_button(ui, Icon::New, "新しいチャット", true, false).clicked()
                            {
                                self.new_chat();
                            }
                            if tab_button(ui, "ファイル", self.files_open).clicked() { self.workspace_panel.set_root_if_empty(self.sessions[self.active].workspace_root.as_deref()); self.files_open = !self.files_open; self.settings = false; self.changes_open = false; self.commands_open = false; self.git_open=false; self.github_open=false; self.tools_open=false;self.plugins_open=false; self.library_open = false; }
                            if tab_button(ui, "ライブラリ", self.library_open).clicked() {
                                let open = !self.library_open;
                                self.close_pages();
                                self.library_open = open;
                                let workspace = self.sessions[self.active].workspace_root.clone();
                                self.library_panel.reload(workspace.as_deref());
                            }
                            if tab_button(ui, "変更", self.changes_open).clicked() { self.changes_open = !self.changes_open; self.settings = false; self.files_open = false; self.commands_open=false; self.git_open=false; self.github_open=false; self.tools_open=false;self.plugins_open=false; self.library_open = false; }
                            let run_menu = tab_button(ui, "実行", self.commands_open || self.tools_open || self.plugins_open);
                            let run_menu_id = ui.make_persistent_id("run-menu");
                            if run_menu.clicked() { ui.memory_mut(|m| m.toggle_popup(run_menu_id)); }
                            egui::popup_below_widget(ui, run_menu_id, &run_menu, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                                ui.set_min_width(200.0);
                                ui.style_mut().visuals.button_frame = false;
                                if ui.button("コマンドを確認して実行").clicked() {self.commands_open=true;self.tools_open=false;self.plugins_open=false; self.library_open = false;self.settings=false;self.files_open=false;self.changes_open=false;self.git_open=false;self.github_open=false;self.command_panel.set_initial_root(self.sessions[self.active].workspace_root.as_deref());ui.close_menu();}
                                if ui.button("AIに作業を相談").clicked() {
                                    let session=&self.sessions[self.active];let prompt=codebot_core::compose_input(&session.draft,&session.attachments).unwrap_or_else(|_|session.draft.clone());
                                    let connectors=self.connectors.iter().filter(|c|session.connectors.contains(&c.name)).cloned().collect();
                                    self.tool_panel.set_initial(session.workspace_root.as_deref(),&prompt,session.skills.clone(),connectors);
                                    self.commands_open=false;self.settings=false;self.files_open=false;self.changes_open=false;self.git_open=false;self.github_open=false;self.tools_open=true;self.plugins_open=false; self.library_open = false;ui.close_menu();
                                }
                                if ui.button("拡張機能").clicked() {
                                    let session=&self.sessions[self.active];self.plugin_panel.set_initial(session.workspace_root.as_deref(),&session.draft,&session.attachments);
                                    self.commands_open=false;self.settings=false;self.files_open=false;self.changes_open=false;self.git_open=false;self.github_open=false;self.tools_open=false;self.plugins_open=true; self.library_open = false;ui.close_menu();
                                }
                            });
                            if tab_button(ui, "Git", self.git_open).clicked() { self.git_open=!self.git_open; self.github_open=false; self.tools_open=false;self.plugins_open=false; self.library_open = false; self.settings=false; self.files_open=false; self.changes_open=false; self.commands_open=false; self.git_panel.set_initial_root(self.sessions[self.active].workspace_root.as_deref()); }
                            if tab_button(ui, "GitHub", self.github_open).clicked() { self.github_open=!self.github_open;self.tools_open=false;self.plugins_open=false; self.library_open = false; self.git_open=false; self.settings=false; self.files_open=false; self.changes_open=false; self.commands_open=false; }
                            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                ui.add(egui::Label::new(RichText::new(title).size(16.0).color(TEXT())).truncate());
                            });
                        });
                    });
                    ui.add_space(12.0);
                    let bounds = ui.available_rect_before_wrap();
                    let width = (bounds.width() - 32.0).min(MEASURE).max(200.0);
                    let left = bounds.center().x - width / 2.0;
                    let composer_width = (bounds.width() - 32.0).min(COMPOSER_WIDTH).max(200.0);
                    let composer_left = bounds.center().x - composer_width / 2.0;
                    if self.library_open {
                        let rect=egui::Rect::from_min_max(egui::pos2(left,bounds.top()),egui::pos2(left+width,bounds.bottom()));
                        ui.scope_builder(egui::UiBuilder::new().max_rect(rect),|ui| {
                            egui::ScrollArea::vertical().id_salt("library-page").show(ui,|ui| {
                                let session = &mut self.sessions[self.active];
                                let changes = self.library_panel.ui(ui, &mut self.connectors, library::Target {
                                    skills: &mut session.skills,
                                    connectors: &mut session.connectors,
                                    workspace: session.workspace_root.as_deref(),
                                });
                                if changes.session { self.sessions_dirty = true; }
                                if changes.preferences { self.save_preferences(); }
                                if let Some(error) = &self.preferences_error { ui.colored_label(ui.visuals().error_fg_color, error); }
                                ui.add_space(8.0);
                                if ui.button("チャットに戻る").clicked() { self.library_open = false; }
                            });
                        });
                    } else if self.plugins_open {
                        let rect=egui::Rect::from_min_max(egui::pos2(left,bounds.top()),egui::pos2(left+width,bounds.bottom()));
                        ui.scope_builder(egui::UiBuilder::new().max_rect(rect),|ui| {
                            egui::ScrollArea::vertical().show(ui,|ui| {
                                self.plugin_panel.ui(ui,self.pending.is_none()&&!self.change_panel.is_busy()&&!self.command_panel.is_busy()&&!self.git_panel.is_busy()&&!self.github_panel.is_busy()&&!self.tool_panel.is_busy());
                                if ui.button("チャットに戻る").clicked() {self.plugins_open=false; self.library_open = false;}
                            });
                        });
                    } else if self.tools_open {
                        let rect=egui::Rect::from_min_max(egui::pos2(left,bounds.top()),egui::pos2(left+width,bounds.bottom()));
                        ui.scope_builder(egui::UiBuilder::new().max_rect(rect),|ui| {
                            egui::ScrollArea::vertical().show(ui,|ui| {
                                let path=self.config_path.trim();
                                match Config::load(if path.is_empty() {None} else {Some(Path::new(path))}) {
                                    Ok(config)=>{
                                        let api=self.api_configs.get(&self.backend).cloned().unwrap_or_else(||self.backend.defaults());
                                        self.tool_panel.ui(ui,self.backend,&api,&self.api_key,&config,self.pending.is_none()&&!self.change_panel.is_busy()&&!self.command_panel.is_busy()&&!self.git_panel.is_busy()&&!self.github_panel.is_busy()&&!self.plugin_panel.is_busy());
                                    },
                                    Err(error)=>{ui.label(error);}
                                }
                                if ui.button("チャットに戻る").clicked() {self.tools_open=false;self.plugins_open=false; self.library_open = false;}
                            });
                        });
                    } else if self.github_open {
                        let rect=egui::Rect::from_min_max(egui::pos2(left,bounds.top()),egui::pos2(left+width,bounds.bottom()));
                        ui.scope_builder(egui::UiBuilder::new().max_rect(rect),|ui| {
                            egui::ScrollArea::vertical().show(ui,|ui| {
                                if let Some(action)=self.github_panel.ui(ui,self.pending.is_none() && !self.change_panel.is_busy() && !self.command_panel.is_busy() && !self.git_panel.is_busy() && !self.tool_panel.is_busy() && !self.plugin_panel.is_busy()) {
                                    match action {
                                        github_panel::Action::Save=>self.save_preferences(),
                                        github_panel::Action::Attach(text)=>{
                                            let mut limited=text.chars().take(12000).collect::<String>();if text.chars().count()>12000 {limited.push_str("\n（12000文字以降を省略）");}
                                            let session=&mut self.sessions[self.active];if !session.draft.is_empty() {session.draft.push_str("\n\n");}session.draft.push_str(&limited);session.history_enabled=false;session.notice=Some("GitHubの資料を下書きに追加しました。内容を確認して送信してください。".into());self.sessions_dirty=true;self.github_open=false; self.tools_open=false;self.plugins_open=false; self.library_open = false;
                                        }
                                        github_panel::Action::Branch{branch,source}=>{
                                            match self.git_panel.prepare_issue_branch(ui.ctx(),self.sessions[self.active].workspace_root.as_deref(),&branch,&source) {
                                                Ok(())=>{self.git_open=true;self.github_open=false; self.tools_open=false;self.plugins_open=false; self.library_open = false;},Err(error)=>self.github_panel.report_message(error)
                                            }
                                        }
                                    }
                                }
                                if let Some(error)=&self.preferences_error {ui.colored_label(ui.visuals().error_fg_color,error);}
                                if ui.button("チャットに戻る").clicked() {self.github_open=false; self.tools_open=false;self.plugins_open=false; self.library_open = false;}
                            });
                        });
                    } else if self.git_open {
                        let rect=egui::Rect::from_min_max(egui::pos2(left,bounds.top()),egui::pos2(left+width,bounds.bottom()));
                        ui.scope_builder(egui::UiBuilder::new().max_rect(rect),|ui| {
                            egui::ScrollArea::vertical().show(ui,|ui| {
                                self.git_panel.ui(ui,self.pending.is_none() && !self.change_panel.is_busy() && !self.command_panel.is_busy() && !self.github_panel.is_busy() && !self.tool_panel.is_busy() && !self.plugin_panel.is_busy());
                                if ui.button("チャットに戻る").clicked() {self.git_open=false; self.github_open=false; self.tools_open=false;self.plugins_open=false; self.library_open = false;}
                            });
                        });
                    } else if self.commands_open {
                        let rect=egui::Rect::from_min_max(egui::pos2(left,bounds.top()),egui::pos2(left+width,bounds.bottom()));
                        ui.scope_builder(egui::UiBuilder::new().max_rect(rect),|ui| {
                            egui::ScrollArea::vertical().show(ui,|ui| {
                                if let Some(output)=self.command_panel.ui(ui,self.pending.is_none() && !self.change_panel.is_busy() && !self.git_panel.is_busy() && !self.github_panel.is_busy() && !self.tool_panel.is_busy() && !self.plugin_panel.is_busy()) {
                                    let session=&mut self.sessions[self.active];
                                    if !session.draft.is_empty() {session.draft.push_str("\n\n");}
                                    session.draft.push_str(&output);session.history_enabled=false;self.sessions_dirty=true;self.commands_open=false; self.git_open=false; self.github_open=false; self.tools_open=false;self.plugins_open=false; self.library_open = false;
                                }
                                if ui.button("チャットに戻る").clicked() {self.commands_open=false; self.git_open=false; self.github_open=false; self.tools_open=false;self.plugins_open=false; self.library_open = false;}
                            });
                        });
                    } else if self.changes_open {
                        let rect = egui::Rect::from_min_max(egui::pos2(left,bounds.top()),egui::pos2(left+width,bounds.bottom()));
                        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                            egui::ScrollArea::vertical().show(ui, |ui| {
                                self.change_panel.ui(ui,self.pending.is_none() && !self.command_panel.is_busy() && !self.git_panel.is_busy() && !self.github_panel.is_busy() && !self.tool_panel.is_busy() && !self.plugin_panel.is_busy());
                                if let Some(error) = &self.preferences_error { ui.colored_label(ui.visuals().error_fg_color,error); }
                                if ui.button("チャットに戻る").clicked() { self.changes_open = false; self.commands_open = false; self.git_open=false; self.github_open=false; self.tools_open=false;self.plugins_open=false; self.library_open = false; }
                            });
                        });
                    } else if self.files_open {
                        let rect = egui::Rect::from_min_max(egui::pos2(left, bounds.top()), egui::pos2(left+width, bounds.bottom()));
                        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                            egui::ScrollArea::vertical().show(ui, |ui| {
                                if let Some((attachment, action)) = self.workspace_panel.ui(ui) {
                                    let session = &mut self.sessions[self.active];
                                    session.history_enabled = false;
                                    if session.workspace_root.as_deref().is_some_and(|root| root != attachment.root) {
                                        session.attachments.clear(); session.history.clear();
                                        session.notice = Some("作業フォルダを変更したため、送信用の履歴と以前の添付を消去しました。".into());
                                    }
                                    session.workspace_root = Some(attachment.root.clone());
                                    if session.attachments.len() < 8 {
                                        if session.attachments.iter().any(|a| a.root != attachment.root) { session.attachments.clear(); session.history.clear(); }
                                        session.attachments.push(attachment);
                                        session.analysis_action = Some(action);
                                        if session.draft.trim().is_empty() { session.draft = format!("選択した範囲の{}をお願いします。", action.label()); }
                                        self.sessions_dirty = true; self.files_open = false;
                                    } else { session.notice = Some("添付は8ファイルまでです".into()); }
                                }
                                if ui.button("チャットに戻る").clicked() { self.files_open = false; }
                            });
                        });
                    } else if self.settings {
                        let rect = egui::Rect::from_min_max(
                            egui::pos2(left, bounds.top()),
                            egui::pos2(left + width, bounds.bottom()),
                        );
                        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                            egui::ScrollArea::vertical().show(ui, |ui| self.settings_page(ui));
                        });
                    } else if self.sessions[self.active].messages.is_empty() {
                        let top = bounds.top() + (bounds.height() * 0.27).max(12.0);
                        let rect = egui::Rect::from_min_max(
                            egui::pos2(composer_left, top),
                            egui::pos2(composer_left + composer_width, bounds.bottom()),
                        );
                        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                            ui.label(RichText::new("新しいチャット").size(28.0));
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new("コードやログを一緒に確認しましょう。")
                                    .size(14.0)
                                    .color(MUTED()),
                            );
                            ui.add_space(28.0);
                            self.composer(ui);
                        });
                    } else {
                        let extra = if self.backend == Backend::Rules {
                            0.0
                        } else {
                            40.0
                        } + if self.sessions[self.active].notice.is_some() {
                            80.0
                        } else {
                            0.0
                        };
                        let extra = extra + self.sessions[self.active].attachments.len() as f32 * 26.0;
                        let session = &self.sessions[self.active];
                        let extra = extra
                            + if session.skills.is_empty() && session.connectors.is_empty() { 0.0 } else { 40.0 }
                            + if self.context_warning().is_some() { 36.0 } else { 0.0 }
                            + if self.mentioned_files().is_empty() { 0.0 } else { 40.0 };
                        let composer_top =
                            (bounds.bottom() - 108.0 - extra).max(bounds.top() + 80.0);
                        let history = egui::Rect::from_min_max(
                            egui::pos2(left, bounds.top() + 12.0),
                            egui::pos2(left + width, composer_top - 18.0),
                        );
                        ui.scope_builder(egui::UiBuilder::new().max_rect(history), |ui| {
                            self.conversation(ui)
                        });
                        let rect = egui::Rect::from_min_max(
                            egui::pos2(composer_left, composer_top),
                            egui::pos2(composer_left + composer_width, bounds.bottom()),
                        );
                        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                            self.composer(ui)
                        });
                    }
                });
            });
        if self.change_panel.take_settings_changed() {
            self.save_preferences();
        }
        self.flush_chats();
    }
}

/// Inline diff of a proposal in an answer, or why it cannot be shown.
type DiffPreview = Result<Vec<codebot_core::ChangePreview>, String>;

/// Saved chats being read in the background, and when reading began.
type StartupLoad = (
    mpsc::Receiver<Result<Option<chat_storage::Archive>, String>>,
    std::time::Instant,
);

fn load_archive(path: Option<PathBuf>) -> Result<Option<chat_storage::Archive>, String> {
    path.ok_or_else(|| "チャット保存先を取得できません".to_owned())
        .and_then(|path| chat_storage::load(&path))
}

#[derive(Clone, Copy)]
enum Icon {
    Send,
    Stop,
    Copy,
    New,
    Panel,
    Settings,
}

/// Material state layer: an on-color overlay for hover (8%) and press (12%).
fn state_layer(ui: &egui::Ui, response: &egui::Response, radius: f32, color: Color32) {
    if !response.enabled() {
        return;
    }
    let opacity = if response.is_pointer_button_down_on() {
        0.12
    } else if response.hovered() {
        0.08
    } else {
        return;
    };
    ui.painter()
        .rect_filled(response.rect, radius, color.gamma_multiply(opacity));
}

/// Material focus indicator drawn just outside the component.
fn focus_ring(ui: &egui::Ui, response: &egui::Response, radius: f32) {
    if response.has_focus() {
        ui.painter().rect_stroke(
            response.rect.expand(2.0),
            radius + 2.0,
            egui::Stroke::new(2.0_f32, ACCENT()),
            egui::StrokeKind::Outside,
        );
    }
}

/// Material filled text field padding (egui's default is 4×2).
const FIELD_MARGIN: egui::Margin = egui::Margin::symmetric(14, 8);

/// Single-line text field with Material padding.
fn text_field(text: &mut dyn egui::TextBuffer) -> egui::TextEdit<'_> {
    egui::TextEdit::singleline(text).margin(FIELD_MARGIN)
}

/// Multi-line text field with Material padding.
fn text_area(text: &mut dyn egui::TextBuffer) -> egui::TextEdit<'_> {
    egui::TextEdit::multiline(text).margin(FIELD_MARGIN)
}

fn history_chars(history: &[ChatMessage]) -> usize {
    history.iter().map(|m| m.content.chars().count()).sum()
}

/// Drop the oldest user/answer pairs until the history fits in `allowed`
/// characters. Returns the number of pairs removed.
fn trim_history(history: &mut Vec<ChatMessage>, allowed: usize) -> usize {
    let mut removed = 0;
    while history.len() >= 2 && history_chars(history) > allowed {
        history.drain(..2);
        removed += 1;
    }
    removed
}

/// Add the chat's skill instructions in front of the first request message.
/// The input limit is widened by the instructions' size; the total stays within
/// `max_context_chars`.
fn apply_skills(
    session: &Session,
    config: &Config,
    messages: &mut [ChatMessage],
) -> Result<(), String> {
    let skills = library::resolve_skills(&session.skills, session.workspace_root.as_deref())?;
    codebot_core::prepend_skills(config, messages, &skills)
}

/// Material input chip with a colored dot and a remove action.
/// Material input chip with a colored dot; `removable` adds the drawn close mark.
fn chip(
    ui: &mut egui::Ui,
    kind: &str,
    name: &str,
    color: Color32,
    removable: bool,
) -> egui::Response {
    let mut label = egui::text::LayoutJob::default();
    label.append(
        &format!("{kind}: {name}"),
        14.0,
        egui::TextFormat::simple(egui::FontId::proportional(13.0), ON_SECONDARY_CONTAINER()),
    );
    // Room for the drawn close mark (the UI fonts have no ✕ glyph).
    label.append(
        " ",
        if removable { 18.0 } else { 4.0 },
        egui::TextFormat::simple(egui::FontId::proportional(13.0), MUTED()),
    );
    let response = ui.add(
        egui::Button::new(label)
            .fill(SURFACE())
            .stroke(egui::Stroke::new(1.0_f32, BORDER()))
            .corner_radius(8.0)
            .min_size(egui::vec2(0.0, 30.0)),
    );
    ui.painter().circle_filled(
        egui::pos2(response.rect.left() + 14.0, response.rect.center().y),
        4.0,
        color,
    );
    let mark = egui::pos2(response.rect.right() - 16.0, response.rect.center().y);
    let stroke = egui::Stroke::new(1.5_f32, MUTED());
    if removable {
        for (a, b) in [((-3.5, -3.5), (3.5, 3.5)), ((-3.5, 3.5), (3.5, -3.5))] {
            ui.painter().line_segment(
                [mark + egui::vec2(a.0, a.1), mark + egui::vec2(b.0, b.1)],
                stroke,
            );
        }
    }

    state_layer(ui, &response, 8.0, TEXT());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            format!("{kind}「{name}」を外す"),
        )
    });
    response
}

/// Material search bar: a full-radius container with a leading search icon.
fn search_bar(ui: &mut egui::Ui, query: &mut String) -> egui::Response {
    egui::Frame::new()
        .fill(SURFACE())
        .corner_radius(SHAPE_FULL)
        .inner_margin(egui::Margin {
            left: 44,
            right: 16,
            top: 10,
            bottom: 10,
        })
        .show(ui, |ui| {
            let response = ui.add(
                crate::text_field(query)
                    .hint_text(RichText::new("チャットを検索").color(MUTED()))
                    .frame(false)
                    .margin(egui::Vec2::ZERO)
                    .desired_width(f32::INFINITY),
            );
            let c = egui::pos2(response.rect.left() - 23.0, response.rect.center().y - 1.0);
            let stroke = egui::Stroke::new(1.75_f32, MUTED());
            ui.painter().circle_stroke(c, 5.0, stroke);
            ui.painter()
                .line_segment([c + egui::vec2(3.8, 3.8), c + egui::vec2(7.5, 7.5)], stroke);
            response
        })
        .inner
}

/// Navigation drawer item with a pill-shaped secondary-container indicator.
fn drawer_item(ui: &mut egui::Ui, text: &str, selected: bool) -> egui::Response {
    let response = ui.add(
        egui::Button::new(RichText::new(text).size(14.0).color(if selected {
            ON_SECONDARY_CONTAINER()
        } else {
            MUTED()
        }))
        .truncate()
        .min_size(egui::vec2(ui.available_width(), 44.0))
        .shortcut_text("")
        .corner_radius(SHAPE_FULL)
        .stroke(egui::Stroke::NONE)
        .fill(if selected {
            SELECTED()
        } else {
            Color32::TRANSPARENT
        }),
    );
    let radius = response.rect.height() / 2.0;
    state_layer(ui, &response, radius, TEXT());
    focus_ring(ui, &response, radius);
    response
}

/// Header destination: a Material text button that becomes tonal while its page is open.
fn tab_button(ui: &mut egui::Ui, text: &str, selected: bool) -> egui::Response {
    // Tighter padding below the 900px breakpoint so the bar fits 700px windows.
    let padding = if ui.ctx().screen_rect().width() < 900.0 {
        8.0
    } else {
        16.0
    };
    let previous = std::mem::replace(&mut ui.spacing_mut().button_padding.x, padding);
    let response = ui.add(
        egui::Button::new(RichText::new(text).size(14.0).color(if selected {
            ON_SECONDARY_CONTAINER()
        } else {
            ACCENT()
        }))
        .selected(selected)
        .min_size(egui::vec2(0.0, 36.0))
        .corner_radius(SHAPE_FULL)
        .stroke(egui::Stroke::NONE)
        .fill(if selected {
            SELECTED()
        } else {
            Color32::TRANSPARENT
        }),
    );
    ui.spacing_mut().button_padding.x = previous;
    let radius = response.rect.height() / 2.0;
    state_layer(
        ui,
        &response,
        radius,
        if selected { TEXT() } else { ACCENT() },
    );
    focus_ring(ui, &response, radius);
    response
}

/// Extended FAB for starting a chat.
fn new_chat_button(ui: &mut egui::Ui) -> egui::Response {
    let mut label = egui::text::LayoutJob::simple_singleline(
        "新しいチャット".into(),
        egui::FontId::proportional(14.0),
        ON_PRIMARY_CONTAINER(),
    );
    // Reserve room for the drawn icon while retaining the native button label.
    label.sections[0].leading_space = 28.0;
    // Reserve a slot below the button so its elevation shadow is painted underneath.
    let shadow_slot = ui.painter().add(egui::Shape::Noop);
    let response = ui.add(
        egui::Button::new(label)
            .min_size(egui::vec2(160.0, 52.0))
            .shortcut_text("")
            .corner_radius(SHAPE_LARGE)
            .stroke(egui::Stroke::NONE)
            .fill(PRIMARY_CONTAINER()),
    );
    let level = if response.hovered() { 2.0 } else { 1.0 };
    ui.painter().set(
        shadow_slot,
        egui::Shadow {
            offset: [0, level as i8],
            blur: (3.0 * level) as u8,
            spread: 0,
            color: Color32::from_black_alpha(46),
        }
        .as_shape(response.rect, SHAPE_LARGE),
    );
    state_layer(ui, &response, SHAPE_LARGE, ON_PRIMARY_CONTAINER());
    focus_ring(ui, &response, SHAPE_LARGE);
    let center = egui::pos2(response.rect.left() + 26.0, response.rect.center().y);
    let stroke = egui::Stroke::new(2.0_f32, ON_PRIMARY_CONTAINER());
    for offset in [egui::vec2(6.0, 0.0), egui::vec2(0.0, 6.0)] {
        ui.painter()
            .line_segment([center - offset, center + offset], stroke);
    }
    response
}

/// Material icon button: filled (primary) or standard, always circular.
fn icon_button(
    ui: &mut egui::Ui,
    icon: Icon,
    name: &str,
    enabled: bool,
    primary: bool,
) -> egui::Response {
    let size = if primary { 40.0 } else { 36.0 };
    let response = ui.add_enabled(
        enabled,
        egui::Button::new("")
            .min_size(egui::vec2(size, size))
            .stroke(egui::Stroke::NONE)
            .fill(if primary {
                if enabled {
                    ACCENT()
                } else {
                    DISABLED()
                }
            } else {
                Color32::TRANSPARENT
            })
            .corner_radius(SHAPE_FULL),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, name));
    let radius = response.rect.height() / 2.0;
    if primary && enabled && (response.hovered() || response.is_pointer_button_down_on()) {
        ui.painter()
            .circle_filled(response.rect.center(), radius, ACCENT_HOVER());
    } else if !primary {
        state_layer(ui, &response, radius, MUTED());
    }
    let color = if primary && enabled {
        ON_PRIMARY()
    } else if enabled {
        MUTED()
    } else {
        DISABLED_CONTENT()
    };
    let p = ui.painter();
    let c = response.rect.center();
    let stroke = egui::Stroke::new(1.75_f32, color);
    let line = |a: [f32; 2], b: [f32; 2]| {
        p.line_segment(
            [c + egui::vec2(a[0], a[1]), c + egui::vec2(b[0], b[1])],
            stroke,
        );
    };
    match icon {
        Icon::Stop => {
            p.rect_filled(
                egui::Rect::from_center_size(c, egui::vec2(11.0, 11.0)),
                2.0,
                color,
            );
        }
        Icon::Send => {
            line([0.0, 7.0], [0.0, -7.0]);
            line([-5.0, -2.0], [0.0, -7.0]);
            line([0.0, -7.0], [5.0, -2.0]);
        }
        Icon::New => {
            line([-6.0, 0.0], [6.0, 0.0]);
            line([0.0, -6.0], [0.0, 6.0]);
        }
        Icon::Panel => {
            p.rect_stroke(
                egui::Rect::from_center_size(c, egui::vec2(16.0, 14.0)),
                2.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            line([-2.0, -6.0], [-2.0, 6.0]);
        }
        Icon::Copy => {
            p.rect_stroke(
                egui::Rect::from_center_size(c + egui::vec2(2.0, 2.0), egui::vec2(10.0, 12.0)),
                2.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            line([-2.0, -7.0], [-7.0, -7.0]);
            line([-7.0, -7.0], [-7.0, 3.0]);
        }
        Icon::Settings => {
            for (x, y) in [(-5.0, -3.0), (0.0, 4.0), (5.0, -1.0)] {
                line([x, -7.0], [x, 7.0]);
                p.circle_filled(c + egui::vec2(x, y), 2.7, SURFACE());
                p.circle_stroke(c + egui::vec2(x, y), 2.2, stroke);
            }
        }
    }
    focus_ring(ui, &response, radius);
    response
        .on_hover_text(name)
        .on_disabled_hover_text(if primary {
            "入力後、診断が完了していれば送信できます"
        } else {
            name
        })
}
fn main() -> eframe::Result {
    eframe::run_native(
        "CodeBot",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1180.0, 800.0])
                .with_min_inner_size([700.0, 540.0]),
            ..Default::default()
        },
        Box::new(|cc| {
            let app = App::load(&cc.egui_ctx);
            theme::apply(&cc.egui_ctx, theme::wants_dark(&cc.egui_ctx, app.theme));
            Ok(Box::new(app))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trimming_removes_whole_oldest_pairs_until_the_history_fits() {
        let mut history = vec![
            ChatMessage::user("a".repeat(40)),
            ChatMessage::assistant("b".repeat(40)),
            ChatMessage::user("c".repeat(10)),
            ChatMessage::assistant("d".repeat(10)),
        ];
        assert_eq!(trim_history(&mut history, 50), 1);
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].content, "c".repeat(10));
        assert_eq!(trim_history(&mut history, 50), 0);
        assert_eq!(trim_history(&mut history, 0), 1);
        assert!(history.is_empty());
    }
    #[test]
    fn the_near_limit_warning_follows_the_configured_budget() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "provider = \"openai\"\nmax_context_chars = 100\n").unwrap();
        let mut app = App::default();
        app.config_path = config.to_string_lossy().into_owned();
        app.backend = Backend::Openai;
        app.sessions[0].draft = "x".repeat(70);
        assert!(app.context_warning().is_none());
        app.sessions[0].draft = "x".repeat(85);
        assert_eq!(app.context_warning(), Some((85, 100)));
        app.backend = Backend::Rules;
        assert!(app.context_warning().is_none());
    }
    /// Serve one canned JSON response per connection on a local port.
    fn mock_api(
        responses: Vec<serde_json::Value>,
    ) -> (String, std::thread::JoinHandle<Vec<serde_json::Value>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let join = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let (mut socket, _) = listener.accept().unwrap();
                let mut raw = Vec::new();
                let mut byte = [0];
                while !raw.ends_with(b"\r\n\r\n") {
                    socket.read_exact(&mut byte).unwrap();
                    raw.push(byte[0]);
                }
                let header = String::from_utf8(raw).unwrap().to_lowercase();
                let size: usize = header
                    .lines()
                    .find_map(|l| {
                        l.strip_prefix("content-length:")
                            .map(|v| v.trim().parse().unwrap())
                    })
                    .unwrap();
                let mut body = vec![0; size];
                socket.read_exact(&mut body).unwrap();
                requests.push(serde_json::from_slice(&body).unwrap());
                let bytes = serde_json::to_vec(&response).unwrap();
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).unwrap();
                socket.write_all(&bytes).unwrap();
            }
            requests
        });
        (url, join)
    }
    #[test]
    fn chat_with_tools_runs_through_the_tool_engine_and_records_the_answer() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "provider = \"rules\"\n").unwrap();
        let (url, server) = mock_api(vec![serde_json::json!({
            "choices": [{"finish_reason": "stop", "message": {"content": "ツールは不要でした。"}}]
        })]);
        let mut app = App::default();
        app.config_path = config.to_string_lossy().into_owned();
        app.backend = Backend::Custom;
        app.api_configs.insert(
            Backend::Custom,
            ApiConfig {
                base_url: url,
                model: "mock".into(),
                anonymous: true,
                ..Default::default()
            },
        );
        app.sessions[0].use_tools = true;
        app.sessions[0].draft = "READMEを直して".into();
        let ctx = egui::Context::default();

        // Without a workspace the draft is kept and nothing is sent.
        app.send(&ctx);
        assert!(app.chat_tool.is_none());
        assert_eq!(app.sessions[0].draft, "READMEを直して");
        assert!(app.sessions[0]
            .notice
            .as_deref()
            .unwrap()
            .contains("作業フォルダ"));

        app.sessions[0].workspace_root = Some(dir.path().to_string_lossy().into_owned());
        app.send(&ctx);
        assert!(app.chat_tool.is_some());
        assert!(app.sessions[0].draft.is_empty());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.chat_tool.is_some() {
            assert!(
                std::time::Instant::now() < deadline,
                "tool conversation did not finish"
            );
            let _ = app.poll_chat_tool(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let session = &app.sessions[0];
        assert_eq!(
            session.messages.last().unwrap(),
            &(false, "ツールは不要でした。".to_owned())
        );
        assert_eq!(session.history.len(), 2);
        assert_eq!(session.history[0].content, "READMEを直して");
        let requests = server.join().unwrap();
        assert_eq!(requests[0]["tools"].as_array().unwrap().len(), 2);
    }
    fn wait_for_restore(app: &mut App) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.poll_restore() {
            assert!(
                std::time::Instant::now() < deadline,
                "startup load did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    #[test]
    fn startup_restore_runs_in_the_background_and_never_overwrites_the_archive_early() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chats.json");
        let saved = Session {
            label: "saved chat".into(),
            messages: vec![(true, "question".into()), (false, "answer".into())],
            ..Default::default()
        };
        chat_storage::save(&path, &[saved], 0).unwrap();
        let original = std::fs::read(&path).unwrap();

        let mut app = App::default();
        app.save_chats = true;
        app.archive_path_override = path.to_string_lossy().into_owned();
        app.start_restore(&egui::Context::default());
        // Until the result is applied, edits to the startup session are not saved.
        app.sessions[0].draft = "typed during loading".into();
        app.sessions_dirty = true;
        app.flush_chats();
        assert_eq!(std::fs::read(&path).unwrap(), original);

        wait_for_restore(&mut app);
        assert_eq!(app.sessions.len(), 1);
        assert_eq!(app.sessions[0].label, "saved chat");
        assert!(!app.archive_blocked);
        assert!(app.storage_error.is_none());
        assert!(app.startup_load.is_none());
    }
    #[test]
    fn a_broken_archive_found_at_startup_is_reported_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chats.json");
        std::fs::write(&path, "not json").unwrap();
        let mut app = App::default();
        app.save_chats = true;
        app.archive_path_override = path.to_string_lossy().into_owned();
        app.start_restore(&egui::Context::default());
        wait_for_restore(&mut app);
        assert!(app.archive_blocked);
        assert!(app.storage_error.is_some());
        app.sessions_dirty = true;
        app.flush_chats();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json");
        // A fast load shows no loading state, so there is nothing to cross-fade.
        assert!(app.reveal.is_none());
    }
    #[test]
    fn skills_are_added_to_the_request_copy_only_and_missing_skills_block_sending() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join(".claude/skills/demo-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: demo-skill\ndescription: demo\n---\nAlways answer in bullet points.",
        )
        .unwrap();
        let session = Session {
            skills: vec!["demo-skill".into()],
            workspace_root: Some(dir.path().to_string_lossy().into_owned()),
            ..Default::default()
        };
        let config = Config {
            max_input_chars: 10,
            ..Default::default()
        };
        let mut messages = vec![ChatMessage::user("hi")];
        apply_skills(&session, &config, &mut messages).unwrap();
        assert!(messages[0]
            .content
            .contains("Always answer in bullet points."));
        assert!(messages[0].content.ends_with("--- 利用者の依頼 ---\nhi"));

        let small = Config {
            max_context_chars: 20,
            ..Default::default()
        };
        let mut messages = vec![ChatMessage::user("hi")];
        assert!(apply_skills(&session, &small, &mut messages).is_err());

        let missing = Session {
            skills: vec!["not-installed".into()],
            ..Default::default()
        };
        let mut messages = vec![ChatMessage::user("hi")];
        assert!(apply_skills(&missing, &config, &mut messages)
            .unwrap_err()
            .contains("not-installed"));
    }
    #[test]
    fn restoring_an_interrupted_generation_marks_partial_text_and_keeps_it_out_of_history() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::default();
        app.preferences_path = Some(dir.path().join("gui-preferences.toml"));
        app.save_chats = true;
        app.sessions[0].generation_incomplete = true;
        app.sessions[0].messages = vec![(true, "question".into()), (false, "partial".into())];
        app.sessions_dirty = true;
        app.flush_chats();
        app.restore_chats(false);
        assert!(!app.sessions[0].generation_incomplete);
        assert!(app.sessions[0].messages[1]
            .1
            .contains("生成中にアプリが終了"));
        assert!(app.sessions[0].history.is_empty());
        assert!(!app.sessions[0].history_enabled);
    }
    #[test]
    fn stopping_keeps_partial_text_ignores_late_deltas_and_unblocks_the_next_request() {
        let mut app = App::default();
        app.sessions[0].in_flight_input = Some("request".into());
        let (tx, rx) = mpsc::channel();
        app.pending = Some((0, rx));
        app.request_cancel = Some(Arc::new(AtomicBool::new(false)));
        tx.send(StreamEvent::Delta("途中の回答".into())).unwrap();
        app.poll();
        app.new_chat();
        app.sessions[1].draft = "別の下書き".into();
        app.stop_generation();
        tx.send(StreamEvent::Delta("遅延イベント".into())).unwrap();
        tx.send(StreamEvent::Finished(StreamEnd::Complete)).unwrap();
        app.poll();
        assert!(app.pending.is_none());
        assert!(app.sessions[0].history.is_empty());
        assert!(app.sessions[0].messages[0].1.contains("途中の回答"));
        assert!(app.sessions[0].messages[0].1.contains("生成を停止"));
        assert!(!app.sessions[0].messages[0].1.contains("遅延イベント"));
        assert_eq!(app.sessions[1].draft, "別の下書き");
        app.sessions[1].in_flight_input = Some("next request".into());
        let (next_tx, rx) = mpsc::channel();
        app.pending = Some((1, rx));
        next_tx.send(StreamEvent::Delta("次の回答".into())).unwrap();
        next_tx
            .send(StreamEvent::Finished(StreamEnd::Complete))
            .unwrap();
        app.poll();
        assert_eq!(app.sessions[1].history.len(), 2);
    }
    #[test]
    fn archive_reload_preserves_corrupt_files_and_does_not_duplicate_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::default();
        app.preferences_path = Some(dir.path().join("gui-preferences.toml"));
        app.save_chats = true;
        app.sessions[0].draft = "日本語の下書き".into();
        app.sessions_dirty = true;
        app.flush_chats();
        assert!(app.storage_error.is_none());
        app.restore_chats(true);
        app.restore_chats(true);
        assert_eq!(app.sessions.len(), 1);
        assert_eq!(app.sessions[0].draft, "日本語の下書き");
        let path = app.archive_path().unwrap();
        std::fs::write(&path, "broken data").unwrap();
        app.restore_chats(true);
        assert!(app.archive_blocked);
        app.sessions_dirty = true;
        app.flush_chats();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "broken data");
    }
    #[test]
    fn delete_and_search_keep_a_valid_active_chat() {
        let mut app = App::default();
        app.sessions[0].label = "Rustの調査".into();
        assert!(app.sessions[0].matches("RUST"));
        assert!(!app.sessions[0].matches("other"));
        app.new_chat();
        app.delete_active_chat();
        assert_eq!(app.active, 0);
        assert_eq!(app.sessions[0].title(), "Rustの調査");
        app.delete_active_chat();
        assert_eq!(app.sessions.len(), 1);
        assert_eq!(app.active, 0);
        assert!(app.sessions[0].messages.is_empty());
    }
    #[test]
    fn history_is_opt_in_and_changed_destinations_require_a_new_opt_in() {
        let mut session = Session::default();
        session.history = vec![
            ChatMessage::user("old question"),
            ChatMessage::assistant("old answer"),
        ];
        session.context_key = Some("provider A".into());
        session.draft = "new question".into();
        assert_eq!(
            session
                .request_messages(&Config::default(), "provider A", false)
                .unwrap()
                .len(),
            1
        );
        session.history_enabled = true;
        assert_eq!(
            session
                .request_messages(&Config::default(), "provider A", false)
                .unwrap()
                .len(),
            3
        );
        assert!(session
            .request_messages(&Config::default(), "provider B", false)
            .is_err());
        assert!(!session.history_enabled);
        assert_eq!(session.draft, "new question");
        assert_eq!(session.history.len(), 2);
        session.context_key = Some("provider B".into());
        session.history_enabled = true;
        assert_eq!(
            session
                .request_messages(&Config::default(), "provider B", false)
                .unwrap()
                .len(),
            3
        );
    }
    #[test]
    fn failed_answers_never_enter_the_sendable_history() {
        let mut app = App::default();
        app.sessions[0].in_flight_input = Some("failed request".into());
        let (tx, rx) = mpsc::channel();
        app.pending = Some((0, rx));
        tx.send(StreamEvent::Finished(StreamEnd::Failed("HTTP 503".into())))
            .unwrap();
        app.poll();
        assert!(app.sessions[0].history.is_empty());
        app.sessions[0].in_flight_input = Some("good request".into());
        let (tx, rx) = mpsc::channel();
        app.pending = Some((0, rx));
        tx.send(StreamEvent::Delta("answer".into())).unwrap();
        tx.send(StreamEvent::Finished(StreamEnd::Complete)).unwrap();
        app.new_chat();
        app.poll();
        assert_eq!(
            app.sessions[0].history,
            vec![
                ChatMessage::user("good request"),
                ChatMessage::assistant("answer")
            ]
        );
        assert!(app.sessions[1].history.is_empty());
    }

    #[test]
    fn new_chat_preserves_existing_drafts_and_returns_from_settings() {
        let mut app = App::default();
        app.sessions[0].draft = "未送信の調査内容".into();
        app.settings = true;

        app.new_chat();

        assert_eq!(app.active, 1);
        assert!(!app.settings);
        assert_eq!(app.sessions[0].draft, "未送信の調査内容");
        assert!(app.sessions[1].draft.is_empty());
        assert!(app.sessions[1].messages.is_empty());
    }

    #[test]
    fn response_goes_to_its_origin_after_switching_chats() {
        let mut app = App::default();
        app.sessions[0]
            .messages
            .push((true, "first request".into()));
        let (tx, rx) = mpsc::channel();
        app.pending = Some((0, rx));
        app.new_chat();
        app.sessions[1].draft = "second request draft".into();
        tx.send(StreamEvent::Delta("first response".into()))
            .unwrap();
        tx.send(StreamEvent::Finished(StreamEnd::Complete)).unwrap();

        app.poll();

        assert_eq!(app.active, 1);
        assert_eq!(
            app.sessions[0].messages[1],
            (false, "first response".into())
        );
        assert!(app.sessions[1].messages.is_empty());
        assert_eq!(app.sessions[1].draft, "second request draft");
        assert!(app.pending.is_none());
        app.poll();
        assert_eq!(app.sessions[0].messages.len(), 2);
    }

    #[test]
    fn disconnected_worker_adds_one_error_and_unblocks_sending() {
        let mut app = App::default();
        let (tx, rx) = mpsc::channel();
        app.pending = Some((0, rx));
        drop(tx);

        app.poll();

        assert!(app.pending.is_none());
        assert_eq!(app.sessions[0].messages.len(), 1);
        assert!(!app.sessions[0].messages[0].0);
        assert!(app.sessions[0].messages[0]
            .1
            .contains("処理が中断されました"));
        app.poll();
        assert_eq!(app.sessions[0].messages.len(), 1);
    }

    #[test]
    fn empty_or_busy_submission_does_not_erase_a_draft() {
        let ctx = egui::Context::default();
        let mut app = App::default();
        app.sessions[0].draft = " \n\t".into();
        app.send(&ctx);
        assert_eq!(app.sessions[0].draft, " \n\t");
        assert!(app.pending.is_none());
        assert!(app.sessions[0].messages.is_empty());

        let (_tx, rx) = mpsc::channel();
        app.pending = Some((0, rx));
        app.new_chat();
        app.sessions[1].draft = "keep this input".into();
        app.send(&ctx);
        assert_eq!(app.sessions[1].draft, "keep this input");
        assert!(app.sessions[1].messages.is_empty());
        assert_eq!(app.pending.as_ref().unwrap().0, 0);
    }
    #[test]
    fn local_send_without_a_loaded_model_keeps_the_draft() {
        let mut app = App::default();
        app.backend = Backend::Local;
        app.sessions[0].draft = "調査内容".into();
        app.send(&egui::Context::default());
        assert!(app.settings);
        assert!(app.pending.is_none());
        assert_eq!(app.sessions[0].draft, "調査内容");
        assert!(app.sessions[0].messages.is_empty());
    }
}
