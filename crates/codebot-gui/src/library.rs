//! Library page: skills (SKILL.md instructions) and connectors (local MCP
//! servers) that can be attached to the current chat.
use codebot_core::{ConnectorConfig, ConnectorTool, Skill, SkillSource};
use eframe::egui::{self, RichText};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
};

type TestResult = (String, Result<Vec<ConnectorTool>, String>);

struct TestTask {
    rx: mpsc::Receiver<TestResult>,
    cancel: Arc<AtomicBool>,
}
impl Drop for TestTask {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub struct LibraryPanel {
    pub skills: Vec<Skill>,
    problems: Vec<(PathBuf, String)>,
    loaded_for: Option<String>,
    import_path: String,
    remove_skill: Option<String>,
    remove_connector: Option<String>,
    message: Option<String>,
    form_name: String,
    form_command: String,
    form_args: String,
    form_env: String,
    form_timeout: u64,
    tested: BTreeMap<String, Result<Vec<ConnectorTool>, String>>,
    test: Option<TestTask>,
}

/// What the page changed, for the app to persist.
#[derive(Default)]
pub struct Changes {
    pub preferences: bool,
    pub session: bool,
}

/// The chat the page attaches skills/connectors to.
pub struct Target<'a> {
    pub skills: &'a mut Vec<String>,
    pub connectors: &'a mut Vec<String>,
    pub workspace: Option<&'a str>,
}

pub fn library_dir() -> Option<PathBuf> {
    codebot_core::default_skills_dir()
}

/// Resolve attached skill names against the current library and workspace.
pub fn resolve_skills(names: &[String], workspace: Option<&str>) -> Result<Vec<Skill>, String> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let library = library_dir();
    let sources = codebot_core::skill_sources(library.as_deref(), workspace.map(Path::new));
    let (available, _) = codebot_core::list_skills(&sources);
    names
        .iter()
        .map(|name| {
            available
                .iter()
                .find(|s| &s.name == name)
                .cloned()
                .ok_or_else(|| {
                    format!("スキル「{name}」が見つかりません。ライブラリを確認してください")
                })
        })
        .collect()
}

