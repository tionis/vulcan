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
    assert_eq!(
        reply.text,
        "Reminders in the next 2000y:\n\nTue 1 Jan 2999\n• 10:00 Call the bank"
    );
    let reply = fixture.run("readonly", "/reminders 1h").unwrap();
    assert_eq!(reply.text, "No reminders in the next 1h.");
}

#[test]
fn reminders_are_grouped_by_local_day() {
    // The fixture's today is 2026-11-07; Berlin is UTC+1 in November.
    let fixture = Fixture::new(
        "[tasks.reminders]\ntimezone = \"Europe/Berlin\"\n",
        &[(
            "Calls.md",
            "- [ ] Call the bank ⏰ 2999-01-01 10:00\n- [ ] Pay rent ⏰ 2999-01-01 08:15\n\
             - [ ] Water plants ⏰ 2999-01-02 23:30\n",
        )],
    );
    let reply = fixture.run("readonly", "/reminders 2000y").unwrap();
    assert_eq!(
        reply.text,
        "Reminders in the next 2000y:\n\n\
         Tue 1 Jan 2999\n• 08:15 Pay rent\n• 10:00 Call the bank\n\n\
         Wed 2 Jan 2999\n• 23:30 Water plants"
    );
}

#[test]
fn day_headings_name_nearby_days() {
    let today = NaiveDate::from_ymd_opt(2026, 11, 7).unwrap();
    let day = |offset: i64| super::day_heading(today + chrono::Duration::days(offset), today);
    assert_eq!(day(-1), "Yesterday, Fri 6 Nov");
    assert_eq!(day(0), "Today, Sat 7 Nov");
    assert_eq!(day(1), "Tomorrow, Sun 8 Nov");
    assert_eq!(day(3), "Tue 10 Nov");
    assert_eq!(day(60), "Wed 6 Jan 2027");
}

#[test]
fn reminder_plans_render_in_the_vault_timezone_with_path_privacy() {
    let fixture = Fixture::new(
        "[tasks.reminders]\ntimezone = \"Europe/Berlin\"\n\
         [bot.reminders]\ncatch_up_window = \"2h\"\n\
         [[bot.reminders.privacy_overrides]]\nglob = \"GM/**\"\nprivacy = \"title_only\"\n",
        &[
            (
                "Calls.md",
                "- [ ] Call the bank ⏰ 2026-01-01 10:00\n- [x] Done ⏰ 2026-01-01 09:00\n",
            ),
            (
                "GM/Secrets.md",
                "- [ ] Prepare the twist ⏰ 2026-01-02 11:00\n",
            ),
        ],
    );
    let guard = fixture.guard("readonly");
    let vault = ChatVault {
        paths: &fixture.paths,
        guard: &guard,
        profile: "readonly",
        gate: &Ungated,
        today: NaiveDate::from_ymd_opt(2026, 11, 7).unwrap(),
    };
    let plan = reminders::plan_chat_reminders(&vault).unwrap();
    assert_eq!(plan.catch_up_ms, 2 * 60 * 60 * 1_000);
    assert_eq!(plan.reminders.len(), 2, "{plan:?}");
    let call = &plan.reminders[0];
    assert_eq!(call.path, "Calls.md");
    assert_eq!(call.task, "Calls.md:1");
    assert_eq!(
        call.message(false),
        "⏰ Call the bank\nCalls.md\n2026-01-01 10:00",
        "10:00 Berlin, shown as written"
    );
    assert_eq!(
        plan.reminders[1].message(true),
        "⏰ Prepare the twist\n2026-01-02 11:00 (late)",
        "title_only hides the GM path"
    );

    fs::write(
        fixture.paths.config_file(),
        "[bot.reminders]\ncatch_up_window = \"soon\"\n",
    )
    .unwrap();
    let error = reminders::plan_chat_reminders(&vault).unwrap_err();
    assert!(error.to_string().contains("catch_up_window"), "{error}");
}

fn vault_for<'a>(
    fixture: &'a Fixture,
    guard: &'a ProfilePermissionGuard,
    profile: &'a str,
) -> ChatVault<'a> {
    ChatVault {
        paths: &fixture.paths,
        guard,
        profile,
        gate: &Ungated,
        today: chrono::Utc::now().date_naive(),
    }
}

