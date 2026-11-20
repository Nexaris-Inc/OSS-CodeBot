//! Skill library. A skill is a folder holding a `SKILL.md` file with YAML-style
//! front matter (`name`, `description`) and Markdown instructions, the same layout
//! Claude Code uses. Skills are text only: attaching one adds its instructions to
//! the request. Nothing in a skill folder is executed.
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Maximum size of a `SKILL.md` file that is read.
pub const MAX_SKILL_FILE_BYTES: u64 = 64 * 1024;
/// Maximum characters of instructions per skill.
pub const MAX_SKILL_BODY_CHARS: usize = 20_000;
/// Maximum number of skills attached to one chat.
pub const MAX_ATTACHED_SKILLS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum SkillSource {
    /// The application's own library folder (imported skills).
    Library,
    /// Claude Code user skills: `~/.claude/skills`.
    User,
    /// Claude Code project skills: `<workspace>/.claude/skills`.
    Workspace,
}
impl SkillSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Library => "ライブラリ",
            Self::User => "ユーザー（~/.claude/skills）",
            Self::Workspace => "作業フォルダ（.claude/skills）",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
    pub path: PathBuf,
    pub source: SkillSource,
}
impl Skill {
    /// Characters this skill adds to a request.
    pub fn request_chars(&self) -> usize {
        skill_block(self).chars().count()
    }
}

/// The app library folder: `<OS config dir>/skills`.
pub fn default_skills_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("org", "OSS-CodeBot", "OSS-CodeBot")
        .map(|dirs| dirs.config_dir().join("skills"))
}

/// Claude Code user skill folder: `~/.claude/skills`.
pub fn user_skills_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".claude").join("skills"))
}

/// Skill folders to scan, in priority order. Earlier sources win on name conflicts.
pub fn skill_sources(
    library: Option<&Path>,
    workspace: Option<&Path>,
) -> Vec<(SkillSource, PathBuf)> {
    let mut sources = Vec::new();
    if let Some(dir) = library {
        sources.push((SkillSource::Library, dir.to_path_buf()));
    }
    if let Some(root) = workspace.filter(|r| !r.as_os_str().is_empty()) {
        sources.push((SkillSource::Workspace, root.join(".claude").join("skills")));
    }
    if let Some(dir) = user_skills_dir() {
        sources.push((SkillSource::User, dir));
    }
    sources
}

/// Parse a `SKILL.md` document into (name, description, body).
pub fn parse_skill(text: &str) -> Result<(String, String, String), String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return Err("SKILL.mdの先頭に「---」で囲んだname・descriptionがありません".into());
    }
    let mut front = Vec::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line.trim_end() == "---" {
            closed = true;
            break;
        }
        front.push(line);
    }
    if !closed {
        return Err("SKILL.mdの前置き（---）が閉じていません".into());
    }
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_owned();
    let (mut name, mut description) = (None, None);
    let mut index = 0;
    while index < front.len() {
        let line = front[index];
        index += 1;
        if line.starts_with([' ', '\t']) || line.trim_start().starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let mut value = value.trim().to_owned();
        // Block scalars (`>` / `|`) and empty values continue on indented lines.
        if value.is_empty() || value.starts_with('>') || value.starts_with('|') {
            let literal = value.starts_with('|');
            let mut parts = Vec::new();
            while index < front.len()
                && (front[index].starts_with([' ', '\t']) || front[index].trim().is_empty())
            {
                parts.push(front[index].trim());
                index += 1;
            }
            value = parts
                .join(if literal { "\n" } else { " " })
                .trim()
                .to_owned();
        } else {
            value = unquote(&value);
        }
        match key.trim() {
            "name" => name = Some(value),
            "description" => description = Some(value),
            _ => {}
        }
    }
    let name = name.ok_or("SKILL.mdにnameがありません")?;
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err("スキル名は64文字以内の英小文字・数字・ハイフンにしてください".into());
    }
    let description = description.ok_or("SKILL.mdにdescriptionがありません")?;
    if description.trim().is_empty() || description.chars().count() > 1024 {
        return Err("descriptionは1〜1024文字にしてください".into());
    }
    if body.is_empty() {
        return Err("SKILL.mdに指示の本文がありません".into());
    }
    if body.chars().count() > MAX_SKILL_BODY_CHARS {
        return Err(format!(
            "スキルの本文が{MAX_SKILL_BODY_CHARS}文字を超えています"
        ));
    }
    if text.contains('\0') {
        return Err("SKILL.mdに不正な文字が含まれています".into());
    }
    Ok((name, description, body))
}

