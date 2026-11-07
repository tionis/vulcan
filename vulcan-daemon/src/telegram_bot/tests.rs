//! The bot against a fake Bot API on loopback.

use super::*;
use crate::registry::AddWikiRequest;
use crate::registry::WikiId;
use axum::extract::{Path as UrlPath, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

const TOKEN: &str = "123:test-token";
const OWNER: i64 = 1001;
const STRANGER: i64 = 666;

#[derive(Default)]
struct FakeState {
    updates: VecDeque<Value>,
    next_update_id: i64,
    calls: Vec<(String, Value)>,
    offsets: Vec<Option<i64>>,
    /// Replies to `getMe` and `getUpdates` with this status and body.
    failure: Option<(u16, Value)>,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<FakeState>>);

impl Fake {
    fn push(&self, mut update: Value) {
        let mut state = self.0.lock().unwrap();
        state.next_update_id += 1;
        update["update_id"] = json!(state.next_update_id);
        state.updates.push_back(update);
    }

    fn message(&self, from: i64, chat_type: &str, text: &str) {
        self.push(json!({
            "message": {
                "message_id": 1,
                "chat": { "id": from, "type": chat_type },
                "from": { "id": from, "is_bot": false },
                "text": text,
            }
        }));
    }

    fn press(&self, from: i64, data: &str) {
        self.push(json!({
            "callback_query": {
                "id": format!("cb-{data}"),
                "from": { "id": from, "is_bot": false },
                "message": {
                    "message_id": 2,
                    "chat": { "id": from, "type": "private" },
                },
                "data": data,
            }
        }));
    }

    fn calls(&self, method: &str) -> Vec<Value> {
        self.0
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(name, _)| name == method)
            .map(|(_, body)| body.clone())
            .collect()
    }

    /// Wait until `method` has been called `count` times.
    fn wait_for(&self, method: &str, count: usize) -> Vec<Value> {
        let start = Instant::now();
        loop {
            let calls = self.calls(method);
            if calls.len() >= count {
                return calls;
            }
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "timed out waiting for {count} {method} calls; got {calls:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

async fn handle(
    State(fake): State<Fake>,
    UrlPath(path): UrlPath<String>,
    body: axum::body::Bytes,
) -> (axum::http::StatusCode, Json<Value>) {
    let (status, body, idle) = respond(&fake, &path, &body);
    if idle {
        // An empty long poll waits a little, as Telegram's would.
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    (
        axum::http::StatusCode::from_u16(status).unwrap(),
        Json(body),
    )
}

fn respond(fake: &Fake, path: &str, body: &[u8]) -> (u16, Value, bool) {
    let (token, method) = path.split_once('/').unwrap_or((path, ""));
    assert_eq!(token, format!("bot{TOKEN}"));
    let body: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let ok = |result: Value| (200, json!({ "ok": true, "result": result }), false);
    let mut state = fake.0.lock().unwrap();
    state.calls.push((method.to_string(), body.clone()));
    if matches!(method, "getMe" | "getUpdates") {
        if let Some((code, body)) = state.failure.clone() {
            return (code, body, method == "getUpdates");
        }
    }
    match method {
        "getMe" => ok(json!({ "id": 1, "is_bot": true, "username": "VaultBot" })),
        "getUpdates" => {
            let offset = body["offset"].as_i64();
            state.offsets.push(offset);
            let updates = state
                .updates
                .iter()
                .filter(|update| {
                    offset.is_none_or(|offset| update["update_id"].as_i64().unwrap() >= offset)
                })
                .cloned()
                .collect::<Vec<_>>();
            let idle = updates.is_empty();
            (200, json!({ "ok": true, "result": updates }), idle)
        }
        "getFile" => {
            let id = body["file_id"].as_str().unwrap_or_default().to_string();
            ok(json!({ "file_id": id, "file_path": format!("photos/{id}.jpg") }))
        }
        "sendMessage" => ok(json!({
            "message_id": 99,
            "chat": { "id": body["chat_id"], "type": "private" },
        })),
        _ => ok(json!(true)),
    }
}

async fn download(UrlPath(path): UrlPath<String>) -> Vec<u8> {
    assert!(path.starts_with(&format!("bot{TOKEN}/photos/")), "{path}");
    b"\xFF\xD8 fake jpeg".to_vec()
}

/// Serve the fake on loopback and return its base URL.
fn serve(fake: &Fake) -> String {
    let router = Router::new()
        .route("/file/{*path}", get(download))
        .route("/{*path}", post(handle))
        .with_state(fake.clone());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                axum::serve(listener, router).await.unwrap();
            });
    });
    format!("http://{address}")
}