impl LibraryPanel {
    pub fn shutdown(&mut self) {
        self.test = None;
    }
    /// Load the skill list once per workspace (or again after `reload`).
    pub fn ensure_loaded(&mut self, workspace: Option<&str>) {
        let key = workspace.unwrap_or("").to_owned();
        if self.loaded_for.as_deref() != Some(key.as_str()) {
            self.reload(workspace);
        }
    }
    pub fn reload(&mut self, workspace: Option<&str>) {
        let library = library_dir();
        let sources = codebot_core::skill_sources(library.as_deref(), workspace.map(Path::new));
        (self.skills, self.problems) = codebot_core::list_skills(&sources);
        self.loaded_for = Some(workspace.unwrap_or("").to_owned());
    }
    pub fn poll(&mut self) {
        let ready = self
            .test
            .as_ref()
            .and_then(|task| match task.rx.try_recv() {
                Ok(result) => Some(Some(result)),
                Err(mpsc::TryRecvError::Disconnected) => Some(None),
                Err(mpsc::TryRecvError::Empty) => None,
            });
        if let Some(result) = ready {
            self.test = None;
            if let Some((name, result)) = result {
                self.tested.insert(name, result);
            }
        }
    }
    fn start_test(
        &mut self,
        ctx: &egui::Context,
        config: ConnectorConfig,
        workspace: Option<&str>,
    ) {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let cwd = workspace.map(PathBuf::from);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result =
                codebot_core::list_connector_tools(&config, cwd.as_deref(), &worker_cancel);
            let _ = tx.send((config.name, result));
            ctx.request_repaint();
        });
        self.test = Some(TestTask { rx, cancel });
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        connectors: &mut Vec<ConnectorConfig>,
        target: Target<'_>,
    ) -> Changes {
        let mut changes = Changes::default();
        self.ensure_loaded(target.workspace);
        ui.heading("ライブラリ");
        ui.add(
            egui::Label::new(
                RichText::new("スキルとコネクターを、このチャットに追加して使います。")
                    .color(crate::MUTED),
            )
            .wrap(),
        );
        ui.add_space(16.0);
        self.skills_ui(ui, target.skills, target.workspace, &mut changes);
        ui.add_space(24.0);
        self.connectors_ui(
            ui,
            connectors,
            target.connectors,
            target.workspace,
            &mut changes,
        );
        if let Some(message) = &self.message {
            ui.add_space(8.0);
            ui.add(egui::Label::new(message).wrap());
        }
        changes
    }

    fn skills_ui(
        &mut self,
        ui: &mut egui::Ui,
        attached: &mut Vec<String>,
        workspace: Option<&str>,
        changes: &mut Changes,
    ) {
        section_title(ui, "スキル");
        ui.add(
            egui::Label::new(
                RichText::new("SKILL.md（name・descriptionの前置きと手順）を読み込みます。追加したスキルの本文は、送信時に依頼の前へ付けてAIに渡します。ローカル診断では使いません。スクリプト等は実行しません。")
                    .size(13.0)
                    .color(crate::MUTED),
            )
            .wrap(),
        );
        ui.add_space(8.0);
        if self.skills.is_empty() {
            ui.label(
                RichText::new("スキルはまだありません。下のフォルダ指定から取り込めます。")
                    .color(crate::MUTED),
            );
        }
        let mut remove = None;
        for skill in &self.skills {
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(&skill.name)
                            .size(15.0)
                            .family(egui::FontFamily::Name("ui-bold".into())),
                    );
                    ui.label(
                        RichText::new(skill.source.label())
                            .size(12.0)
                            .color(crate::MUTED),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if skill.source == SkillSource::Library {
                            if self.remove_skill.as_deref() == Some(skill.name.as_str()) {
                                if ui.button("削除を確定").clicked() {
                                    remove = Some(skill.name.clone());
                                }
                            } else if ui.small_button("削除").clicked() {
                                self.remove_skill = Some(skill.name.clone());
                            }
                        }
                        let mut on = attached.contains(&skill.name);
                        if ui
                            .add(crate::checkbox::Checkbox::new(
                                &mut on,
                                "このチャットで使う",
                            ))
                            .changed()
                        {
                            if on && attached.len() >= codebot_core::MAX_ATTACHED_SKILLS {
                                self.message = Some(format!(
                                    "スキルは1つのチャットに{}件まで追加できます",
                                    codebot_core::MAX_ATTACHED_SKILLS
                                ));
                            } else if on {
                                attached.push(skill.name.clone());
                                changes.session = true;
                            } else {
                                attached.retain(|n| n != &skill.name);
                                changes.session = true;
                            }
                        }
                    });
                });
                ui.add(egui::Label::new(RichText::new(&skill.description).size(13.0)).wrap());
                ui.label(
                    RichText::new(format!(
                        "{}文字 · {}",
                        crate::context_bar::short(skill.request_chars()),
                        skill.path.display()
                    ))
                    .size(11.0)
                    .color(crate::MUTED),
                );
            });
        }
        if let Some(name) = remove {
            self.remove_skill = None;
            self.message = Some(match library_dir() {
                Some(dir) => match codebot_core::remove_skill(&dir, &name) {
                    Ok(()) => {
                        attached.retain(|n| n != &name);
                        changes.session = true;
                        format!("スキル「{name}」をライブラリから削除しました")
                    }
                    Err(error) => error,
                },
                None => "ライブラリの保存先を取得できません".into(),
            });
            self.reload(workspace);
        }
        let missing: Vec<String> = attached
            .iter()
            .filter(|n| !self.skills.iter().any(|s| &s.name == *n))
            .cloned()
            .collect();
        if !missing.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(
                    crate::ERROR,
                    format!("見つからないスキル：{}", missing.join("、")),
                );
                if ui.small_button("このチャットから外す").clicked() {
                    attached.retain(|n| !missing.contains(n));
                    changes.session = true;
                }
            });
        }
        ui.add_space(8.0);
        ui.label(RichText::new("スキルを取り込む（SKILL.mdのあるフォルダ）").size(13.0));
        let mut import = false;
        let mut reload = false;
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                reload = ui.button("再読み込み").clicked();
                import = ui.button("取り込む").clicked();
                ui.add(
                    crate::text_field(&mut self.import_path)
                        .hint_text(RichText::new("/path/to/my-skill").color(crate::MUTED))
                        .desired_width(ui.available_width()),
                );
            });
        });
        if import {
            self.message = Some(match library_dir() {
                Some(dir) => {
                    match codebot_core::import_skill(Path::new(self.import_path.trim()), &dir) {
                        Ok(skill) => {
                            self.import_path.clear();
                            format!("スキル「{}」をライブラリに取り込みました", skill.name)
                        }
                        Err(error) => error,
                    }
                }
                None => "ライブラリの保存先を取得できません".into(),
            });
        }
        if import || reload {
            self.reload(workspace);
        }
        if let Some(dir) = library_dir() {
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "取り込み先：{}　ほかに ~/.claude/skills と作業フォルダの .claude/skills も読み込みます。",
                        dir.display()
                    ))
                    .size(11.0)
                    .color(crate::MUTED),
                )
                .wrap(),
            );
        }
        if !self.problems.is_empty() {
            ui.collapsing(
                format!("読み込めなかったスキル（{}件）", self.problems.len()),
                |ui| {
                    for (path, error) in &self.problems {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("{}：{}", path.display(), error)).size(12.0),
                            )
                            .wrap(),
                        );
                    }
                },
            );
        }
    }

    fn connectors_ui(
        &mut self,
        ui: &mut egui::Ui,
        connectors: &mut Vec<ConnectorConfig>,
        attached: &mut Vec<String>,
        workspace: Option<&str>,
        changes: &mut Changes,
    ) {
        section_title(ui, "コネクター（MCP）");
        ui.add(
            egui::Label::new(
                RichText::new("ローカルのMCPサーバー（標準入出力）を登録します。追加したコネクターのツールは「実行 → AIに作業を相談」でAIに提示し、呼び出しごとに内容を確認してから実行します。起動時の環境変数はPATH等の基本項目と、指定した変数名だけを渡します。")
                    .size(13.0)
                    .color(crate::MUTED),
            )
            .wrap(),
        );
        ui.add_space(8.0);
        if connectors.is_empty() {
            ui.label(RichText::new("コネクターはまだありません。").color(crate::MUTED));
        }
        let busy = self.test.is_some();
        let mut remove = None;
        let mut test = None;
        for connector in connectors.iter() {
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(&connector.name)
                            .size(15.0)
                            .family(egui::FontFamily::Name("ui-bold".into())),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.remove_connector.as_deref() == Some(connector.name.as_str()) {
                            if ui.button("削除を確定").clicked() {
                                remove = Some(connector.name.clone());
                            }
                        } else if ui.small_button("削除").clicked() {
                            self.remove_connector = Some(connector.name.clone());
                        }
                        if ui
                            .add_enabled(!busy, egui::Button::new("接続テスト"))
                            .clicked()
                        {
                            test = Some(connector.clone());
                        }
                        let mut on = attached.contains(&connector.name);
                        if ui
                            .add(crate::checkbox::Checkbox::new(
                                &mut on,
                                "このチャットで使う",
                            ))
                            .changed()
                        {
                            if on && attached.len() >= codebot_core::MAX_ATTACHED_CONNECTORS {
                                self.message = Some(format!(
                                    "コネクターは1つのチャットに{}件まで追加できます",
                                    codebot_core::MAX_ATTACHED_CONNECTORS
                                ));
                            } else if on {
                                attached.push(connector.name.clone());
                                changes.session = true;
                            } else {
                                attached.retain(|n| n != &connector.name);
                                changes.session = true;
                            }
                        }
                    });
                });
                let mut line = connector.command.clone();
                for arg in &connector.args {
                    line.push(' ');
                    line.push_str(arg);
                }
                ui.add(egui::Label::new(RichText::new(line).monospace().size(12.0)).wrap());
                ui.label(
                    RichText::new(format!(
                        "追加の環境変数：{} · タイムアウト {}秒",
                        if connector.env_names.is_empty() {
                            "なし".into()
                        } else {
                            connector.env_names.join(", ")
                        },
                        connector.timeout_seconds
                    ))
                    .size(11.0)
                    .color(crate::MUTED),
                );
                match self.tested.get(&connector.name) {
                    Some(Ok(tools)) => {
                        ui.collapsing(format!("ツール {}件", tools.len()), |ui| {
                            for tool in tools {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!(
                                            "{} — {}",
                                            tool.name, tool.description
                                        ))
                                        .size(12.0),
                                    )
                                    .wrap(),
                                );
                            }
                        });
                    }
                    Some(Err(error)) => {
                        ui.colored_label(crate::ERROR, error);
                    }
                    None => {}
                }
            });
        }
        if busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("コネクターに接続しています…");
                if ui.small_button("中止").clicked() {
                    if let Some(task) = &self.test {
                        task.cancel.store(true, Ordering::Relaxed);
                    }
                }
            });
        }
        if let Some(config) = test {
            self.tested.remove(&config.name);
            self.start_test(ui.ctx(), config, workspace);
        }
        if let Some(name) = remove {
            connectors.retain(|c| c.name != name);
            attached.retain(|n| n != &name);
            self.tested.remove(&name);
            self.remove_connector = None;
            changes.preferences = true;
            changes.session = true;
        }

        ui.add_space(8.0);
        ui.collapsing("コネクターを登録", |ui| {
            if self.form_timeout == 0 {
                self.form_timeout = 30;
            }
            ui.label(RichText::new("名前（英小文字・数字・-・_）").size(13.0));
            ui.add(crate::text_field(&mut self.form_name).desired_width(f32::INFINITY));
            ui.label(RichText::new("実行ファイル").size(13.0));
            ui.add(
                crate::text_field(&mut self.form_command)
                    .hint_text(RichText::new("npx / uvx / /path/to/server").color(crate::MUTED))
                    .desired_width(f32::INFINITY),
            );
            ui.label(RichText::new("引数（1行に1つ）").size(13.0));
            ui.add(
                crate::text_area(&mut self.form_args)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
            ui.label(
                RichText::new("渡す環境変数名（空白またはカンマ区切り。値は保存しません）")
                    .size(13.0),
            );
            ui.add(crate::text_field(&mut self.form_env).desired_width(f32::INFINITY));
            ui.horizontal(|ui| {
                ui.label(RichText::new("タイムアウト（秒）").size(13.0));
                ui.add(egui::DragValue::new(&mut self.form_timeout).range(1..=300));
            });
            if ui.button("登録").clicked() {
                let config = ConnectorConfig {
                    name: self.form_name.trim().to_owned(),
                    command: self.form_command.trim().to_owned(),
                    args: self
                        .form_args
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(str::to_owned)
                        .collect(),
                    env_names: self
                        .form_env
                        .split(|c: char| c == ',' || c.is_whitespace())
                        .filter(|n| !n.is_empty())
                        .map(str::to_owned)
                        .collect(),
                    timeout_seconds: self.form_timeout,
                };
                self.message = Some(match config.validate() {
                    Err(error) => error,
                    Ok(()) if connectors.iter().any(|c| c.name == config.name) => {
                        "同じ名前のコネクターがあります".into()
                    }
                    Ok(()) if connectors.len() >= 32 => "コネクターは32件まで登録できます".into(),
                    Ok(()) => {
                        let name = config.name.clone();
                        connectors.push(config);
                        changes.preferences = true;
                        self.form_name.clear();
                        self.form_command.clear();
                        self.form_args.clear();
                        self.form_env.clear();
                        format!(
                            "コネクター「{name}」を登録しました。接続テストでツールを確認できます"
                        )
                    }
                });
            }
        });
    }
}

fn section_title(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(16.0)
            .family(egui::FontFamily::Name("ui-bold".into())),
    );
    ui.add_space(4.0);
}

/// Material outlined card.
fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0_f32, crate::BORDER))
        .corner_radius(12.0)
        .inner_margin(egui::Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
    ui.add_space(8.0);
}
