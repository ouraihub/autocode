use super::*;
use gpui::TestAppContext;
use model_provider::{ConfigSource, ModelSelection};
fn fake_claude() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../agent_harness/tests/fixtures/fake-claude.sh")
    }

    pub(crate) fn new_store(directory: &Path, cx: &mut TestAppContext) -> Entity<AgentStore> {
        cx.executor().allow_parking();
        // SAFETY: tests in this crate only ever set these variables to the same fixtures.
        unsafe {
            std::env::set_var("CLAUDE_CODE_EXECUTABLE", fake_claude());
            std::env::set_var(
                "CLAUDE_CONFIG_DIR",
                fake_claude().with_file_name("claude-config"),
            );
        }
        cx.update(|cx| {
            gpui_tokio::init(cx);
            if !cx.has_global::<settings::SettingsStore>() {
                let settings = settings::SettingsStore::test(cx);
                cx.set_global(settings);
            }
        });
        let languages = Arc::new(LanguageRegistry::test(cx.background_executor.clone()));
        cx.new(|cx| AgentStore::new(directory.into(), languages, cx))
    }

    pub(crate) fn install_global(store: Entity<AgentStore>, cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(GlobalAgentStore(store)));
    }

    #[gpui::test]
    async fn reported_runtime_model_does_not_replace_managed_selection(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| store.create_session(AgentKind::Omp, directory.path().to_path_buf(), cx));
        let selected = ModelSelection::Managed { provider_id: "service-id".into(), model_id: "model-id".into() };
        session.update(cx, |session, cx| {
            session.update_settings(|settings| settings.model = Some(selected.clone()), cx);
            session.apply_event(AgentEvent::ModelReported { model_id: "model-id".into(), provider_id: Some("autocode-service-id".into()), reasoning: None }, cx);
            assert_eq!(session.reported_model(), Some(("model-id", Some("autocode-service-id"))));
            assert_eq!(session.settings().model, Some(selected));
        });
    }

    pub(crate) fn run_until(
        cx: &mut TestAppContext,
        mut done: impl FnMut(&mut TestAppContext) -> bool,
    ) {
        for _ in 0..500 {
            cx.run_until_parked();
            if done(cx) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the agent never reached the expected state");
    }

    #[gpui::test]
    async fn says_once_when_claude_did_not_start_in_auto(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let notices = |cx: &mut TestAppContext| {
            session.read_with(cx, |session, cx| {
                summarize(session.entries(), cx)
                    .into_iter()
                    .filter(|entry| entry.starts_with("notice: Auto mode isn't available"))
                    .count()
            })
        };
        let reported = |mode: &str| {
            vec![AgentEvent::PermissionModeReported {
                mode: mode.to_string(),
            }]
        };
        feed_events(&session, reported("auto"), cx);
        assert_eq!(notices(cx), 0);
        feed_events(&session, reported("default"), cx);
        feed_events(&session, reported("default"), cx);
        assert_eq!(notices(cx), 1);
    }

    #[gpui::test]
    async fn saved_chats_load_only_while_agent_chat_is_on(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.update_settings(
                |settings| {
                    settings.model = Some(ModelSelection::Native { id: "opus".into() })
                },
                cx,
            )
        });
        cx.run_until_parked();

        update_chat_settings(cx, |content| content.enabled = Some(false));
        let reopened = new_store(directory.path(), cx);
        cx.run_until_parked();
        let saved = |cx: &mut TestAppContext| {
            reopened.read_with(cx, |store, _| store.sessions_metadata().count())
        };
        assert_eq!(saved(cx), 0);

        reopened.update(cx, |store, cx| store.load_saved_chats(cx));
        cx.run_until_parked();
        assert_eq!(saved(cx), 1);
    }

    #[test]
    fn each_agent_is_updated_at_most_once_per_interval() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let attempts = agent_updates_path(&directory.path().join("missing"));
        assert!(claim_update_attempt(&attempts, AgentKind::Claude));
        assert!(!claim_update_attempt(&attempts, AgentKind::Claude));
        assert!(claim_update_attempt(&attempts, AgentKind::Codex));

        let interval_old = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after 1970")
            .as_secs()
            - AGENT_UPDATE_INTERVAL.as_secs();
        std::fs::write(&attempts, format!(r#"{{"claude":{interval_old}}}"#))
            .expect("write attempts");
        assert!(claim_update_attempt(&attempts, AgentKind::Claude));
        assert!(!claim_update_attempt(&attempts, AgentKind::Claude));
    }

    #[gpui::test]
    async fn saved_chats_open_before_the_chat_list_finishes_loading(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.update_settings(
                |settings| settings.permission = PermissionMode::FullAccess,
                cx,
            )
        });
        let id = session_id(&session, cx);
        cx.run_until_parked();

        let reopened_store = new_store(directory.path(), cx);
        let reopened = reopened_store
            .update(cx, |store, cx| {
                store.open_saved_session(id.clone().into(), cx)
            })
            .await
            .expect("chat reopens");
        assert_eq!(session_id(&reopened, cx), id);
    }

    #[gpui::test]
    async fn a_chosen_permission_mode_stays_with_its_chat(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        assert_eq!(
            session.read_with(cx, |session, _| session.settings().permission),
            PermissionMode::Auto
        );
        session.update(cx, |session, cx| {
            session.update_settings(
                |settings| settings.permission = PermissionMode::FullAccess,
                cx,
            )
        });
        let id = session_id(&session, cx);
        cx.run_until_parked();

        let reopened_store = new_store(directory.path(), cx);
        cx.run_until_parked();
        let reopened = reopened_store
            .update(cx, |store, cx| store.open_session(&id, cx))
            .await
            .expect("chat reopens");
        assert_eq!(
            reopened.read_with(cx, |session, _| session.settings().permission),
            PermissionMode::FullAccess
        );
        let fresh = reopened_store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        assert_eq!(
            fresh.read_with(cx, |session, _| session.settings().permission),
            PermissionMode::Auto,
            "new chats always start in auto"
        );
    }

    #[gpui::test]
    async fn claude_reply_streams_into_the_transcript_and_is_saved(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.send_message("scenario:happy", cx)
        });
        run_until(cx, |cx| {
            session.read_with(cx, |session, _| !session.is_working())
        });

        session.read_with(cx, |session, cx| {
            assert_eq!(session.metadata().title, "scenario:happy");
            assert_eq!(
                session.metadata().native_session_id.as_deref(),
                Some("sess-1")
            );
            let summary: Vec<String> = session
                .entries()
                .iter()
                .map(|entry| match entry {
                    Entry::User { text, .. } => format!("user: {text}"),
                    Entry::Assistant { markdown, .. } => {
                        format!("assistant: {}", markdown.read(cx).source())
                    }
                    Entry::Thinking(markdown) => {
                        format!("thinking: {}", markdown.read(cx).source())
                    }
                    Entry::Tool(tool) => format!("tool {}: {:?}", tool.id, tool.status),
                    Entry::Notice { text, .. } => format!("notice: {text}"),
                    Entry::Image { path } => format!("image: {}", path.display()),
                })
                .collect();
            assert_eq!(
                summary,
                [
                    "user: scenario:happy",
                    "thinking: pondering",
                    "assistant: Hello",
                    "tool tool-1: Completed",
                    "tool tool-2: Failed",
                ]
            );
        });

        cx.executor().advance_clock(SAVE_DEBOUNCE * 2);
        let saved_entries = entries_path(directory.path(), &session_id(&session, cx));
        run_until(cx, |_| saved_entries.exists());
        let summaries = store.read_with(cx, |store, cx| {
            store.sessions_in(&[directory.path().to_path_buf()], cx)
        });
        assert_eq!(summaries.len(), 1);
        assert!(!summaries[0].working);
    }

    #[gpui::test]
    async fn permissions_and_questions_wait_for_the_user(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.send_message("scenario:askuser", cx)
        });

        run_until(cx, |cx| {
            session.read_with(cx, |session, _| !session.pending_questions().is_empty())
        });
        let permission_id = session.read_with(cx, |session, _| {
            let pending = &session.pending_questions()[0];
            assert!(pending.is_permission());
            assert_eq!(pending.questions[0].question, "Run `ls`?");
            pending.id().to_string()
        });
        session.update(cx, |session, cx| {
            session.answer_permission(&permission_id, true, cx)
        });

        run_until(cx, |cx| {
            session.read_with(cx, |session, _| {
                session
                    .pending_questions()
                    .first()
                    .is_some_and(|pending| !pending.is_permission())
            })
        });
        let (question_id, answers) = session.read_with(cx, |session, _| {
            let pending = &session.pending_questions()[0];
            let answers = pending
                .questions
                .iter()
                .map(|question| UserInputAnswer {
                    question_id: question.id.clone(),
                    labels: vec!["B".into()],
                })
                .collect::<Vec<_>>();
            (pending.id().to_string(), answers)
        });
        session.update(cx, |session, cx| session.answer(&question_id, answers, cx));

        run_until(cx, |cx| {
            session.read_with(cx, |session, _| !session.is_working())
        });
        session.read_with(cx, |session, _| {
            assert!(session.pending_questions().is_empty());
            assert!(
                !session
                    .entries()
                    .iter()
                    .any(|entry| matches!(entry, Entry::Notice { is_error: true, .. })),
                "the fixture reports an error unless both answers arrive"
            );
        });
    }

    #[gpui::test]
    async fn answers_reach_the_agent_and_withdrawals_are_reported(_cx: &mut TestAppContext) {
        let answer = || {
            vec![UserInputAnswer {
                question_id: "q".into(),
                labels: vec!["A".into()],
            }]
        };

        let (agent_tx, agent_rx) = tokio::sync::oneshot::channel();
        let (chat_tx, chat_rx) = tokio::sync::oneshot::channel();
        chat_tx.send(answer()).unwrap();
        assert!(!agent_stopped_waiting(agent_tx, chat_rx).await);
        assert_eq!(agent_rx.await.unwrap(), answer());

        let (agent_tx, agent_rx) = tokio::sync::oneshot::channel();
        let (chat_tx, chat_rx) = tokio::sync::oneshot::channel::<Vec<UserInputAnswer>>();
        drop(agent_rx);
        assert!(agent_stopped_waiting(agent_tx, chat_rx).await);
        assert!(chat_tx.is_closed());

        let (agent_tx, agent_rx) = tokio::sync::oneshot::channel();
        let (chat_tx, chat_rx) = tokio::sync::oneshot::channel();
        drop(chat_tx);
        assert!(!agent_stopped_waiting(agent_tx, chat_rx).await);
        assert!(agent_rx.await.is_err());
    }

    #[gpui::test]
    async fn withdrawn_questions_leave_the_picker(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Codex, directory.path().to_path_buf(), cx)
        });
        let pending = |id: &str, responder| PendingQuestion {
            questions: vec![UserInputQuestion {
                id: id.into(),
                header: "Choice".into(),
                question: "Pick one".into(),
                options: vec!["A".into()],
                multi_select: false,
                prefill: None,
                multiline: false,
                accepts_typed_answer: false,
            }],
            responder,
            generation: 0,
            deadline: None,
        };
        let (withdrawn_tx, withdrawn_rx) = tokio::sync::oneshot::channel();
        let (waiting_tx, _waiting_rx) = tokio::sync::oneshot::channel();
        drop(withdrawn_rx);
        session.update(cx, |session, cx| {
            session
                .pending_questions
                .push(pending("withdrawn", Some(withdrawn_tx)));
            session
                .pending_questions
                .push(pending("waiting", Some(waiting_tx)));
            session.pending_questions.push(pending("local", None));
            session.drop_withdrawn_questions(cx);
            let ids: Vec<&str> = session
                .pending_questions()
                .iter()
                .map(PendingQuestion::id)
                .collect();
            assert_eq!(ids, ["waiting", "local"]);
        });
    }

    #[test]
    fn only_allow_or_deny_permission_requests_count_as_tool_permissions() {
        let question = |options: &[&str]| PendingQuestion {
            questions: vec![UserInputQuestion {
                id: "q".into(),
                header: agent_harness::claude::PERMISSION_HEADER.into(),
                question: "Proceed?".into(),
                options: options.iter().map(|option| option.to_string()).collect(),
                multi_select: false,
                prefill: None,
                multiline: false,
                accepts_typed_answer: false,
            }],
            responder: None,
            generation: 0,
            deadline: None,
        };
        assert!(question(&["Allow", "Deny"]).is_tool_permission());
        assert!(!question(&["Yes", "No"]).is_tool_permission());
    }

    #[gpui::test]
    async fn agent_activity_is_reported_per_project(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let working_root = directory.path().join("working");
        let waiting_root = directory.path().join("waiting");
        let idle_root = directory.path().join("idle");
        let working = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, working_root.clone(), cx)
        });
        let waiting = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, waiting_root.clone(), cx)
        });
        store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, idle_root.clone(), cx)
        });
        working.update(cx, |session, _| session.working = true);
        waiting.update(cx, |session, _| {
            session.working = true;
            session.pending_questions.push(PendingQuestion {
                questions: Vec::new(),
                responder: None,
                generation: 0,
                deadline: None,
            });
        });

        store.read_with(cx, |store, cx| {
            let activity = |roots: &[PathBuf]| store.agent_activity_in(roots, cx);
            assert_eq!(
                activity(std::slice::from_ref(&working_root)),
                Some(workspace::AgentActivity::Working)
            );
            assert_eq!(
                activity(std::slice::from_ref(&waiting_root)),
                Some(workspace::AgentActivity::NeedsInput)
            );
            assert_eq!(
                activity(&[working_root.clone(), waiting_root.clone()]),
                Some(workspace::AgentActivity::NeedsInput)
            );
            assert_eq!(activity(std::slice::from_ref(&idle_root)), None);
            assert_eq!(activity(&[directory.path().to_path_buf()]), None);
        });
    }

    #[gpui::test]
    async fn agent_activity_changes_are_announced_only_when_they_change(cx: &mut TestAppContext) {
        use std::{cell::Cell, rc::Rc};

        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let changes = cx.update(|cx| {
            let changes = cx.new(|_| workspace::AgentActivityChanges);
            cx.set_global(workspace::ProjectAgentActivity {
                activity: project_agent_activity,
                changes: changes.clone(),
            });
            announce_activity_changes(&store, cx);
            changes
        });
        let announcements = Rc::new(Cell::new(0));
        let _subscription = cx.update(|cx| {
            let announcements = announcements.clone();
            cx.observe(&changes, move |_, _| {
                announcements.set(announcements.get() + 1)
            })
        });
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        cx.run_until_parked();
        assert_eq!(announcements.get(), 0, "a new idle chat changes nothing");

        session.update(cx, |session, cx| {
            session.working = true;
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(announcements.get(), 1);

        session.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        assert_eq!(
            announcements.get(),
            1,
            "streaming updates alone are not announced"
        );

        session.update(cx, |session, cx| {
            session.pending_questions.push(PendingQuestion {
                questions: Vec::new(),
                responder: None,
                generation: 0,
                deadline: None,
            });
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(announcements.get(), 2);
    }

    #[gpui::test]
    async fn switching_to_full_access_approves_a_waiting_permission(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.send_message("scenario:askuser", cx)
        });
        run_until(cx, |cx| {
            session.read_with(cx, |session, _| !session.pending_questions().is_empty())
        });

        session.update(cx, |session, cx| {
            session.update_settings(
                |settings| settings.permission = PermissionMode::FullAccess,
                cx,
            )
        });

        run_until(cx, |cx| {
            session.read_with(cx, |session, _| {
                session
                    .pending_questions()
                    .first()
                    .is_some_and(|pending| !pending.is_permission())
            })
        });
    }

    #[gpui::test]
    async fn picked_settings_become_the_default_for_new_chats(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let first = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Codex, directory.path().to_path_buf(), cx)
        });
        first.update(cx, |session, cx| {
            session.update_settings(
                |settings| {
                    settings.model = Some(ModelSelection::Native { id: "gpt-5.6-sol".into() });
                    settings.reasoning = Some(ReasoningLevel::High);
                },
                cx,
            )
        });
        cx.run_until_parked();
        let second = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Codex, directory.path().to_path_buf(), cx)
        });
        second.read_with(cx, |session, _| {
            assert_eq!(session.settings().native_model(), Some("gpt-5.6-sol"));
            assert_eq!(session.settings().reasoning, Some(ReasoningLevel::High));
        });
        let claude = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        claude.read_with(cx, |session, _| {
            assert_eq!(session.settings(), &RunSettings::default())
        });
    }

    #[gpui::test]
    async fn context_usage_keeps_known_fields_across_partial_reports(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.apply_event(
                AgentEvent::ContextUsage {
                    tokens: Some(20_000),
                    window: None,
                },
                cx,
            );
            session.apply_event(
                AgentEvent::ContextUsage {
                    tokens: None,
                    window: Some(200_000),
                },
                cx,
            );
            session.apply_event(
                AgentEvent::ContextUsage {
                    tokens: Some(50_000),
                    window: Some(0),
                },
                cx,
            );
        });
        session.read_with(cx, |session, _| {
            let context = session.context().expect("context measured");
            assert_eq!(context.tokens, Some(50_000));
            assert_eq!(context.window, Some(200_000));
            assert_eq!(context.fraction(), Some(0.25));
        });
    }

    fn last_reply(session: &Entity<AgentSession>, cx: &mut TestAppContext) -> String {
        session.read_with(cx, |session, cx| {
            session
                .entries()
                .iter()
                .rev()
                .find_map(|entry| match entry {
                    Entry::Assistant { markdown, .. } => {
                        Some(markdown.read(cx).source().to_string())
                    }
                    _ => None,
                })
                .unwrap_or_default()
        })
    }

    #[cfg(unix)]
    #[gpui::test]
    async fn changed_settings_restart_the_agent_with_new_flags(cx: &mut TestAppContext) {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir().expect("temporary directory");
        let script = directory.path().join("echo-args-claude");
        std::fs::write(
            &script,
            r#"#!/bin/sh
args="$*"
printf '{"type":"system","subtype":"init","model":"m","tools":[],"cwd":"/tmp","session_id":"sess-args"}\n'
while read -r line; do
  printf '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"%s"}}}\n' "$args"
  printf '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[]}}\n'
  printf '{"type":"result","subtype":"success","result":"ok","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-args"}\n'
done
"#,
        )
        .expect("write fixture");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("make fixture executable");
        let store = new_store(directory.path(), cx);
        store.update(cx, |store, _| {
            store.harnesses.insert(
                AgentKind::Claude,
                Arc::new(ClaudeHarness::new().with_executable(script)),
            );
        });
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.update_settings(
                |settings| {
                    settings.model = Some(ModelSelection::Native { id: "claude-opus-5-5".into() });
                    settings.reasoning = Some(ReasoningLevel::High);
                },
                cx,
            );
            session.send_message("first", cx);
        });
        run_until(cx, |cx| {
            session.read_with(cx, |session, _| !session.is_working())
        });
        let first = last_reply(&session, cx);
        assert!(first.contains("--model claude-opus-5-5"), "{first}");
        assert!(first.contains("--effort high"), "{first}");
        assert!(!first.contains("--resume"), "{first}");

        session.update(cx, |session, cx| {
            session.update_settings(
                |settings| settings.reasoning = Some(ReasoningLevel::Low),
                cx,
            );
            session.send_message("second", cx);
        });
        run_until(cx, |cx| {
            session.read_with(cx, |session, _| !session.is_working())
                && last_reply(&session, cx) != first
        });
        let second = last_reply(&session, cx);
        assert!(second.contains("--effort low"), "{second}");
        assert!(second.contains("--resume=sess-args"), "{second}");
    }

    #[cfg(unix)]
    #[gpui::test]
    async fn manual_compaction_reloads_claude_on_the_next_message(cx: &mut TestAppContext) {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir().expect("temporary directory");
        let script = directory.path().join("steer-aware-claude");
        std::fs::write(
            &script,
            r#"#!/bin/sh
case "$*" in *--resume*) run=resumed;; *) run=new;; esac
printf '{"type":"system","subtype":"init","model":"m","tools":[],"cwd":"/tmp","session_id":"sess-commands"}\n'
while read -r line; do
  case "$line" in *priority*) kind=steer;; *) kind=first;; esac
  case "$line" in
    *'"/compact'*)
      printf '{"type":"system","subtype":"status","status":"compacting","session_id":"sess-commands"}\n'
      printf '{"type":"system","subtype":"compact_boundary","session_id":"sess-commands","compact_metadata":{"trigger":"manual","pre_tokens":900,"post_tokens":120}}\n'
      printf '{"type":"system","subtype":"status","status":null,"compact_result":"success","session_id":"sess-commands"}\n';;
    *'"long task'*)
      printf '{"type":"system","subtype":"compact_boundary","session_id":"sess-commands","compact_metadata":{"trigger":"auto","pre_tokens":900,"post_tokens":150}}\n';;
    *'"doomed'*)
      printf '{"type":"system","subtype":"status","status":"compacting","session_id":"sess-commands"}\n'
      printf '{"type":"system","subtype":"status","status":null,"compact_result":"failed","compact_error":"prompt_too_long","session_id":"sess-commands"}\n';;
  esac
  printf '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"%s %s"}}}\n' "$run" "$kind"
  printf '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[]}}\n'
  printf '{"type":"result","subtype":"success","result":"ok","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-commands"}\n'
