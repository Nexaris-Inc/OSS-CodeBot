use std::process::Command;

fn workspace_with_skill() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let skill = dir.path().join(".claude/skills/terse-review");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: terse-review\ndescription: Review in three bullet points\n---\nUse exactly three bullet points.",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        "provider = \"openai\"\n[api]\nmodel = \"m\"\nbase_url = \"https://api.openai.com/v1\"\n\n[[connectors]]\nname = \"notes\"\ncommand = \"notes-server\"\nargs = [\"--stdio\"]\n",
    )
    .unwrap();
    dir
}

fn cli(dir: &tempfile::TempDir) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_codebot-cli"));
    // Keep the user's own skill folders out of the test.
    command
        .env("HOME", dir.path().join("home"))
        .env("USERPROFILE", dir.path().join("home"))
        .env("XDG_CONFIG_HOME", dir.path().join("home/config"))
        .env("APPDATA", dir.path().join("home/appdata"));
    command
}

#[test]
fn list_skills_shows_workspace_skills_and_config_connectors() {
    let dir = workspace_with_skill();
    let out = cli(&dir)
        .arg("--config")
        .arg(dir.path().join("config.toml"))
        .arg("--workspace")
        .arg(dir.path())
        .arg("--list-skills")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("terse-review"), "{stdout}");
    assert!(stdout.contains("notes  notes-server --stdio"), "{stdout}");
}

#[test]
fn preview_includes_skill_instructions_and_missing_skills_fail_before_sending() {
    let dir = workspace_with_skill();
    let out = cli(&dir)
        .arg("--config")
        .arg(dir.path().join("config.toml"))
        .arg("--workspace")
        .arg(dir.path())
        .args(["--skill", "terse-review", "--preview", "review this"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Use exactly three bullet points."));
    assert!(stdout.trim_end().ends_with("review this"));

    let out = cli(&dir)
        .arg("--config")
        .arg(dir.path().join("config.toml"))
        .args(["--skill", "not-installed", "--preview", "review this"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not-installed"));
}

#[test]
fn duplicate_or_invalid_connectors_in_the_config_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "provider = \"openai\"\n[[connectors]]\nname = \"a\"\ncommand = \"x\"\n[[connectors]]\nname = \"a\"\ncommand = \"y\"\n",
    )
    .unwrap();
    let out = cli(&dir)
        .arg("--config")
        .arg(&path)
        .arg("--list-skills")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("重複"));
}
