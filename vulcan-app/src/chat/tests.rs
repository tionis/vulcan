use super::*;
use crate::execution::Ungated;
use std::fs;
use tempfile::{tempdir, TempDir};
use vulcan_core::{
    initialize_vulcan_dir, resolve_permission_profile, scan_vault_with_progress, ScanMode,
};

#[test]
fn parses_text_commands_and_bot_suffixes() {
    assert_eq!(
        parse_chat_message("  buy milk "),
        ChatCommand::Capture {
            text: "buy milk".to_string()
        }
    );
    assert_eq!(parse_chat_message(""), ChatCommand::Empty);
    assert_eq!(
        parse_chat_message("/task@VulcanBot call Anna tomorrow"),
        ChatCommand::Task {
            text: "call Anna tomorrow".to_string()
        }
    );
    assert_eq!(
        parse_chat_message("/TODAY"),
        ChatCommand::Today,
        "commands are case-insensitive"
    );
    assert_eq!(parse_chat_message("/due"), ChatCommand::Due { days: 7 });
    assert_eq!(parse_chat_message("/due 3d"), ChatCommand::Due { days: 3 });
    assert_eq!(
        parse_chat_message("/due soon"),
        ChatCommand::Usage { command: "due" }
    );
    assert_eq!(
        parse_chat_message("/reminders"),
        ChatCommand::Reminders {
            window: "24h".to_string()
        }
    );
    assert_eq!(
        parse_chat_message("/task"),
        ChatCommand::Usage { command: "task" }
    );
    assert_eq!(
        parse_chat_message("/vault rpg"),
        ChatCommand::Vault {
            name: Some("rpg".to_string())
        }
    );
    assert_eq!(parse_chat_message("/start"), ChatCommand::Help);
    assert_eq!(
        parse_chat_message("/frobnicate now"),
        ChatCommand::Unknown {
            command: "frobnicate".to_string()
        }
    );
    assert!(parse_chat_message("note to self").writes());
    assert!(!parse_chat_message("/find x").writes());
}

#[test]
fn help_lists_every_command() {
    let help = chat_help_text();
    for (command, _) in CHAT_COMMANDS {
        assert!(help.contains(&format!("/{command} ")), "{help}");
    }
}

struct Fixture {
    _temp: TempDir,
    paths: VaultPaths,
}

impl Fixture {
    fn new(config: &str, notes: &[(&str, &str)]) -> Self {
        let temp = tempdir().unwrap();
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).unwrap();
        fs::write(paths.config_file(), config).unwrap();
        for (path, content) in notes {
            let file = temp.path().join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, content).unwrap();
        }
        scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).unwrap();
        Self { _temp: temp, paths }
    }

    fn guard(&self, profile: &str) -> ProfilePermissionGuard {
        ProfilePermissionGuard::new(
            &self.paths,
            resolve_permission_profile(&self.paths, Some(profile)).unwrap(),
        )
    }

    fn run(&self, profile: &str, message: &str) -> Result<ChatReply, AppError> {
        let guard = self.guard(profile);
        let vault = ChatVault {
            paths: &self.paths,
            guard: &guard,
            profile,
            gate: &Ungated,
            today: NaiveDate::from_ymd_opt(2026, 11, 7).unwrap(),
        };
        run_chat_command(&vault, &parse_chat_message(message))
    }

    fn act(&self, action: &ChatAction) -> Result<ChatReply, AppError> {
        let guard = self.guard("unrestricted");
        let vault = ChatVault {
            paths: &self.paths,
            guard: &guard,
            profile: "unrestricted",
            gate: &Ungated,
            today: NaiveDate::from_ymd_opt(2026, 11, 7).unwrap(),
        };
        run_chat_action(&vault, action)
    }

    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.paths.vault_root().join(path)).unwrap()
    }
}

#[test]
fn free_text_is_captured_to_the_inbox_and_indexed() {
    let fixture = Fixture::new(
        "[inbox]\npath = \"Inbox.md\"\nformat = \"- {text}\"\ntimestamp = false\n",
        &[],
    );
    let reply = fixture.run("unrestricted", "call the plumber").unwrap();
    assert_eq!(reply.text, "Captured to Inbox.md");
    assert_eq!(reply.changed_paths, vec!["Inbox.md".to_string()]);
    assert!(fixture.read("Inbox.md").contains("- call the plumber"));

    // The capture is indexed, so a search finds it immediately.
    let reply = fixture.run("unrestricted", "/find plumber").unwrap();
    assert!(reply.text.contains("Inbox.md"), "{}", reply.text);
    assert_eq!(
        reply.buttons[0].action,
        ChatAction::ShowNote {
            path: "Inbox.md".to_string()
        }
    );
}