#[test]
fn reminder_buttons_complete_and_snooze_inline_tasks() {
    let fixture = Fixture::new(
        "[tasks.reminders]\ntimezone = \"UTC\"\n",
        &[(
            "Calls.md",
            "- [ ] Call the bank ⏰ 2026-01-01 10:00\n- [ ] Water plants ⏰ 2026-01-01 11:00\n- [ ] Pay rent [remind:: 2026-01-01T12:00]\n",
        )],
    );
    let readonly = fixture.guard("readonly");
    let plan = reminders::plan_chat_reminders(&vault_for(&fixture, &readonly, "readonly")).unwrap();
    assert!(
        plan.reminders.iter().all(|r| r.actions.is_empty()),
        "no buttons without write access"
    );

    let guard = fixture.guard("unrestricted");
    let vault = vault_for(&fixture, &guard, "unrestricted");
    let plan = reminders::plan_chat_reminders(&vault).unwrap();
    assert_eq!(plan.reminders[0].actions, vec!["done", "1h", "tomorrow"]);
    let [bank, plants, rent] = [0, 1, 2].map(|i| plan.reminders[i].occurrence_id.clone());

    let now = chrono::Utc::now().timestamp_millis();
    let snoozed = reminders::run_reminder_action(&vault, &bank, "1h", now).unwrap();
    let at = now + 60 * 60 * 1_000;
    let at = at + (60_000 - at % 60_000) % 60_000;
    let expected = chrono::DateTime::from_timestamp_millis(at)
        .unwrap()
        .format("%Y-%m-%d %H:%M")
        .to_string();
    assert_eq!(snoozed, format!("Snoozed until {expected}."));
    assert!(fixture
        .read("Calls.md")
        .contains(&format!("- [ ] Call the bank ⏰ {expected}")));
    assert_eq!(
        reminders::run_reminder_action(&vault, &bank, "1h", now).unwrap(),
        "This task already changed; nothing was done.",
        "a second press finds the old occurrence gone"
    );

    let tomorrow = (chrono::Utc::now().date_naive() + chrono::Duration::days(1)).format("%Y-%m-%d");
    reminders::run_reminder_action(&vault, &rent, "tomorrow", now).unwrap();
    assert!(
        fixture
            .read("Calls.md")
            .contains(&format!("- [ ] Pay rent ⏰ {tomorrow} 09:00")),
        "{}",
        fixture.read("Calls.md")
    );

    assert_eq!(
        reminders::run_reminder_action(&vault, &plants, "done", now).unwrap(),
        "Done."
    );
    assert!(fixture.read("Calls.md").contains("- [x] Water plants"));

    fs::write(
        fixture.paths.config_file(),
        "[bot.reminders]\nsnooze_options = [\"later\"]\n",
    )
    .unwrap();
    let error = reminders::plan_chat_reminders(&vault).unwrap_err();
    assert!(error.to_string().contains("snooze_options"), "{error}");
}

#[test]
fn snoozing_a_task_note_adds_an_absolute_reminder() {
    let fixture = Fixture::new("[tasks.reminders]\ntimezone = \"UTC\"\n", &[]);
    let path = fixture
        .run("unrestricted", "/task Renew passport")
        .unwrap()
        .changed_paths
        .remove(0);
    crate::tasks::apply_task_reminder_add_with_guard(
        &fixture.paths,
        &crate::tasks::TaskReminderAddRequest {
            task: path.clone(),
            spec: "2026-01-01T10:00".to_string(),
            description: None,
            dry_run: false,
        },
        None,
    )
    .unwrap();
    let guard = fixture.guard("unrestricted");
    let vault = vault_for(&fixture, &guard, "unrestricted");
    let plan = reminders::plan_chat_reminders(&vault).unwrap();
    let occurrence = plan
        .reminders
        .iter()
        .find(|r| r.path == path)
        .expect("task note reminder planned")
        .occurrence_id
        .clone();
    let outcome = reminders::run_reminder_action(
        &vault,
        &occurrence,
        "tomorrow",
        chrono::Utc::now().timestamp_millis(),
    )
    .unwrap();
    assert!(outcome.starts_with("Snoozed until "), "{outcome}");
    let note = fixture.read(&path);
    assert!(note.contains("Snoozed"), "{note}");
    let plan = reminders::plan_chat_reminders(&vault).unwrap();
    assert_eq!(
        plan.reminders.iter().filter(|r| r.path == path).count(),
        2,
        "the original and the snoozed reminder"
    );
}