fn unquote(value: &str) -> String {
    for quote in ['"', '\''] {
        if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
            return value[1..value.len() - 1].to_owned();
        }
    }
    value.to_owned()
}

/// Read one skill folder. Symbolic links are not followed.
pub fn read_skill(dir: &Path, source: SkillSource) -> Result<Skill, String> {
    let path = dir.join("SKILL.md");
    let metadata = fs::symlink_metadata(&path).map_err(|_| "SKILL.mdがありません")?;
    if !metadata.is_file() {
        return Err("SKILL.mdが通常のファイルではありません".into());
    }
    if metadata.len() > MAX_SKILL_FILE_BYTES {
        return Err("SKILL.mdが64 KiBを超えています".into());
    }
    let text = fs::read_to_string(&path).map_err(|_| "SKILL.mdをUTF-8として読み込めません")?;
    let (name, description, body) = parse_skill(&text)?;
    Ok(Skill {
        name,
        description,
        body,
        path,
        source,
    })
}

/// List skills from the given sources. Missing folders are skipped; invalid skill
/// folders are reported as `(folder, reason)` without stopping the listing.
pub fn list_skills(sources: &[(SkillSource, PathBuf)]) -> (Vec<Skill>, Vec<(PathBuf, String)>) {
    let mut skills: Vec<Skill> = Vec::new();
    let mut problems = Vec::new();
    for (source, dir) in sources {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        let mut folders: Vec<PathBuf> = entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
            .map(|entry| entry.path())
            .collect();
        folders.sort();
        for folder in folders.into_iter().take(256) {
            match read_skill(&folder, *source) {
                Ok(skill) if skills.iter().any(|s| s.name == skill.name) => problems.push((
                    folder,
                    format!(
                        "同じ名前のスキル「{}」が先に見つかったため無視しました",
                        skill.name
                    ),
                )),
                Ok(skill) => skills.push(skill),
                Err(error) => problems.push((folder, error)),
            }
        }
    }
    (skills, problems)
}

/// Copy a skill's `SKILL.md` into the library as `<library>/<name>/SKILL.md`.
/// Only the instructions are imported; scripts and other files are not copied.
pub fn import_skill(source: &Path, library: &Path) -> Result<Skill, String> {
    let dir = if source.file_name().is_some_and(|n| n == "SKILL.md") {
        source.parent().unwrap_or(source)
    } else {
        source
    };
    let skill = read_skill(dir, SkillSource::Library)?;
    let target = library.join(&skill.name);
    if fs::symlink_metadata(&target).is_ok() {
        return Err(format!(
            "ライブラリに「{}」が既にあります。削除してから取り込んでください",
            skill.name
        ));
    }
    fs::create_dir_all(&target).map_err(|_| "ライブラリのフォルダを作成できません")?;
    let text = fs::read(&skill.path).map_err(|_| "SKILL.mdを読み込めません")?;
    fs::write(target.join("SKILL.md"), text).map_err(|_| "スキルを保存できません")?;
    read_skill(&target, SkillSource::Library)
}

/// Remove a skill that lives in the app library.
pub fn remove_skill(library: &Path, name: &str) -> Result<(), String> {
    let skill = read_skill(&library.join(name), SkillSource::Library)?;
    if skill.name != name {
        return Err("削除するスキルの名前が一致しません".into());
    }
    fs::remove_file(&skill.path).map_err(|_| "SKILL.mdを削除できません")?;
    fs::remove_dir(library.join(name))
        .map_err(|_| "SKILL.md以外のファイルがあるためフォルダを残しました".into())
}

fn skill_block(skill: &Skill) -> String {
    format!(
        "\n--- スキル: {} ---\n説明: {}\n{}\n--- スキルここまで ---\n",
        skill.name, skill.description, skill.body
    )
}