struct Fixture {
    _temporary: tempfile::TempDir,
    registry: WikiRegistry,
    vault: PathBuf,
    state: PathBuf,
    config: DaemonTelegramBotConfig,
}

fn fixture(base: &str, ceiling: Option<&str>, profile: &str) -> Fixture {
    let temporary = tempfile::tempdir().unwrap();
    let vault = temporary.path().join("vault");
    std::fs::create_dir_all(vault.join(".vulcan")).unwrap();
    std::fs::write(
        vault.join(".vulcan/config.toml"),
        "[inbox]\npath = \"Inbox.md\"\nformat = \"- {text}\"\ntimestamp = false\n",
    )
    .unwrap();
    std::fs::write(vault.join("Garden.md"), "# Garden\n\nWater the tomatoes.\n").unwrap();
    let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
    registry
        .add(
            &AddWikiRequest {
                profile: None,
                id: WikiId::parse("notes").unwrap(),
                path: vault.clone(),
                groups: vec![],
                git_dir: None,
                permissions_profile: ceiling.map(str::to_string),
                sync_backend: Some("none".to_string()),
                platform_profile: None,
            },
            false,
        )
        .unwrap();
    registry
        .set_telegram_bot(
            "TEST_TELEGRAM_TOKEN".to_string(),
            Some(base.to_string()),
            false,
        )
        .unwrap();
    let config = registry
        .set_bot_user(
            DaemonBotUser {
                telegram_id: OWNER,
                vaults: vec![WikiId::parse("notes").unwrap()],
                default_vault: None,
                profile: profile.to_string(),
                reminders: false,
                alerts: false,
                digest: None,
            },
            false,
        )
        .unwrap()
        .bot
        .telegram
        .unwrap();
    Fixture {
        state: temporary.path().join("state"),
        _temporary: temporary,
        registry,
        vault,
        config,
    }
}

/// Run the bot on a thread; `stop` it to get its result.
fn start(
    fixture: &Fixture,
) -> (
    Arc<ShutdownSignal>,
    std::thread::JoinHandle<Result<(), String>>,
) {
    let stop = Arc::new(ShutdownSignal::default());
    let config = fixture.config.clone();
    let registry = fixture.registry.clone();
    let state = fixture.state.clone();
    let signal = Arc::clone(&stop);
    let handle = std::thread::spawn(move || {
        run_telegram_bot(&config, TOKEN.to_string(), &registry, &state, None, &signal)
    });
    (stop, handle)
}