#[test]
fn captures_go_to_the_daily_note_when_configured() {
    let fixture = Fixture::new(
        "[bot]\ndefault_capture = \"daily\"\n[periodic.daily]\nfolder = \"Journal\"\n",
        &[],
    );
    let reply = fixture.run("unrestricted", "met Anna for lunch").unwrap();
    assert_eq!(
        reply.changed_paths,
        vec!["Journal/2026-11-07.md".to_string()]
    );
    assert!(fixture
        .read("Journal/2026-11-07.md")
        .contains("met Anna for lunch"));
}

#[test]
fn writes_are_refused_outside_the_profile() {
    let fixture = Fixture::new("[inbox]\npath = \"Inbox.md\"\n", &[]);
    let error = fixture.run("readonly", "sneaky capture").unwrap_err();
    assert!(
        error.to_string().to_lowercase().contains("permission")
            || error.to_string().contains("denied"),
        "{error}"
    );
    assert!(!fixture.paths.vault_root().join("Inbox.md").exists());
}

#[test]
fn inline_task_target_adds_a_task_to_the_inbox() {
    let fixture = Fixture::new(
        "[bot]\ntask_target = \"inline\"\n[inbox]\npath = \"Inbox.md\"\n",
        &[],
    );
    let reply = fixture
        .run("unrestricted", "/task water the plants")
        .unwrap();
    assert!(reply.text.starts_with("Task added: "), "{}", reply.text);
    assert!(fixture.read("Inbox.md").contains("- [ ] water the plants"));
}

#[test]
fn tasknotes_target_creates_a_task_note() {
    let fixture = Fixture::new("", &[]);
    let reply = fixture.run("unrestricted", "/task Renew passport").unwrap();
    assert!(
        reply.text.starts_with("Task: Renew passport"),
        "{}",
        reply.text
    );
    assert_eq!(reply.changed_paths.len(), 1);
    assert!(fixture
        .read(&reply.changed_paths[0])
        .contains("Renew passport"));
}

#[test]
fn today_lists_due_tasks_and_done_buttons_complete_them() {
    let fixture = Fixture::new(
        "",
        &[(
            "Projects/Alpha.md",
            "# Alpha\n\n\
             - [ ] Overdue thing 📅 2026-11-01\n\
             - [ ] Today thing ⏳ 2026-11-07\n\
             - [ ] Later thing 📅 2026-11-12\n\
             - [x] Done thing 📅 2026-11-07\n",
        )],
    );
    let reply = fixture.run("unrestricted", "/today").unwrap();
    assert!(reply.text.starts_with("Due today (2):"), "{}", reply.text);
    assert!(reply.text.contains("- Overdue thing 2026-11-01 (overdue)"));
    assert!(reply.text.contains("- Today thing 2026-11-07"));
    assert!(!reply.text.contains("Later thing"));
    assert_eq!(reply.buttons.len(), 2);

    let week = fixture.run("unrestricted", "/due 7").unwrap();
    assert!(week.text.contains("Later thing"), "{}", week.text);

    let overdue = reply
        .buttons
        .iter()
        .find(|button| button.label.contains("Overdue thing"))
        .unwrap();
    let done = fixture.act(&overdue.action).unwrap();
    assert_eq!(done.text, "Done: Overdue thing");
    assert!(fixture
        .read("Projects/Alpha.md")
        .contains("- [x] Overdue thing 📅 2026-11-01 ✅ 2026-11-07"));
    let reply = fixture.run("unrestricted", "/today").unwrap();
    assert!(reply.text.starts_with("Due today (1):"), "{}", reply.text);
}

#[test]
fn notes_are_shown_inline_or_as_documents_when_long() {
    let long = format!("# Long\n\n{}\n", "word ".repeat(1000));
    let fixture = Fixture::new(
        "",
        &[
            ("Short.md", "# Short\n\nHello there.\n"),
            ("Long.md", long.as_str()),
        ],
    );
    let reply = fixture.run("readonly", "/note Short").unwrap();
    assert!(
        reply.text.starts_with("Short.md\n\n# Short"),
        "{}",
        reply.text
    );
    assert_eq!(reply.document, None);

    let reply = fixture.run("readonly", "/note Long.md").unwrap();
    let document = reply.document.expect("long note is sent as a document");
    assert_eq!(document.file_name, "Long.md");
    assert_eq!(document.content, long);
}

#[test]
fn reminders_lists_upcoming_inline_reminders() {
    let fixture = Fixture::new(
        "[tasks.reminders]\ntimezone = \"UTC\"\n",
        &[("Calls.md", "- [ ] Call the bank ⏰ 2999-01-01 10:00\n")],
    );
    let reply = fixture.run("readonly", "/reminders 2000y").unwrap();
    assert!(
        reply.text.contains("2999-01-01T10:00:00Z Call the bank"),
        "{}",
        reply.text
    );
    let reply = fixture.run("readonly", "/reminders 1h").unwrap();
    assert_eq!(reply.text, "No reminders within 1h.");
}