done
"#,
        )
        .expect("write fixture");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("make fixture executable");
        let store = new_store(directory.path(), cx);
        store.update(cx, |store, _| {
            store.harnesses.insert(
                AgentKind::Claude,
                Arc::new(ClaudeHarness::new().with_executable(script)),
            );
        });
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });

        for (message, expected) in [
            ("hello", "new first"),
            ("follow up", "new steer"),
            ("/compact", "new steer"),
            ("after compacting", "resumed first"),
            ("long task", "resumed steer"),
            ("after auto compaction", "resumed steer"),
            ("doomed", "resumed steer"),
        ] {
            let entries_before = session.read_with(cx, |session, _| session.entries().len());
            session.update(cx, |session, cx| session.send_message(message, cx));
            run_until(cx, |cx| {
                session.read_with(cx, |session, _| {
                    !session.is_working() && session.entries().len() > entries_before + 1
                })
            });
            assert_eq!(last_reply(&session, cx), expected, "reply to {message:?}");
        }
        session.read_with(cx, |session, _| {
            assert!(!session.is_compacting());
            assert_eq!(
                session.metadata.context.and_then(|context| context.tokens),
                Some(150)
            );
            let notices: Vec<(&str, bool)> = session
                .entries()
                .iter()
                .filter_map(|entry| match entry {
                    Entry::Notice { text, is_error } => Some((text.as_ref(), *is_error)),
                    _ => None,
                })
                .collect();
            assert_eq!(
                notices,
                [
                    ("Context compacted.", false),
                    ("Context compacted.", false),
                    ("Couldn't compact the conversation: prompt_too_long", true),
                ]
            );
        });
    }

    #[gpui::test]
    async fn agent_titles_replace_the_first_message_but_not_a_rename(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Opencode, directory.path().to_path_buf(), cx)
        });
        let prompt = "Can you look at why login redirects twice after the session expires?";
        let title_changed = |title: &str| AgentEvent::TitleChanged {
            title: title.into(),
        };
        let id = session.update(cx, |session, cx| {
            session
                .transcript
                .push_user(prompt.into(), Vec::new(), Vec::new());
            session.metadata.title = title_from(prompt);
            session.apply_event(title_changed("Fix double login redirect"), cx);
            assert_eq!(session.metadata.title, "Fix double login redirect");
            session.apply_event(title_changed("Something else"), cx);
            assert_eq!(session.metadata.title, "Fix double login redirect");
            session.metadata.title = title_from(prompt);
            session.metadata.id.clone()
        });
        store.update(cx, |store, cx| {
            store.rename_session(&id, "Login bug".into(), cx)
        });
        session.update(cx, |session, cx| {
            session.apply_event(title_changed("Fix double login redirect"), cx);
            assert_eq!(session.metadata.title, "Login bug");
        });
    }

    #[gpui::test]
    async fn compacting_label_follows_the_agent(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.working = true;
            session.apply_event(AgentEvent::Compacting { active: true }, cx);
            assert!(session.is_compacting());
            session.apply_event(AgentEvent::Compacting { active: false }, cx);
            assert!(!session.is_compacting());
            session.apply_event(AgentEvent::Compacting { active: true }, cx);
            session.apply_event(
                AgentEvent::Compacted {
                    tokens: None,
                    manual: false,
                },
                cx,
            );
            assert!(!session.is_compacting());
            assert!(!session.restart_on_next_send, "auto compaction keeps the process");
            assert_eq!(session.metadata.context.and_then(|context| context.tokens), None);
            session.apply_event(AgentEvent::Compacting { active: true }, cx);
            session.apply_event(
                AgentEvent::Done {
                    status: DoneStatus::Completed,
                    result: None,
                    error: None,
                    session_id: None,
                },
                cx,
            );
            assert!(!session.is_compacting());
        });
    }

    #[gpui::test]
    async fn commands_are_never_glued_to_messages_sent_while_stopping(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        use_quick_stopping_claude(&store, cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.send_message("scenario:interrupt", cx)
        });
        run_until(cx, |cx| {
            session.read_with(cx, |session, _| {
                session.metadata().native_session_id.is_some()
            })
        });
        session.update(cx, |session, cx| {
            session.stop(cx);
            session.send_message("/compact", cx);
            session.send_message("scenario:happy", cx);
            assert_eq!(
                session.prompt_after_stop.as_ref().map(|prompt| prompt.text.as_str()),
                Some("/compact")
            );
            let queued: Vec<&str> = session
                .queue()
                .iter()
                .map(|queued| queued.prompt.text.as_str())
                .collect();
            assert_eq!(queued, ["scenario:happy"]);
            assert!(!session.can_steer(&Prompt::from("/compact".to_string())));
        });
    }

    fn use_quick_stopping_claude(store: &Entity<AgentStore>, cx: &mut TestAppContext) {
        store.update(cx, |store, _| {
            store.harnesses.insert(
                AgentKind::Claude,
                Arc::new(
                    ClaudeHarness::new()
                        .with_executable(fake_claude())
                        .with_graces(Duration::from_millis(50), Duration::from_millis(50)),
                ),
            );
        });
    }

    #[gpui::test]
    async fn message_sent_while_stopping_runs_after_the_stop(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        use_quick_stopping_claude(&store, cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.send_message("scenario:interrupt", cx)
        });
        run_until(cx, |cx| {
            session.read_with(cx, |session, _| {
                session.metadata().native_session_id.is_some()
            })
        });
        session.update(cx, |session, cx| {
            session.stop(cx);
            session.send_message("scenario:happy", cx);
        });
        run_until(cx, |cx| {
            session.read_with(cx, |session, _| !session.is_working())
                && last_reply(&session, cx) == "Hello"
        });
    }

    #[gpui::test]
    async fn deleting_a_running_chat_removes_its_files_for_good(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        use_quick_stopping_claude(&store, cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.send_message("scenario:interrupt", cx)
        });
        run_until(cx, |cx| {
            session.read_with(cx, |session, _| {
                session.metadata().native_session_id.is_some()
            })
        });
        let id = session_id(&session, cx);
        let metadata_file = metadata_path(directory.path(), &id);
        run_until(cx, |_| metadata_file.exists());

        store.update(cx, |store, cx| store.delete_session(&id, cx));
        session.update(cx, |session, cx| {
            session.apply_event(
                AgentEvent::ContextUsage {
                    tokens: Some(1),
                    window: Some(2),
                },
                cx,
            );
            session.send_message("scenario:happy", cx);
        });
        cx.executor().advance_clock(SAVE_DEBOUNCE * 2);
        run_until(cx, |_| !metadata_file.exists());
        cx.executor().advance_clock(SAVE_DEBOUNCE * 2);
        cx.run_until_parked();
        assert!(!metadata_file.exists());
        assert!(!entries_path(directory.path(), &id).exists());
        session.read_with(cx, |session, _| assert!(!session.is_working()));
    }

    #[gpui::test]
    async fn a_steer_started_after_a_turn_ends_counts_as_working(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Codex, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.apply_event(
                AgentEvent::Done {
                    status: DoneStatus::Completed,
                    result: None,
                    error: None,
                    session_id: None,
                },
                cx,
            );
            assert!(!session.is_working());
            session.apply_event(
                AgentEvent::Steered {
                    assistant_message_id: None,
                    next_assistant_message_id: None,
                },
                cx,
            );
            assert!(session.is_working());
        });
    }

    #[gpui::test]
    async fn the_computer_stays_awake_while_an_agent_works(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Codex, directory.path().to_path_buf(), cx)
        });
        let set_working = |working: bool, cx: &mut TestAppContext| {
            session.update(cx, |session, cx| {
                let event = if working {
                    AgentEvent::Steered {
                        assistant_message_id: None,
                        next_assistant_message_id: None,
                    }
                } else {
                    AgentEvent::Done {
                        status: DoneStatus::Completed,
                        result: None,
                        error: None,
                        session_id: None,
                    }
                };
                session.apply_event(event, cx);
            });
            cx.run_until_parked();
        };
        assert_eq!(cx.active_idle_sleep_preventions(), 0);

        set_working(true, cx);
        assert_eq!(cx.active_idle_sleep_preventions(), 1);

        update_chat_settings(cx, |settings| settings.prevent_idle_sleep = Some(false));
        cx.run_until_parked();
        assert_eq!(cx.active_idle_sleep_preventions(), 0);

        update_chat_settings(cx, |settings| settings.prevent_idle_sleep = Some(true));
        cx.run_until_parked();
        assert_eq!(cx.active_idle_sleep_preventions(), 1);

        session.update(cx, |session, cx| {
            session.pending_questions.push(PendingQuestion {
                questions: Vec::new(),
                responder: None,
                generation: 0,
                deadline: None,
            });
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(cx.active_idle_sleep_preventions(), 0);

        session.update(cx, |session, cx| {
            session.pending_questions.clear();
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(cx.active_idle_sleep_preventions(), 1);

        set_working(false, cx);
        assert_eq!(cx.active_idle_sleep_preventions(), 0);
    }

    #[gpui::test]
    async fn a_stopped_message_can_be_edited_until_the_agent_changes_something(
        cx: &mut TestAppContext,
    ) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let stopped_turn = |session: &Entity<AgentSession>, prompts, call, cx: &mut _| {
            feed_stopped_turn(session, "fix teh bug", prompts, call, cx)
        };
        let read = || ToolCall::ReadFile {
            path: "src/main.rs".into(),
        };
        let editable = |cx: &mut TestAppContext| {
            session.read_with(cx, |session, _| session.editable_message())
        };

        stopped_turn(&session, 1, read(), cx);
        assert_eq!(editable(cx), Some(0));
        let prompt = session.update(cx, |session, cx| session.edit_last_message(cx));
        assert_eq!(
            prompt.map(|prompt| prompt.text),
            Some("fix teh bug".to_string())
        );
        session.read_with(cx, |session, _| {
            assert!(session.entries().is_empty());
            assert_eq!(
                session.metadata().dropped_prompt.as_deref(),
                Some("prompt-1")
            );
            assert_eq!(session.metadata().title, "");
        });

        session.update(cx, |session, _| session.transcript.entries.clear());
        stopped_turn(
            &session,
            1,
            ToolCall::Exec {
                command: "rm -rf build".into(),
            },
            cx,
        );
        assert_eq!(editable(cx), None);

        session.update(cx, |session, _| session.transcript.entries.clear());
        stopped_turn(&session, 2, read(), cx);
        assert_eq!(
            editable(cx),
            None,
            "a steered turn holds more than the last message"
        );

        session.update(cx, |session, _| session.transcript.entries.clear());
        stopped_turn(&session, 1, read(), cx);
        session.update(cx, |session, cx| session.send_message("scenario:happy", cx));
        assert_eq!(editable(cx), None);
        session.read_with(cx, |session, _| {
            assert_eq!(
                session.metadata().dropped_prompt,
                None,
                "the next run forgets it"
            );
        });
    }

    #[gpui::test]
    async fn a_turn_the_agent_starts_on_its_own_counts_as_working(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let done = || AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: None,
        };
        session.update(cx, |session, cx| {
            for wake in [
                AgentEvent::TextDelta {
                    text: "The build finished.".into(),
                },
                AgentEvent::ReasoningDelta {
                    text: "Checking the output".into(),
                },
                AgentEvent::ToolCall {
                    id: "tool-1".into(),
                    call: ToolCall::Exec {
                        command: "cat build.log".into(),
                    },
                },
            ] {
                session.apply_event(done(), cx);
                assert!(!session.is_working());
                session.apply_event(wake, cx);
                assert!(session.is_working());
                assert!(session.working_since().is_some());
            }
            session.apply_event(done(), cx);
            session.apply_event(
                AgentEvent::TaskFinished {
                    task_id: "task-1".into(),
                    status: DoneStatus::Completed,
                },
                cx,
            );
            session.apply_event(
                AgentEvent::ContextUsage {
                    tokens: Some(10),
                    window: None,
                },
                cx,
            );
            assert!(!session.is_working());
        });
    }

    #[gpui::test]
    async fn a_chat_deleted_while_opening_stays_deleted(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let id = store.update(cx, |store, cx| {
            let session =
                store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx);
            session.read(cx).metadata().id.clone()
        });
        store.update(cx, |store, _| store.live.clear());
        let opening = store.update(cx, |store, cx| store.open_session(&id, cx));
        store.update(cx, |store, cx| store.delete_session(&id, cx));
        assert!(opening.await.is_err());
        store.read_with(cx, |store, _| assert!(!store.live.contains_key(&id)));
    }

    #[gpui::test]
    async fn background_tasks_follow_the_live_list(cx: &mut TestAppContext) {
        use agent_harness::{BackgroundTaskInfo, BackgroundTaskKind};
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let info = |task_id: &str, kind| BackgroundTaskInfo {
            task_id: task_id.into(),
            kind,
            description: String::new(),
        };
        feed_events(
            &session,
            vec![
                AgentEvent::TaskStarted {
                    task_id: "fg1".into(),
                    tool_use_id: Some("toolu_fg".into()),
                    kind: BackgroundTaskKind::Shell,
                    description: "Foreground command".into(),
                },
                AgentEvent::TaskStarted {
                    task_id: "bg1".into(),
                    tool_use_id: Some("toolu_bg".into()),
                    kind: BackgroundTaskKind::Shell,
                    description: "Run tests".into(),
                },
            ],
            cx,
        );
        let tasks = |cx: &mut TestAppContext| {
            session.read_with(cx, |session, _| {
                session
                    .background_tasks()
                    .iter()
                    .map(|task| (task.task_id.clone(), task.description.clone(), task.tool_use_id.clone()))
                    .collect::<Vec<_>>()
            })
        };
        assert!(tasks(cx).is_empty(), "started tasks are not background work by themselves");
        feed_events(
            &session,
            vec![AgentEvent::BackgroundTasksChanged {
                tasks: vec![
                    info("bg1", BackgroundTaskKind::Shell),
                    info("a1", BackgroundTaskKind::Agent),
                ],
            }],
            cx,
        );
        assert_eq!(
            tasks(cx),
            [
                ("bg1".into(), "Run tests".into(), Some("toolu_bg".into())),
                ("a1".into(), String::new(), None),
            ]
        );
        feed_events(
            &session,
            vec![AgentEvent::TaskStarted {
                task_id: "a1".into(),
                tool_use_id: Some("toolu_agent".into()),
                kind: BackgroundTaskKind::Agent,
                description: "Explore".into(),
            }],
            cx,
        );
        assert_eq!(
            tasks(cx)[1],
            ("a1".into(), "Explore".into(), Some("toolu_agent".into())),
            "a start message after the list fills in the task"
        );
        feed_events(
            &session,
            vec![AgentEvent::BackgroundTasksChanged {
                tasks: vec![info("a1", BackgroundTaskKind::Agent)],
            }],
            cx,
        );
        assert_eq!(tasks(cx).len(), 1, "the list replaces the set even without an end message");
        feed_events(
            &session,
            vec![AgentEvent::TaskFinished {
                task_id: "a1".into(),
                status: DoneStatus::Completed,
            }],
            cx,
        );
        assert!(tasks(cx).is_empty());
        feed_events(
            &session,
            vec![AgentEvent::BackgroundTasksChanged {
                tasks: vec![info("bg2", BackgroundTaskKind::Shell)],
            }],
            cx,
        );
        session.update(cx, |session, cx| session.run_ended(cx));
        assert!(tasks(cx).is_empty(), "tasks end with the agent process");
    }

    #[gpui::test]
    async fn empty_reasoning_never_adds_a_thought(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        feed_events(
            &session,
            vec![
                AgentEvent::ReasoningDelta {
                    text: String::new(),
                },
                AgentEvent::TextDelta {
                    text: "one ".into(),
                },
                AgentEvent::ReasoningDelta {
                    text: String::new(),
                },
                AgentEvent::TextDelta { text: "two".into() },
            ],
            cx,
        );
        assert_eq!(last_reply(&session, cx), "one two");
        session.read_with(cx, |session, _| {
            assert_eq!(session.entries().len(), 1);
        });
    }

    pub(crate) fn feed_user(session: &Entity<AgentSession>, text: &str, cx: &mut TestAppContext) {
        session.update(cx, |session, cx| {
            session
                .transcript
                .push_user(text.to_string(), Vec::new(), Vec::new());
            cx.notify();
        });
    }

    pub(crate) fn feed_events(
        session: &Entity<AgentSession>,
        events: Vec<AgentEvent>,
        cx: &mut TestAppContext,
    ) {
        session.update(cx, |session, cx| {
            session.working = true;
            for event in events {
                session.apply_event(event, cx);
            }
        });
    }

    pub(crate) fn feed_stopped_turn(
        session: &Entity<AgentSession>,
        text: &str,
        prompts: usize,
        call: ToolCall,
        cx: &mut TestAppContext,
    ) {
        session.update(cx, |session, _| {
            session
                .transcript
                .push_user(text.into(), Vec::new(), Vec::new());
            session.metadata.title = title_from(text);
            session.turn_prompts = prompts;
            session.turn_prompt_id = None;
            session.editable_prompt = None;
            session.user_stopped = true;
        });
        feed_events(
            session,
            vec![
                AgentEvent::PromptRecorded {
                    id: "prompt-1".into(),
                },
                AgentEvent::PromptRecorded {
                    id: "prompt-2".into(),
                },
                AgentEvent::ToolCall {
                    id: "tool-1".into(),
                    call,
                },
                AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    result: None,
                    error: None,
                    session_id: None,
                },
            ],
            cx,
        );
    }

    pub(crate) fn feed_subagent(session: &Entity<AgentSession>, cx: &mut TestAppContext) {
        let child = |event| AgentEvent::Subagent {
            parent_tool_use_id: "spawn-1".into(),
            event: Box::new(event),
        };
        session.update(cx, |session, cx| {
            session.working = true;
            for event in [
                AgentEvent::ToolCall {
                    id: "spawn-1".into(),
                    call: ToolCall::Unknown {
                        name: "Agent: scan the repo".into(),
                        input: None,
                    },
                },
                child(AgentEvent::UserMessage {
                    text: "Scan the repo".into(),
                }),
                child(AgentEvent::TextDelta {
                    text: "Looking".into(),
                }),
                child(AgentEvent::ToolCall {
                    id: "child-tool".into(),
                    call: ToolCall::Exec {
                        command: "ls".into(),
                    },
                }),
            ] {
                session.apply_event(event, cx);
            }
        });
    }

    fn summarize(entries: &[Entry], cx: &App) -> Vec<String> {
        entries
            .iter()
            .map(|entry| match entry {
                Entry::User { text, .. } => format!("user: {text}"),
                Entry::Assistant { markdown, .. } => {
                    format!("assistant: {}", markdown.read(cx).source())
                }
                Entry::Thinking(markdown) => format!("thinking: {}", markdown.read(cx).source()),
                Entry::Tool(tool) => format!("tool {}: {:?}", tool.id, tool.status),
                Entry::Notice { text, .. } => format!("notice: {text}"),
                Entry::Image { path } => format!("image: {}", path.display()),
            })
            .collect()
    }

    #[gpui::test]
    async fn subagent_activity_gets_its_own_saved_transcript(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        feed_subagent(&session, cx);
        session.read_with(cx, |session, cx| {
            assert_eq!(summarize(session.entries(), cx), ["tool spawn-1: Running"]);
            let subagent = session.subagent("spawn-1").expect("subagent recorded");
            assert_eq!(
                summarize(subagent.entries(), cx),
                [
                    "user: Scan the repo",
                    "assistant: Looking",
                    "tool child-tool: Running"
                ]
            );
            assert_eq!(subagent.status(), None);
        });
        session.update(cx, |session, cx| {
            session.apply_event(
                AgentEvent::Subagent {
                    parent_tool_use_id: "spawn-1".into(),
                    event: Box::new(AgentEvent::Done {
                        status: DoneStatus::Completed,
                        result: None,
                        error: None,
                        session_id: None,
                    }),
                },
                cx,
            );
            assert!(!session.subagent_running("spawn-1"));
        });

        let id = session_id(&session, cx);
        session.update(cx, |session, cx| session.touch(cx));
        cx.executor().advance_clock(SAVE_DEBOUNCE * 2);
        cx.run_until_parked();
        let reopened_store = new_store(directory.path(), cx);
        cx.run_until_parked();
        let reopened = reopened_store
            .update(cx, |store, cx| store.open_session(&id, cx))
            .await
            .expect("chat reopens");
        reopened.read_with(cx, |session, cx| {
            let subagent = session.subagent("spawn-1").expect("subagent saved");
            assert_eq!(subagent.status(), Some(DoneStatus::Completed));
            assert_eq!(
                summarize(subagent.entries(), cx),
                [
                    "user: Scan the repo",
                    "assistant: Looking",
                    "tool child-tool: Canceled"
                ]
            );
        });
    }

    #[gpui::test]
    async fn a_background_subagent_runs_after_the_turn_that_spawned_it(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        feed_subagent(&session, cx);
        session.update(cx, |session, cx| {
            let (steering, _) = tokio::sync::mpsc::channel(1);
            session.run = Some(ActiveRun {
                generation: 0,
                round_done: false,
                steering,
                interrupt: CancellationToken::new(),
                permission: tokio::sync::watch::channel(PermissionMode::Ask).0,
                _events: Task::ready(()),
            });
            for event in [
                AgentEvent::TaskStarted {
                    task_id: "agent-task".into(),
                    tool_use_id: Some("spawn-1".into()),
                    kind: agent_harness::BackgroundTaskKind::Agent,
                    description: "Scan the repo".into(),
                },
                AgentEvent::ToolResult {
                    id: "spawn-1".into(),
                    is_error: false,
                    output: Some("Started in the background".into()),
                    diff: None,
                },
                AgentEvent::Done {
                    status: DoneStatus::Completed,
                    result: None,
                    error: None,
                    session_id: None,
                },
            ] {
                session.apply_event(event, cx);
            }
            assert!(!session.is_working());
            assert!(session.subagent_running("spawn-1"));
            session.apply_event(
                AgentEvent::TaskFinished {
                    task_id: "agent-task".into(),
                    status: DoneStatus::Completed,
                },
                cx,
            );
            assert!(!session.subagent_running("spawn-1"));
        });
    }

    #[gpui::test]
    async fn claude_background_subagent_traffic_lands_in_its_transcript(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.send_message("scenario:wake", cx)
        });
        run_until(cx, |cx| {
            session.read_with(cx, |session, _| session.run.is_none() && !session.is_working())
        });
        session.read_with(cx, |session, cx| {
            let subagent = session.subagent("toolu_agent").expect("subagent recorded");
            assert_eq!(
                summarize(subagent.entries(), cx),
                ["assistant: sub working", "tool sub-t1: Completed"]
            );
            assert!(!summarize(session.entries(), cx).contains(&"assistant: sub working".into()));
        });
    }

    #[test]
    fn transcripts_saved_before_subagents_still_load() {
        let saved: SavedTranscript =
            serde_json::from_str(r#"[{"type":"user","text":"hi","at":1}]"#).expect("parses");
        assert!(matches!(saved, SavedTranscript::EntriesOnly(entries) if entries.len() == 1));
        let saved: SavedTranscript =
            serde_json::from_str(r#"{"entries":[],"subagents":[{"id":"a","entries":[]}]}"#)
                .expect("parses");
        assert!(
            matches!(saved, SavedTranscript::Current { subagents, .. } if subagents.len() == 1)
        );
        let saved: SavedTranscript =
            serde_json::from_str(r#"{"entries":[],"queue":[{"text":"later"}]}"#).expect("parses");
        assert!(matches!(saved, SavedTranscript::Current { queue, .. } if queue.len() == 1));
    }

    fn user_texts(session: &Entity<AgentSession>, cx: &mut TestAppContext) -> Vec<String> {
        session.read_with(cx, |session, _| {
            session
                .entries()
                .iter()
                .filter_map(|entry| match entry {
                    Entry::User { text, .. } => Some(text.to_string()),
                    _ => None,
                })
                .collect()
        })
    }

    fn update_chat_settings(
        cx: &mut TestAppContext,
        update: impl FnOnce(&mut settings::AgentChatSettingsContent),
    ) {
        cx.update(|cx| {
            <settings::SettingsStore as gpui::UpdateGlobal>::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |content| {
                    update(content.agent_chat.get_or_insert_default())
                })
            })
        });
    }

    #[gpui::test]
    async fn new_chats_start_with_the_default_effort_and_permission(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let remembered = RunSettings {
            model: Some(ModelSelection::Native { id: "opus".into() }),
            reasoning: Some(ReasoningLevel::Low),
            permission: PermissionMode::Ask,
            ..RunSettings::default()
        };
        let remembered_without_permission = RunSettings {
            permission: PermissionMode::Auto,
            ..remembered.clone()
        };
        store.update(cx, |store, cx| {
            store.remember_settings(AgentKind::Claude, remembered.clone(), cx);
            store.remember_settings(AgentKind::Codex, remembered.clone(), cx);
        });
        let new_chat_settings = |kind, cx: &mut TestAppContext| {
            store.update(cx, |store, cx| {
                let session = store.create_session(kind, directory.path().to_path_buf(), cx);
                session.read(cx).settings().clone()
            })
        };
        assert_eq!(
            new_chat_settings(AgentKind::Claude, cx),
            remembered_without_permission
        );

        update_chat_settings(cx, |content| {
            content.default_effort = Some(settings::AgentChatDefaultEffort::High);
            content.default_permission = Some(settings::AgentChatDefaultPermissionContent {
                claude_code: Some(settings::ClaudeCodePermissionDefault::Bypass),
                ..Default::default()
            });
        });
        let claude = new_chat_settings(AgentKind::Claude, cx);
        assert_eq!(claude.native_model(), Some("opus"));
        assert_eq!(claude.reasoning, Some(ReasoningLevel::High));
        assert_eq!(claude.permission, PermissionMode::FullAccess);
        let codex = new_chat_settings(AgentKind::Codex, cx);
        assert_eq!(codex.reasoning, Some(ReasoningLevel::High));
        assert_eq!(codex.permission, PermissionMode::Auto);
    }

    #[test]
    fn effort_the_model_does_not_offer_falls_back_to_the_model_default() {
        let model = |id: &str, levels: Vec<ReasoningLevel>| Model {
            id: id.into(),
            label: id.into(),
            description: None,
            reasoning_levels: levels,
            options: Vec::new(),
        };
        let models = [
            model(
                "gpt",
                vec![
                    ReasoningLevel::Low,
                    ReasoningLevel::Medium,
                    ReasoningLevel::High,
                ],
            ),
            model("haiku", Vec::new()),
        ];
        let settings = |model: &str, reasoning| RunSettings {
            model: Some(ModelSelection::Native { id: model.into() }),
            reasoning: Some(reasoning),
            ..RunSettings::default()
        };
        assert_eq!(
            reasoning_for_model(&settings("gpt", ReasoningLevel::Max), &models),
            Some(Some(ReasoningLevel::High))
        );
        assert_eq!(
            reasoning_for_model(&settings("gpt", ReasoningLevel::Low), &models),
            None
        );
        assert_eq!(
            reasoning_for_model(&settings("haiku", ReasoningLevel::High), &models),
            Some(None)
        );
        assert_eq!(
            reasoning_for_model(&settings("custom", ReasoningLevel::Max), &models),
            None
        );
    }

    #[gpui::test]
    async fn messages_sent_while_working_steer_when_set_to(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        use_quick_stopping_claude(&store, cx);
        update_chat_settings(cx, |content| {
            content.while_working = Some(settings::AgentChatWhileWorking::Steer);
        });
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.send_message("scenario:interrupt", cx)
        });
        run_until(cx, |cx| {
            session.read_with(cx, |session, _| session.run.is_some())
        });
        session.update(cx, |session, cx| session.send_message("change of plan", cx));
        assert!(session.read_with(cx, |session, _| session.queue().is_empty()));
        assert_eq!(
            user_texts(&session, cx),
            ["scenario:interrupt", "change of plan"]
        );
    }

    #[gpui::test]
    async fn messages_sent_while_working_queue_and_go_out_after_the_turn(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        use_quick_stopping_claude(&store, cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| session.send_message("scenario:interrupt", cx));
        run_until(cx, |cx| session.read_with(cx, |session, _| session.run.is_some()));
        session.update(cx, |session, cx| {
            session.send_message("first queued", cx);
            session.send_message("second queued", cx);
            session.send_message("third queued", cx);
        });
        let ids: Vec<u64> = session.read_with(cx, |session, _| {
            session.queue().iter().map(|queued| queued.id).collect()
        });
        assert_eq!(ids.len(), 3);
        assert_eq!(user_texts(&session, cx), ["scenario:interrupt"]);
        session.update(cx, |session, cx| {
            session.move_queued(ids[2], 0, cx);
            session.remove_queued(ids[1], cx);
        });
        let prompt = session
            .update(cx, |session, cx| session.begin_queued_edit(ids[0], cx))
            .expect("queued message");
        session.update(cx, |session, cx| {
            session.finish_queued_edit(
                ids[0],
                Some(Prompt::text(format!("{} edited", prompt.text))),
                cx,
            )
        });
        let queued: Vec<String> = session.read_with(cx, |session, _| {
            session.queue().iter().map(|queued| queued.prompt.text.clone()).collect()
        });
        assert_eq!(queued, ["third queued", "first queued edited"]);

        session.update(cx, |session, cx| {
            session.apply_event(
                AgentEvent::Done {
                    status: DoneStatus::Completed,
                    result: None,
                    error: None,
                    session_id: None,
                },
                cx,
            );
        });
        assert_eq!(
            user_texts(&session, cx),
            ["scenario:interrupt", "third queued"]
        );
        session.read_with(cx, |session, _| {
            assert_eq!(session.queue().len(), 1);
            assert!(session.is_working());
        });
    }

    #[gpui::test]
    async fn send_now_shows_the_message_and_queue_order_survives_idle(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        use_quick_stopping_claude(&store, cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| session.send_message("scenario:interrupt", cx));
        run_until(cx, |cx| session.read_with(cx, |session, _| session.run.is_some()));
        session.update(cx, |session, cx| session.send_message("scenario:happy", cx));
        let id = session.read_with(cx, |session, _| session.queue()[0].id);
        session.update(cx, |session, cx| session.send_queued_now(id, cx));
        assert_eq!(
            user_texts(&session, cx),
            ["scenario:interrupt", "scenario:happy"]
        );
        run_until(cx, |cx| {
            session.read_with(cx, |session, _| !session.is_working())
                && last_reply(&session, cx) == "Hello"
        });

        session.update(cx, |session, cx| {
            session.enqueue(Prompt::text("older"), cx);
            session.send_message("newer", cx);
        });
        assert_eq!(user_texts(&session, cx).last().map(String::as_str), Some("older"));
        let queued: Vec<String> = session.read_with(cx, |session, _| {
            session.queue().iter().map(|queued| queued.prompt.text.clone()).collect()
        });
        assert_eq!(queued, ["newer"]);
    }

    #[gpui::test]
    async fn the_queue_continues_after_a_failed_turn_but_not_after_stop(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let done = |status| AgentEvent::Done {
            status,
            result: None,
            error: None,
            session_id: None,
        };
        session.update(cx, |session, cx| {
            session.enqueue(Prompt::text("after error"), cx);
            session.working = true;
            session.apply_event(done(DoneStatus::Errored), cx);
        });
        assert_eq!(user_texts(&session, cx).last().map(String::as_str), Some("after error"));

        session.update(cx, |session, cx| {
            session.enqueue(Prompt::text("after stop"), cx);
            session.stop(cx);
            session.working = true;
            session.apply_event(done(DoneStatus::Interrupted), cx);
            session.run_ended(cx);
        });
        session.read_with(cx, |session, _| assert_eq!(session.queue().len(), 1));
    }

    pub(crate) fn enqueue(session: &Entity<AgentSession>, text: &str, cx: &mut TestAppContext) {
        session.update(cx, |session, cx| session.enqueue(Prompt::text(text), cx));
    }

    fn saved_queue(session: &Entity<AgentSession>, cx: &mut TestAppContext) -> Vec<String> {
        session.read_with(cx, |session, cx| match session.saved_transcript(cx) {
            SavedTranscript::Current { queue, .. } => {
                queue.into_iter().map(|prompt| prompt.text).collect()
            }
            SavedTranscript::EntriesOnly(_) => Vec::new(),
        })
    }

    #[gpui::test]
    async fn a_queued_message_being_edited_stays_saved_and_is_not_sent(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        enqueue(&session, "being edited", cx);
        enqueue(&session, "next up", cx);
        let edited_id = session.read_with(cx, |session, _| session.queue()[0].id);
        let prompt = session
            .update(cx, |session, cx| session.begin_queued_edit(edited_id, cx))
            .expect("queued message");
        assert_eq!(prompt.text, "being edited");
        assert_eq!(saved_queue(&session, cx), ["being edited", "next up"]);

        session.update(cx, |session, cx| {
            session.working = true;
            session.apply_event(
                AgentEvent::Done {
                    status: DoneStatus::Completed,
                    result: None,
                    error: None,
                    session_id: None,
                },
                cx,
            );
        });
        assert_eq!(user_texts(&session, cx).last().map(String::as_str), Some("next up"));
        assert_eq!(saved_queue(&session, cx), ["being edited"]);

        session.update(cx, |session, cx| {
            session.finish_queued_edit(edited_id, Some(Prompt::text("  ")), cx)
        });
        session.read_with(cx, |session, _| assert_eq!(session.editing_queued(), None));
        assert_eq!(saved_queue(&session, cx), ["being edited"]);
    }

    #[gpui::test]
    async fn retrying_keeps_unrelated_errors(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.transcript.push_notice("older crash".into(), true);
            session
                .transcript
                .push_user("failed".into(), Vec::new(), Vec::new());
            session.fail_run("could not start".into(), cx);
        });
        let index = session.read_with(cx, |session, _| {
            session
                .entries()
                .iter()
                .position(|entry| matches!(entry, Entry::User { undelivered: true, .. }))
                .expect("undelivered message")
        });
        session.update(cx, |session, cx| session.retry_undelivered(index, cx));
        session.read_with(cx, |session, cx| {
            let summary = summarize(session.entries(), cx);
            assert_eq!(summary[0], "notice: older crash");
            assert!(!summary.contains(&"notice: could not start".to_string()));
            assert_eq!(summary.iter().filter(|line| *line == "user: failed").count(), 1);
        });
    }

    #[gpui::test]
    async fn pins_and_names_are_saved_without_opening_the_chat(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let id = store.update(cx, |store, cx| {
            let session = store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx);
            session.update(cx, |session, cx| session.touch(cx));
            session.read(cx).metadata().id.clone()
        });
        store.update(cx, |store, _| store.live.clear());
        store.update(cx, |store, cx| {
            store.rename_session(&id, "  Renamed  ".into(), cx);
            store.set_pinned(&id, true, cx);
        });
        cx.run_until_parked();
        let reopened = new_store(directory.path(), cx);
        cx.run_until_parked();
        reopened.read_with(cx, |store, cx| {
            let summary = &store.sessions_in(&[directory.path().to_path_buf()], cx)[0];
            assert_eq!(summary.title.as_ref(), "Renamed");
            assert!(summary.pinned);
        });
    }

    #[gpui::test]
    async fn saved_messages_toggle_persist_and_leave_with_their_chat(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        cx.run_until_parked();
        let id = store.update(cx, |store, cx| {
            let session =
                store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx);
            session.read(cx).metadata().id.clone()
        });
        store.update(cx, |store, cx| {
            store.toggle_saved_message(&id, 0, "fix the bug".into(), false, cx);
            store.toggle_saved_message(&id, 1, "Fixed it.".into(), true, cx);
            store.toggle_saved_message(&id, 1, "Fixed it.".into(), true, cx);
            store.toggle_saved_message(&id, 3, "Old reply".into(), true, cx);
            store.toggle_saved_message(&id, 3, "Old reply, then more".into(), true, cx);
        });
        cx.run_until_parked();
        let saved = |store: &Entity<AgentStore>, cx: &mut TestAppContext| {
            store.read_with(cx, |store, _| {
                store
                    .saved_messages()
                    .iter()
                    .map(|message| (message.entry, message.text.clone()))
                    .collect::<Vec<_>>()
            })
        };
        let expected = vec![
            (0, "fix the bug".to_string()),
            (3, "Old reply, then more".to_string()),
        ];
        assert_eq!(saved(&store, cx), expected);
        let reopened = new_store(directory.path(), cx);
        cx.run_until_parked();
        assert_eq!(saved(&reopened, cx), expected);

        reopened.update(cx, |store, cx| store.delete_session(&id, cx));
        assert!(saved(&reopened, cx).is_empty());
    }

    #[gpui::test]
    async fn messages_saved_with_the_chat_list_move_to_their_own_file(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let saved = serde_json::json!({
            "chat_id": "a",
            "entry": 2,
            "text": "Fixed it.",
            "from_agent": true,
            "saved_at": 1,
        });
        std::fs::write(
            chat_list_path(directory.path()),
            serde_json::json!({ "compact_rows": true, "saved_messages": [saved] }).to_string(),
        )
        .expect("chat list");
        let store = new_store(directory.path(), cx);
        cx.run_until_parked();
        store.read_with(cx, |store, _| {
            assert!(store.list_prefs().compact_rows);
            assert_eq!(store.saved_text("a", 2), Some("Fixed it."));
        });
        let file = |path: PathBuf| std::fs::read_to_string(path).expect("written");
        assert!(file(saved_messages_path(directory.path())).contains("Fixed it."));
        assert!(!file(chat_list_path(directory.path())).contains("Fixed it."));
    }

    #[test]
    fn chats_and_list_settings_saved_with_sections_or_archived_still_load() {
        let metadata: SessionMetadata = serde_json::from_value(serde_json::json!({
            "id": "a",
            "kind": "claude",
            "title": "",
            "cwd": "/tmp",
            "created_at": 1,
            "updated_at": 1,
            "native_session_id": null,
            "settings": {},
            "section": "old-section",
            "archived": true,
        }))
        .expect("old metadata loads");
        assert_eq!(metadata.id, "a");
        let prefs: ChatListPrefs = serde_json::from_value(serde_json::json!({
            "sections": [{ "id": "old-section", "name": "Later" }],
            "compact_rows": true,
        }))
        .expect("old list settings load");
        assert!(prefs.compact_rows);
    }

    #[gpui::test]
    async fn saved_chats_load_their_history_and_keep_its_context(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let exec = |command: &str| ToolCall::Exec {
            command: command.into(),
        };
        session.update(cx, |session, cx| {
            session.load_external_history(
                vec![
                    AgentEvent::UserMessage {
                        text: "Fix the bug".into(),
                    },
                    AgentEvent::ReasoningDelta {
                        text: "Check the tests".into(),
                    },
                    AgentEvent::ToolCall {
                        id: "t1".into(),
                        call: exec("cargo test"),
                    },
                    AgentEvent::ToolResult {
                        id: "t1".into(),
                        is_error: false,
                        output: None,
                        diff: None,
                    },
                    AgentEvent::TextDelta {
                        text: "Fixed.".into(),
                    },
                    AgentEvent::AssistantMessageCompleted {
                        assistant_message_id: String::new(),
                    },
                    AgentEvent::ToolCall {
                        id: "t2".into(),
                        call: exec("git push"),
                    },
                ],
                "Claude Code".into(),
                cx,
            )
        });
        session.read_with(cx, |session, cx| {
            assert_eq!(
                summarize(session.entries(), cx),
                [
                    "user: Fix the bug",
                    "thinking: Check the tests",
                    "tool t1: Completed",
                    "assistant: Fixed.",
                    "tool t2: Canceled",
                    "notice: Continued from a saved Claude Code chat",
                ]
            );
            let context = session.metadata().fork_context.clone().expect("history");
            assert!(context.contains("Fix the bug") && context.contains("Fixed."));
            assert!(!session.is_working());
        });
    }

    #[gpui::test]
    async fn forks_get_the_next_free_number(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let parent = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let parent_id = session_id(&parent, cx);
        store.update(cx, |store, cx| {
            store.rename_session(&parent_id, "Chat".to_string(), cx)
        });

        let mut titles = Vec::new();
        for _ in 0..2 {
            let fork = store
                .update(cx, |store, cx| store.fork_session(&parent_id, false, cx))
                .await
                .expect("fork");
            titles.push(fork.read_with(cx, |fork, _| fork.metadata().title.clone()));
        }
        let fork_of_fork_id = store
            .update(cx, |store, cx| store.fork_session(&parent_id, false, cx))
            .await
            .map(|fork| session_id(&fork, cx))
            .expect("fork");
        let fork_of_fork = store
            .update(cx, |store, cx| store.fork_session(&fork_of_fork_id, false, cx))
            .await
            .expect("fork of fork");
        titles.push(fork_of_fork.read_with(cx, |fork, _| fork.metadata().title.clone()));

        assert_eq!(titles, ["Chat (2)", "Chat (3)", "Chat (5)"]);
    }

    #[gpui::test]
    async fn unlisted_models_stay_pickable_after_switching_away(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let ids = |selected: &str, cx: &mut TestAppContext| {
            store.read_with(cx, |store, _| {
                store
                    .picker_models(AgentKind::Codex, Some(selected))
                    .into_iter()
                    .map(|model| model.id)
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(ids("gpt-6.1-sol", cx), ["gpt-6.1-sol"]);

        store.update(cx, |store, _| {
            store.remember_unlisted_model(AgentKind::Codex, "gpt-6.1-sol")
        });
        assert_eq!(ids("gpt-6-astra", cx), ["gpt-6-astra", "gpt-6.1-sol"]);
        assert_eq!(ids("gpt-6.1-sol", cx), ["gpt-6.1-sol"]);
    }

    #[gpui::test]
    async fn forks_copy_history_and_send_it_with_their_first_message(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let parent = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        parent.update(cx, |session, cx| session.send_message("scenario:happy", cx));
        run_until(cx, |cx| parent.read_with(cx, |session, _| !session.is_working()));
        parent.update(cx, |session, cx| {
            session.update_settings(
                |settings| settings.permission = PermissionMode::FullAccess,
                cx,
            )
        });
        let parent_id = session_id(&parent, cx);

        let fork = store
            .update(cx, |store, cx| store.fork_session(&parent_id, false, cx))
            .await
            .expect("fork");
        fork.read_with(cx, |fork, cx| {
            let summary = summarize(fork.entries(), cx);
            assert_eq!(summary.first().map(String::as_str), Some("user: scenario:happy"));
            assert!(summary.last().is_some_and(|last| last.starts_with("notice: Forked from")));
            let context = fork.metadata().fork_context.clone().expect("history");
            assert!(context.contains("scenario:happy") && context.contains("Hello"));
            assert_eq!(fork.metadata().parent_id.as_deref(), Some(parent_id.as_str()));
            assert!(!fork.metadata().side_chat);
            assert_eq!(fork.settings().permission, PermissionMode::FullAccess);
        });

        let side = store
            .update(cx, |store, cx| store.fork_session(&parent_id, true, cx))
            .await
            .expect("side chat");
        side.read_with(cx, |side, cx| {
            assert_eq!(summarize(side.entries(), cx).len(), 1);
            assert!(side.metadata().side_chat);
            assert!(side.metadata().fork_context.is_some());
        });
        side.update(cx, |side, cx| side.send_message("scenario:happy", cx));
        run_until(cx, |cx| side.read_with(cx, |session, _| !session.is_working()));
        side.read_with(cx, |side, _| assert!(side.metadata().fork_context.is_none()));
    }

    async fn forked_from_finished_parent(
        cx: &mut TestAppContext,
    ) -> (tempfile::TempDir, Entity<AgentSession>) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let parent = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        parent.update(cx, |session, cx| session.send_message("scenario:happy", cx));
        run_until(cx, |cx| {
            parent.read_with(cx, |session, _| !session.is_working())
        });
        let parent_id = session_id(&parent, cx);
        let fork = store
            .update(cx, |store, cx| store.fork_session(&parent_id, false, cx))
            .await
            .expect("fork");
        fork.read_with(cx, |fork, _| {
            assert_eq!(fork.metadata().fork_source.as_deref(), Some("sess-1"));
            assert!(fork.metadata().fork_context.is_some());
        });
        (directory, fork)
    }

    #[gpui::test]
    async fn forks_continue_a_copy_of_the_agents_own_session(cx: &mut TestAppContext) {
        let (_directory, fork) = forked_from_finished_parent(cx).await;
        fork.update(cx, |fork, cx| fork.send_message("scenario:fork", cx));
        run_until(cx, |cx| {
            fork.read_with(cx, |session, _| !session.is_working())
        });
        fork.read_with(cx, |fork, cx| {
            assert!(!fork.run_failed(), "{:?}", summarize(fork.entries(), cx));
            assert_eq!(
                fork.metadata().native_session_id.as_deref(),
                Some("sess-forked")
            );
            assert!(fork.metadata().fork_source.is_none());
            assert!(fork.metadata().fork_context.is_none());
        });
    }

    #[gpui::test]
    async fn a_fork_drops_the_native_copy_once_its_parent_moves_on(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let parent = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        parent.update(cx, |session, cx| session.send_message("scenario:happy", cx));
        run_until(cx, |cx| {
            parent.read_with(cx, |session, _| !session.is_working())
        });
        let parent_id = session_id(&parent, cx);
        let fork = store
            .update(cx, |store, cx| store.fork_session(&parent_id, false, cx))
            .await
            .expect("fork");
        fork.read_with(cx, |fork, _| assert!(fork.metadata().fork_source.is_some()));

        parent.update(cx, |session, cx| session.send_message("scenario:happy", cx));
        run_until(cx, |cx| {
            parent.read_with(cx, |session, _| !session.is_working())
        });
        fork.read_with(cx, |fork, _| {
            assert!(fork.metadata().fork_source.is_none());
            assert!(
                fork.metadata().fork_context.is_some(),
                "the fork keeps its own history"
            );
        });
    }

    #[gpui::test]
    async fn a_failed_native_fork_falls_back_to_sending_the_history(cx: &mut TestAppContext) {
        let (_directory, fork) = forked_from_finished_parent(cx).await;
        fork.update(cx, |fork, cx| {
            fork.send_message("scenario:fork-fallback", cx)
        });
        run_until(cx, |cx| {
            fork.read_with(cx, |session, _| !session.is_working())
        });
        fork.read_with(cx, |fork, cx| {
            let summary = summarize(fork.entries(), cx);
            assert!(!fork.run_failed(), "{summary:?}");
            assert!(
                !fork
                    .entries()
                    .iter()
                    .any(|entry| matches!(entry, Entry::Notice { is_error: true, .. })),
                "the failed attempt stays out of the chat: {summary:?}"
            );
            assert_eq!(
                fork.metadata().native_session_id.as_deref(),
                Some("sess-fallback")
            );
            assert!(fork.metadata().fork_source.is_none());
            assert!(fork.metadata().fork_context.is_none());
        });
    }

    #[gpui::test]
    async fn finished_runs_stay_unseen_until_viewed(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| session.send_message("scenario:happy", cx));
        run_until(cx, |cx| session.read_with(cx, |session, _| !session.is_working()));
        session.read_with(cx, |session, _| {
            assert_eq!(session.metadata().outcome, Some(ChatOutcome::Completed));
            assert!(session.metadata().unseen);
        });
        session.update(cx, |session, cx| session.mark_seen(cx));
        session.read_with(cx, |session, _| assert!(!session.metadata().unseen));
    }

    fn session_id(session: &Entity<AgentSession>, cx: &mut TestAppContext) -> String {
        session.read_with(cx, |session, _| session.metadata().id.clone())
    }
    #[test]
    fn legacy_model_string_reads_as_native_selection() {
        let settings: RunSettings = serde_json::from_str(r#"{"model":"gpt-5.6-sol"}"#)
            .expect("legacy settings must decode");
        assert_eq!(
            settings.model,
            Some(ModelSelection::Native {
                id: "gpt-5.6-sol".into()
            })
        );
        assert_eq!(settings.config_source, ConfigSource::Native);

        let encoded = serde_json::to_value(&settings).expect("settings encode");
        assert_eq!(encoded["model"]["source"], "native");
        assert_eq!(encoded["model"]["id"], "gpt-5.6-sol");
    }

    #[test]
    fn managed_model_preserves_provider_identity() {
        let settings: RunSettings = serde_json::from_str(
            r#"{"model":{"source":"managed","provider_id":"p1","model_id":"m1"},"config_source":"managed"}"#,
        )
        .expect("managed settings must decode");
        assert_eq!(settings.config_source, ConfigSource::Managed);
        assert_eq!(
            settings.model,
            Some(ModelSelection::Managed {
                provider_id: "p1".into(),
                model_id: "m1".into()
            })
        );
        assert_eq!(settings.native_model(), None);
    }

    #[test]
    fn damaged_model_object_is_rejected_not_cleared() {
        let error = serde_json::from_str::<RunSettings>(
            r#"{"model":{"source":"managed","provider_id":"p1"}}"#,
        )
        .expect_err("missing model_id must be visible");
        assert!(error.to_string().contains("model_id"));
    }