fn texts_to(calls: &[Value], chat: i64) -> Vec<String> {
    calls
        .iter()
        .filter(|call| call["chat_id"] == json!(chat))
        .map(|call| call["text"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn answers_allowlisted_users_and_ignores_everyone_else() {
    let fake = Fake::default();
    let fixture = fixture(&serve(&fake), None, "unrestricted");
    let (stop, handle) = start(&fixture);

    fake.message(STRANGER, "private", "/help");
    fake.message(OWNER, "group", "/help");
    fake.message(OWNER, "private", "/help");
    fake.message(OWNER, "private", "buy oat milk");
    fake.message(OWNER, "private", "/find tomatoes");
    let sent = fake.wait_for("sendMessage", 3);
    assert_eq!(texts_to(&sent, STRANGER), Vec::<String>::new());
    let owner = texts_to(&sent, OWNER);
    assert!(owner[0].contains("/task "), "{owner:?}");
    assert_eq!(owner[1], "Captured to Inbox.md");
    assert!(owner[2].contains("Garden.md"), "{owner:?}");
    assert!(std::fs::read_to_string(fixture.vault.join("Inbox.md"))
        .unwrap()
        .contains("- buy oat milk"));

    // The search result's button carries only an opaque token.
    let data = sent[2]["reply_markup"]["inline_keyboard"][0][0]["callback_data"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(data.starts_with("a:") && !data.contains("Garden"), "{data}");
    fake.press(STRANGER, &data);
    fake.press(OWNER, "a:not-a-token");
    fake.press(OWNER, &data);
    let sent = fake.wait_for("sendMessage", 4);
    assert!(sent[3]["text"]
        .as_str()
        .unwrap()
        .contains("Water the tomatoes"));
    let answers = fake.wait_for("answerCallbackQuery", 2);
    assert_eq!(answers[0]["text"], "This button has expired.");
    assert!(!answers.iter().any(|answer| answer["callback_query_id"]
        == json!(format!("cb-{data}"))
        && answer["text"].is_string()));

    stop.cancel();
    handle.join().unwrap().unwrap();
    let status = read_telegram_bot_status(&fixture.state).unwrap().unwrap();
    assert_eq!(status.state, "stopped");
    assert_eq!(status.bot_username.as_deref(), Some("VaultBot"));
    assert_eq!(status.rejected_users.get(&STRANGER), Some(&2));
    assert_eq!(status.handled_updates, 8);
    assert!(fake.calls("setMyCommands").len() == 1);

    // The offset survives a restart, so nothing is handled twice.
    let state = ChatStateStore::open(&fixture.state).unwrap();
    assert_eq!(state.offset(PLATFORM).unwrap(), Some(9));
    let (stop, handle) = start(&fixture);
    let before = fake.calls("getUpdates").len();
    fake.wait_for("getUpdates", before + 1);
    stop.cancel();
    handle.join().unwrap().unwrap();
    assert_eq!(fake.calls("sendMessage").len(), 4);
    assert_eq!(fake.0.lock().unwrap().offsets.last(), Some(&Some(9)));
}

#[test]
fn a_user_profile_wider_than_the_registration_is_refused() {
    let fake = Fake::default();
    let fixture = fixture(&serve(&fake), Some("readonly"), "unrestricted");
    let (stop, handle) = start(&fixture);
    fake.message(OWNER, "private", "sneaky capture");
    let sent = fake.wait_for("sendMessage", 1);
    assert!(
        sent[0]["text"].as_str().unwrap().contains("exceeds"),
        "{sent:?}"
    );
    assert!(!fixture.vault.join("Inbox.md").exists());
    stop.cancel();
    handle.join().unwrap().unwrap();
}

#[test]
fn readonly_users_can_read_but_not_write() {
    let fake = Fake::default();
    let fixture = fixture(&serve(&fake), None, "readonly");
    let (stop, handle) = start(&fixture);
    fake.message(OWNER, "private", "a capture");
    fake.message(OWNER, "private", "/note Garden");
    let sent = fake.wait_for("sendMessage", 2);
    let texts = texts_to(&sent, OWNER);
    assert!(texts[0].starts_with("Error: "), "{texts:?}");
    assert!(
        !texts[0].contains(&fixture.vault.display().to_string()),
        "errors do not reveal the vault path: {texts:?}"
    );
    assert!(texts[1].contains("Water the tomatoes"), "{texts:?}");
    stop.cancel();
    handle.join().unwrap().unwrap();
}

#[test]
fn a_rejected_token_stops_the_service() {
    let fake = Fake::default();
    fake.0.lock().unwrap().failure = Some((
        401,
        json!({ "ok": false, "error_code": 401, "description": "Unauthorized" }),
    ));
    let fixture = fixture(&serve(&fake), None, "readonly");
    let (_stop, handle) = start(&fixture);
    let error = handle.join().unwrap().unwrap_err();
    assert!(error.contains("token was rejected"), "{error}");
    assert!(!error.contains("test-token"));
    let status = read_telegram_bot_status(&fixture.state).unwrap().unwrap();
    assert_eq!(status.state, "failed");
}

#[test]
fn a_second_poller_is_reported_as_a_conflict() {
    let fake = Fake::default();
    let fixture = fixture(&serve(&fake), None, "readonly");
    let (stop, handle) = start(&fixture);
    fake.wait_for("getUpdates", 1);
    fake.0.lock().unwrap().failure = Some((
        409,
        json!({ "ok": false, "error_code": 409, "description": "Conflict: terminated by other getUpdates request" }),
    ));
    let start = Instant::now();
    loop {
        let status = read_telegram_bot_status(&fixture.state)
            .unwrap()
            .unwrap_or_default();
        if status.state == "conflict" {
            assert!(status.last_error.unwrap().contains("only one device"));
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "no conflict state"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    stop.cancel();
    handle.join().unwrap().unwrap();
}

#[test]
fn the_report_shows_configuration_without_the_token() {
    let fixture = fixture("http://127.0.0.1:9", None, "readonly");
    let config = fixture.registry.load().unwrap();
    let report = telegram_bot_report(&config, &fixture.state).unwrap();
    assert!(report.configured);
    assert_eq!(report.token_env.as_deref(), Some("TEST_TELEGRAM_TOKEN"));
    assert_eq!(report.users.len(), 1);
    assert_eq!(report.users[0].profile, "readonly");
    assert_eq!(report.status, None);
}

#[test]
fn ignored_senders_are_counted_within_a_bound() {
    let temporary = tempfile::tempdir().unwrap();
    let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
    let mut bot = Bot {
        client: TelegramClient::new(Some("http://127.0.0.1:9"), TOKEN.to_string()).unwrap(),
        registry: &registry,
        state_root: temporary.path(),
        gate: None,
        state: ChatStateStore::open(temporary.path()).unwrap(),
        status: TelegramBotStatus::default(),
        plans: Vec::new(),
        next_plan_ms: 0,
        next_digest_ms: 0,
    };
    for id in 1..=100 {
        bot.reject(id);
    }
    bot.reject(1);
    assert_eq!(bot.status.rejected_users.len(), MAX_REJECTED_USERS);
    assert_eq!(bot.status.rejected_users[&1], 2);
}

/// `minutes` from now as a `⏰` wall-clock time in UTC.
fn remind_at(minutes: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::minutes(minutes))
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

#[test]
fn delivers_due_reminders_once_and_updates_them_when_done_elsewhere() {
    let fake = Fake::default();
    let fixture = fixture(&serve(&fake), None, "readonly");
    std::fs::write(
        fixture.vault.join(".vulcan/config.toml"),
        "[tasks.reminders]\ntimezone = \"UTC\"\n[bot.reminders]\ncatch_up_window = \"1d\"\n",
    )
    .unwrap();
    std::fs::write(
        fixture.vault.join("Calls.md"),
        format!(
            "- [ ] Call the bank ⏰ {}\n- [ ] Long missed ⏰ {}\n- [ ] Much later ⏰ 2999-01-01 10:00\n",
            remind_at(-1),
            remind_at(-3 * 24 * 60),
        ),
    )
    .unwrap();
    let mut user = fixture.config.users[0].clone();
    user.reminders = true;
    fixture.registry.set_bot_user(user, false).unwrap();

    let (stop, handle) = start(&fixture);
    let sent = fake.wait_for("sendMessage", 1);
    assert_eq!(sent[0]["chat_id"], json!(OWNER));
    let text = sent[0]["text"].as_str().unwrap();
    assert!(text.starts_with("⏰ Call the bank\nCalls.md\n"), "{text}");
    assert!(!text.contains("(late)"), "{text}");

    // Completed in the vault: the message is edited, not resent.
    let calls = std::fs::read_to_string(fixture.vault.join("Calls.md")).unwrap();
    std::fs::write(
        fixture.vault.join("Calls.md"),
        calls.replacen("- [ ] Call the bank", "- [x] Call the bank", 1),
    )
    .unwrap();
    let edits = fake.wait_for("editMessageText", 1);
    assert_eq!(edits[0]["message_id"], json!(99));
    assert!(
        edits[0]["text"]
            .as_str()
            .unwrap()
            .ends_with("No longer open: completed, cancelled, or removed."),
        "{edits:?}"
    );
    stop.cancel();
    handle.join().unwrap().unwrap();
    assert_eq!(fake.calls("sendMessage").len(), 1, "the missed one expired");

    // A restart neither resends nor re-resolves.
    let (stop, handle) = start(&fixture);
    let polls = fake.calls("getUpdates").len();
    fake.wait_for("getUpdates", polls + 5);
    stop.cancel();
    handle.join().unwrap().unwrap();
    assert_eq!(fake.calls("sendMessage").len(), 1);
    assert_eq!(fake.calls("editMessageText").len(), 1);
    let report = telegram_bot_report(&fixture.registry.load().unwrap(), &fixture.state).unwrap();
    assert_eq!(report.reminders.get("expired"), Some(&1));
    assert_eq!(report.reminders.get("resolved"), Some(&1));
    assert_eq!(report.status.unwrap().reminders_sent, 1);
}

#[test]
fn users_without_reminders_get_none() {
    let fake = Fake::default();
    let fixture = fixture(&serve(&fake), None, "readonly");
    std::fs::write(
        fixture.vault.join("Calls.md"),
        format!("- [ ] Call the bank ⏰ {}\n", remind_at(-1)),
    )
    .unwrap();
    let (stop, handle) = start(&fixture);
    fake.wait_for("getUpdates", 5);
    stop.cancel();
    handle.join().unwrap().unwrap();
    assert!(fake.calls("sendMessage").is_empty());
}

#[test]
fn reminder_buttons_snooze_through_the_ledger() {
    let fake = Fake::default();
    let fixture = fixture(&serve(&fake), None, "unrestricted");
    std::fs::write(
        fixture.vault.join(".vulcan/config.toml"),
        "[tasks.reminders]\ntimezone = \"UTC\"\n[bot.reminders]\nsnooze_options = [\"2h\"]\n",
    )
    .unwrap();
    std::fs::write(
        fixture.vault.join("Calls.md"),
        format!("- [ ] Call the bank ⏰ {}\n", remind_at(-1)),
    )
    .unwrap();
    let mut user = fixture.config.users[0].clone();
    user.reminders = true;
    fixture.registry.set_bot_user(user, false).unwrap();

    let (stop, handle) = start(&fixture);
    let sent = fake.wait_for("sendMessage", 1);
    let buttons = sent[0]["reply_markup"]["inline_keyboard"][0]
        .as_array()
        .unwrap()
        .clone();
    let labels = buttons
        .iter()
        .map(|button| button["text"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(labels, ["Done", "Snooze 2h"]);
    let snooze = buttons[1]["callback_data"].as_str().unwrap().to_string();
    let (row, _) = snooze.strip_prefix("r:").unwrap().split_once(':').unwrap();
    assert!(
        !snooze.contains("Calls"),
        "only the ledger row and an index: {snooze}"
    );

    fake.press(OWNER, &format!("r:{row}:7"));
    fake.press(OWNER, &snooze);
    let edits = fake.wait_for("editMessageText", 1);
    assert!(
        edits[0]["text"]
            .as_str()
            .unwrap()
            .contains("Snoozed until "),
        "{edits:?}"
    );
    let calls = std::fs::read_to_string(fixture.vault.join("Calls.md")).unwrap();
    assert!(!calls.contains(&remind_at(-1)), "{calls}");
    fake.press(OWNER, &snooze);
    let answers = fake.wait_for("answerCallbackQuery", 3);
    let texts = answers
        .iter()
        .map(|answer| answer["text"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(
        texts,
        [
            "This reminder is no longer open.",
            "",
            "This reminder is no longer open."
        ],
        "an unoffered action and a second press are refused"
    );
    stop.cancel();
    handle.join().unwrap().unwrap();
    assert_eq!(
        fake.calls("sendMessage").len(),
        1,
        "the snoozed one is not due yet"
    );
}

#[test]
fn photos_are_saved_with_their_caption_and_undo_reverts_them() {
    let fake = Fake::default();
    let fixture = fixture(&serve(&fake), None, "unrestricted");
    fake.push(json!({
        "message": {
            "message_id": 1,
            "chat": { "id": OWNER, "type": "private" },
            "from": { "id": OWNER, "is_bot": false },
            "caption": "Whiteboard",
            "photo": [
                { "file_id": "small", "file_size": 10 },
                { "file_id": "large", "file_size": 20 },
            ],
        }
    }));
    fake.push(json!({
        "message": {
            "message_id": 2,
            "chat": { "id": OWNER, "type": "private" },
            "from": { "id": OWNER, "is_bot": false },
            "document": { "file_id": "huge", "file_name": "movie.mkv", "file_size": 50_000_000 },
        }
    }));
    let (stop, handle) = start(&fixture);
    let sent = fake.wait_for("sendMessage", 2);
    let saved = sent[0]["text"].as_str().unwrap();
    assert!(saved.starts_with("Saved Attachments/photo-"), "{saved}");
    let path = saved
        .lines()
        .next()
        .unwrap()
        .trim_start_matches("Saved ")
        .to_string();
    assert_eq!(
        std::fs::read(fixture.vault.join(&path)).unwrap(),
        b"\xFF\xD8 fake jpeg"
    );
    let inbox = std::fs::read_to_string(fixture.vault.join("Inbox.md")).unwrap();
    assert!(
        inbox.contains(&format!("- Whiteboard ![[{path}]]")),
        "{inbox}"
    );
    assert_eq!(
        sent[1]["text"],
        "Files over 20 MB cannot be received by bots."
    );
    assert_eq!(
        fake.calls("getFile").len(),
        1,
        "the largest photo size only; the oversized file is never fetched"
    );
    assert_eq!(fake.calls("getFile")[0]["file_id"], "large");

    fake.message(OWNER, "private", "/undo");
    fake.message(OWNER, "private", "/undo");
    let sent = fake.wait_for("sendMessage", 4);
    assert_eq!(sent[2]["text"], format!("Undone: {path}, Inbox.md"));
    assert_eq!(sent[3]["text"], "Nothing to undo.");
    assert!(!fixture.vault.join(&path).exists());
    assert!(!std::fs::read_to_string(fixture.vault.join("Inbox.md"))
        .unwrap_or_default()
        .contains("Whiteboard"));
    stop.cancel();
    handle.join().unwrap().unwrap();
}

#[test]
fn undo_buttons_revert_only_the_latest_capture() {
    let fake = Fake::default();
    let fixture = fixture(&serve(&fake), None, "unrestricted");
    let (stop, handle) = start(&fixture);
    fake.message(OWNER, "private", "buy milk");
    fake.message(OWNER, "private", "buy eggs");
    let sent = fake.wait_for("sendMessage", 2);
    let undo_button = |index: usize| {
        let button = &sent[index]["reply_markup"]["inline_keyboard"][0][0];
        assert_eq!(button["text"], "Undo", "{sent:?}");
        button["callback_data"].as_str().unwrap().to_string()
    };
    let (first, second) = (undo_button(0), undo_button(1));

    fake.press(OWNER, &first);
    let answers = fake.wait_for("answerCallbackQuery", 1);
    assert_eq!(answers[0]["text"], "Only the latest capture can be undone.");
    fake.press(OWNER, &second);
    let sent = fake.wait_for("sendMessage", 3);
    assert_eq!(sent[2]["text"], "Undone: Inbox.md");
    let inbox = std::fs::read_to_string(fixture.vault.join("Inbox.md")).unwrap();
    assert!(
        inbox.contains("buy milk") && !inbox.contains("buy eggs"),
        "{inbox}"
    );

    fake.press(OWNER, &second);
    let answers = fake.wait_for("answerCallbackQuery", 3);
    assert_eq!(answers[2]["text"], "This button has expired.");
    stop.cancel();
    handle.join().unwrap().unwrap();
}

#[test]
fn sync_alerts_reach_alert_users_of_the_wiki_only() {
    let fake = Fake::default();
    let fixture = fixture(&serve(&fake), None, "readonly");
    // A variable only this test uses, so parallel tests cannot see it.
    std::env::set_var("TEST_ALERT_TELEGRAM_TOKEN", TOKEN);
    fixture
        .registry
        .set_telegram_bot(
            "TEST_ALERT_TELEGRAM_TOKEN".to_string(),
            fixture.config.api_base.clone(),
            false,
        )
        .unwrap();
    let alert = |wiki: &str| crate::alerts::SyncAlert {
        version: 1,
        event: "sync_conflicted".to_string(),
        severity: crate::alerts::AlertSeverity::Warning,
        job_id: "job-1".to_string(),
        wiki_id: wiki.to_string(),
        state: vulcan_sync::SyncJobState::Conflicted,
        category: None,
        retryable: false,
    };
    let config = fixture.registry.load().unwrap();
    let telegram = config.bot.telegram.clone().unwrap();
    assert!(
        crate::alert_delivery::telegram_alert_sink(&config).is_none(),
        "no user asked for alerts"
    );
    crate::alert_delivery::deliver_telegram(&fixture.registry, &telegram, &alert("notes")).unwrap();
    assert!(fake.calls("sendMessage").is_empty());

    let mut user = fixture.config.users[0].clone();
    user.alerts = true;
    fixture.registry.set_bot_user(user, false).unwrap();
    let config = fixture.registry.load().unwrap();
    assert!(crate::alert_delivery::telegram_alert_sink(&config).is_some());
    crate::alert_delivery::deliver_telegram(&fixture.registry, &telegram, &alert("other")).unwrap();
    assert!(fake.calls("sendMessage").is_empty(), "not the user's wiki");
    crate::alert_delivery::deliver_telegram(&fixture.registry, &telegram, &alert("notes")).unwrap();
    let sent = fake.calls("sendMessage");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["chat_id"], json!(OWNER));
    let text = sent[0]["text"].as_str().unwrap();
    assert!(
        text.starts_with("⚠️ Vulcan sync needs resolution\nWiki `notes`"),
        "{text}"
    );
}

#[test]
fn digests_arrive_once_a_day() {
    let fake = Fake::default();
    let fixture = fixture(&serve(&fake), None, "unrestricted");
    let today = chrono::Utc::now().date_naive();
    std::fs::write(
        fixture.vault.join(".vulcan/config.toml"),
        "[tasks.reminders]\ntimezone = \"UTC\"\n",
    )
    .unwrap();
    std::fs::write(
        fixture.vault.join("Today.md"),
        format!("- [ ] Pay rent 📅 {today}\n"),
    )
    .unwrap();
    let mut user = fixture.config.users[0].clone();
    user.digest = Some("00:00".to_string());
    fixture.registry.set_bot_user(user, false).unwrap();

    let (stop, handle) = start(&fixture);
    let sent = fake.wait_for("sendMessage", 1);
    let text = sent[0]["text"].as_str().unwrap();
    assert!(
        text.starts_with("☀️ Today, ") && text.contains("Pay rent"),
        "{text}"
    );
    assert_eq!(
        sent[0]["reply_markup"]["inline_keyboard"][0][0]["text"],
        "Done: Pay rent"
    );
    let polls = fake.calls("getUpdates").len();
    fake.wait_for("getUpdates", polls + 5);
    stop.cancel();
    handle.join().unwrap().unwrap();

    // Neither a later cycle nor a restart sends it again the same day.
    let (stop, handle) = start(&fixture);
    let polls = fake.calls("getUpdates").len();
    fake.wait_for("getUpdates", polls + 5);
    stop.cancel();
    handle.join().unwrap().unwrap();
    assert_eq!(fake.calls("sendMessage").len(), 1);
}