/// Instructions added in front of the request for the attached skills.
pub fn skill_preamble(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut text = String::from(
        "以下は利用者がこの会話に追加したスキル（作業手順の指示）です。該当する場面ではこの指示に従って回答してください。\n",
    );
    for skill in skills {
        text.push_str(&skill_block(skill));
    }
    text.push_str("\n--- 利用者の依頼 ---\n");
    text
}

/// Add the skills' instructions in front of the first request message. The
/// input limit is widened by the instructions' size; the whole request stays
/// within `max_context_chars`.
pub fn prepend_skills(
    config: &crate::Config,
    messages: &mut [crate::ChatMessage],
    skills: &[Skill],
) -> Result<(), String> {
    let preamble = skill_preamble(skills);
    if preamble.is_empty() || messages.is_empty() {
        return Ok(());
    }
    messages[0].content.insert_str(0, &preamble);
    let mut limits = config.clone();
    limits.max_input_chars = limits
        .max_input_chars
        .saturating_add(preamble.chars().count());
    crate::validate_messages(&limits, messages)
        .map_err(|e| format!("スキルを含めると送信できません：{e}"))
}

/// Resolve skill names against the app library, the workspace and `~/.claude/skills`.
pub fn resolve_skills(names: &[String], workspace: Option<&Path>) -> Result<Vec<Skill>, String> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let library = default_skills_dir();
    let (available, _) = list_skills(&skill_sources(library.as_deref(), workspace));
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

#[cfg(test)]
mod tests {
    use super::*;

    const SKILL: &str = "---\nname: rust-review\ndescription: \"Rustコードをレビューする\"\nallowed-tools: Read\n---\n\n# 手順\n\n1. unsafeを確認する\n";

    #[test]
    fn parses_claude_code_front_matter_and_ignores_unknown_keys() {
        let (name, description, body) = parse_skill(SKILL).unwrap();
        assert_eq!(name, "rust-review");
        assert_eq!(description, "Rustコードをレビューする");
        assert!(body.starts_with("# 手順"));
        let folded = "---\nname: a\ndescription: >\n  first\n  second\n---\nbody";
        assert_eq!(parse_skill(folded).unwrap().1, "first second");
    }

    #[test]
    fn rejects_missing_or_invalid_fields() {
        assert!(parse_skill("no front matter").is_err());
        assert!(parse_skill("---\nname: a\n").is_err());
        assert!(parse_skill("---\ndescription: d\n---\nbody").is_err());
        assert!(parse_skill("---\nname: Bad Name\ndescription: d\n---\nbody").is_err());
        assert!(parse_skill("---\nname: a\ndescription: d\n---\n").is_err());
        let long = format!(
            "---\nname: a\ndescription: d\n---\n{}",
            "x".repeat(MAX_SKILL_BODY_CHARS + 1)
        );
        assert!(parse_skill(&long).is_err());
    }

    #[test]
    fn lists_imports_and_removes_with_priority_and_problems() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("library");
        let external = dir.path().join("external");
        fs::create_dir_all(external.join("rust-review")).unwrap();
        fs::write(external.join("rust-review/SKILL.md"), SKILL).unwrap();
        fs::create_dir_all(external.join("broken")).unwrap();
        fs::write(external.join("broken/SKILL.md"), "oops").unwrap();
        fs::write(external.join("rust-review/run.sh"), "echo no").unwrap();

        let imported = import_skill(&external.join("rust-review"), &library).unwrap();
        assert_eq!(imported.source, SkillSource::Library);
        assert!(!library.join("rust-review/run.sh").exists());
        assert!(import_skill(&external.join("rust-review/SKILL.md"), &library).is_err());

        let sources = vec![
            (SkillSource::Library, library.clone()),
            (SkillSource::Workspace, external.clone()),
            (SkillSource::User, dir.path().join("missing")),
        ];
        let (skills, problems) = list_skills(&sources);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].source, SkillSource::Library);
        assert_eq!(problems.len(), 2);

        let preamble = skill_preamble(&skills);
        assert!(preamble.contains("--- スキル: rust-review ---"));
        assert!(preamble.ends_with("--- 利用者の依頼 ---\n"));
        assert!(skills[0].request_chars() < preamble.chars().count());

        remove_skill(&library, "rust-review").unwrap();
        assert!(!library.join("rust-review").exists());
    }
}