#[test]
fn undo_reverts_the_last_capture_only_while_unchanged() {
    let fixture = Fixture::new(
        "[inbox]\npath = \"Inbox.md\"\nformat = \"- {text}\"\ntimestamp = false\n",
        &[("Inbox.md", "# Inbox\n")],
    );
    let guard = fixture.guard("unrestricted");
    let vault = vault_for(&fixture, &guard, "unrestricted");
    assert_eq!(parse_chat_message("/undo"), ChatCommand::Undo);

    let first = fixture.run("unrestricted", "first thought").unwrap();
    assert_eq!(first.undo.len(), 1);
    assert_eq!(first.undo[0].before.as_deref(), Some("# Inbox\n"));
    let reply = run_chat_undo(&vault, &first.undo).unwrap();
    assert_eq!(reply.text, "Undone: Inbox.md");
    assert_eq!(fixture.read("Inbox.md"), "# Inbox\n");
    assert!(run_chat_undo(&vault, &first.undo)
        .unwrap()
        .text
        .contains("changed since the capture"));

    // An edit after the capture blocks the undo.
    let second = fixture.run("unrestricted", "second thought").unwrap();
    fs::write(
        fixture.paths.vault_root().join("Inbox.md"),
        format!("{}- edited by hand\n", fixture.read("Inbox.md")),
    )
    .unwrap();
    let reply = run_chat_undo(&vault, &second.undo).unwrap();
    assert_eq!(
        reply.text,
        "Inbox.md changed since the capture; nothing was undone."
    );
    assert!(fixture.read("Inbox.md").contains("edited by hand"));

    // A created task note is removed again.
    let task = fixture.run("unrestricted", "/task Renew passport").unwrap();
    assert_eq!(task.undo[0].before, None);
    run_chat_undo(&vault, &task.undo).unwrap();
    assert!(!fixture
        .paths
        .vault_root()
        .join(&task.changed_paths[0])
        .exists());

    // Read-only profiles cannot undo.
    let readonly = fixture.guard("readonly");
    let third = fixture.run("unrestricted", "third").unwrap();
    assert!(run_chat_undo(&vault_for(&fixture, &readonly, "readonly"), &third.undo).is_err());
}

#[test]
fn attachments_are_saved_and_embedded_with_their_caption() {
    let fixture = Fixture::new(
        "[inbox]\npath = \"Inbox.md\"\nformat = \"- {text}\"\ntimestamp = false\n\
         [bot]\nattachment_folder = \"Media/Bot\"\n",
        &[],
    );
    let guard = fixture.guard("unrestricted");
    let vault = vault_for(&fixture, &guard, "unrestricted");
    let attachment = ChatAttachment {
        file_name: "../My receipt?.pdf".to_string(),
        bytes: b"%PDF-1.4 fake".to_vec(),
        caption: Some("Hardware store".to_string()),
    };
    let reply = run_chat_attachment(&vault, &attachment).unwrap();
    assert_eq!(
        reply.text,
        "Saved Media/Bot/My-receipt-.pdf\nCaptured to Inbox.md"
    );
    assert_eq!(
        fs::read(fixture.paths.vault_root().join("Media/Bot/My-receipt-.pdf")).unwrap(),
        b"%PDF-1.4 fake"
    );
    assert!(fixture
        .read("Inbox.md")
        .contains("- Hardware store ![[Media/Bot/My-receipt-.pdf]]"));

    // Same name again: a free name, not an overwrite.
    let again = run_chat_attachment(&vault, &attachment).unwrap();
    assert!(
        again.text.starts_with("Saved Media/Bot/My-receipt--1.pdf"),
        "{}",
        again.text
    );

    // Undo removes the file together with its capture.
    assert_eq!(again.undo.len(), 2);
    run_chat_undo(&vault, &again.undo).unwrap();
    assert!(!fixture
        .paths
        .vault_root()
        .join("Media/Bot/My-receipt--1.pdf")
        .exists());
    assert!(!fixture.read("Inbox.md").contains("My-receipt--1"));

    let readonly = fixture.guard("readonly");
    assert!(run_chat_attachment(&vault_for(&fixture, &readonly, "readonly"), &attachment).is_err());
}

#[test]
fn file_names_are_sanitized() {
    assert_eq!(sanitize_file_name("photo.jpg"), "photo.jpg");
    assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
    assert_eq!(sanitize_file_name(".hidden"), "hidden");
    assert_eq!(sanitize_file_name("..."), "file");
    assert_eq!(sanitize_file_name("a b:c*d.txt"), "a-b-c-d.txt");
    let long = format!("{}.ogg", "x".repeat(200));
    let clean = sanitize_file_name(&long);
    assert!(clean.len() <= 100, "{clean}");
    assert_eq!(clean.rsplit_once('.').map(|(_, ext)| ext), Some("ogg"));
}
