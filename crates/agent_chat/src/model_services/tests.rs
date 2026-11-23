use super::*;

#[gpui::test]
async fn provider_creation_edit_and_reopen_preserve_saved_metadata(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| { cx.set_global(db::AppDatabase::test_new()); workspace::AppState::test(cx); editor::init(cx); });
    let directory = tempfile::tempdir().unwrap();
    crate::session::tests::new_store(directory.path(), cx);
    cx.update(ProviderServices::install);
    let project = project::Project::test(project::FakeFs::new(cx.executor()), [], cx).await;
    let (multi, cx) = cx.add_window_view(|window, cx| workspace::MultiWorkspace::test_new(project, window, cx));
    let workspace = multi.read_with(cx, |multi, _| multi.workspace().clone());
    workspace.update_in(cx, crate::provider_panel::open);
    let view = workspace.read_with(cx, |workspace, cx| workspace.active_modal::<crate::provider_panel::ProviderPanel>(cx).unwrap());
    let page = view.read_with(cx, |panel, _| panel.view.clone());
    cx.run_until_parked();
    page.update_in(cx, |page, window, cx| {
        page.open_provider(true, window, cx);
        ModelServicesView::set_text(&page.name, "Local service", window, cx);
        ModelServicesView::set_text(&page.endpoint, "http://127.0.0.1:12345/v1", window, cx);
        page.save_provider_dialog(window, cx);
    });
    for _ in 0..100 {
        cx.run_until_parked();
        if !page.read_with(cx, |page, _| page.busy || page.draft_saving) { break; }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    page.read_with(cx, |page, _| {
        let provider = page.provider().expect("saved provider is selected");
        assert_eq!(provider.name, "Local service");
        assert_eq!(provider.endpoints[0].base_url, "http://127.0.0.1:12345/v1");
    });
    page.update_in(cx, |page, window, cx| {
        ModelServicesView::set_text(&page.endpoint, "file:///invalid", window, cx);
        page.commit_endpoint(window, cx);
    });
    cx.run_until_parked();
    page.read_with(cx, |page, cx| {
        assert!(!page.busy);
        assert!(page.failed);
        assert_eq!(page.endpoint.read(cx).text(cx), "file:///invalid");
        assert_eq!(page.provider().unwrap().endpoints[0].base_url, "http://127.0.0.1:12345/v1");
    });
    page.update_in(cx, |page, window, cx| {
        page.close_dialog(window, cx);
        page.open_line(Some(Protocol::OpenaiChat), window, cx);
        ModelServicesView::set_text(&page.endpoint, "http://127.0.0.1:12346/v1", window, cx);
        page.save_line(window, cx);
    });
    for _ in 0..100 {
        cx.run_until_parked();
        if !page.read_with(cx, |page, _| page.busy || page.draft_saving) { break; }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    page.read_with(cx, |page, _| {
        assert!(page.dialog.is_none());
        assert_eq!(page.provider().unwrap().endpoints[0].base_url, "http://127.0.0.1:12346/v1");
    });
    page.update_in(cx, |page, window, cx| page.reload(window, cx));
    for _ in 0..100 {
        cx.run_until_parked();
        if !page.read_with(cx, |page, _| page.busy || page.draft_saving) { break; }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    page.read_with(cx, |page, _| assert_eq!(page.provider().unwrap().endpoints[0].base_url, "http://127.0.0.1:12346/v1"));
    page.update_in(cx, |page, window, cx| {
        page.open_keys(None, window, cx);
        ModelServicesView::set_text(&page.key_label, "local", window, cx);
        ModelServicesView::set_text(&page.key, "test-secret", window, cx);
        page.save_key(window, cx);
    });
    for _ in 0..100 {
        cx.run_until_parked();
        if !page.read_with(cx, |page, _| page.busy || page.draft_saving) { break; }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    page.read_with(cx, |page, cx| {
        // The GPUI test vault cannot read back writes: the editor must retain the secret on failure.
        assert!(page.failed);
        assert!(page.provider().unwrap().keys.is_empty());
        assert_eq!(page.dialog, Some(Dialog::Keys));
        assert_eq!(page.key.read(cx).text(cx), "test-secret");
    });
    page.update_in(cx, |page, window, cx| {
        page.close_dialog(window, cx);
        page.open_line(None, window, cx);
        ModelServicesView::set_text(&page.endpoint, "http://127.0.0.1:12346/v1", window, cx);
        page.save_line(window, cx);
    });
    for _ in 0..100 {
        cx.run_until_parked();
        if !page.read_with(cx, |page, _| page.busy || page.draft_saving) { break; }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    page.read_with(cx, |page, _| {
        let provider = page.provider().unwrap();
        assert_eq!(provider.endpoints.len(), 2);
        assert_eq!(provider.default_protocol, Protocol::OpenaiChat);
        assert!(provider.endpoints.iter().any(|endpoint| endpoint.protocol == Protocol::OpenaiResponses));
    });
    page.update_in(cx, |page, window, cx| page.close_dialog(window, cx));
}

#[gpui::test]
async fn pending_key_probe_waits_for_commit_and_preserves_input_when_vault_fails(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| { cx.set_global(db::AppDatabase::test_new()); workspace::AppState::test(cx); editor::init(cx); });
    let directory = tempfile::tempdir().unwrap();
    crate::session::tests::new_store(directory.path(), cx);
    cx.update(ProviderServices::install);
    let (page, cx) = cx.add_window_view(|window, cx| ModelServicesView::new(window, cx));
    cx.run_until_parked();
    page.update_in(cx, |page, window, cx| {
        page.open_provider(true, window, cx);
        ModelServicesView::set_text(&page.name, "Pending key", window, cx);
        ModelServicesView::set_text(&page.endpoint, "http://127.0.0.1:9/v1", window, cx);
        page.save_provider_dialog(window, cx);
    });
    for _ in 0..100 {
        cx.run_until_parked();
        if !page.read_with(cx, |page, _| page.busy || page.draft_saving) { break; }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // The test platform accepts writes but cannot read them back. Both click orders
    // must report the vault failure rather than probing an unauthenticated snapshot.
    for discover in [false, true] {
        for blur_first in [false, true] {
            page.update_in(cx, |page, window, cx| {
                ModelServicesView::set_text(&page.key, "test-only-key", window, cx);
                if blur_first { page.add_first_key(window, cx); }
                if discover { page.sync_models(window, cx); } else { page.check_connection(window, cx); }
                assert!(page.key_saving, "probe must join the credential commit before starting HTTP");
                assert_eq!(page.pending_probe, Some(discover));
            });
            for _ in 0..100 {
                cx.run_until_parked();
                if !page.read_with(cx, |page, _| page.busy || page.draft_saving) { break; }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            page.read_with(cx, |page, cx| {
                assert!(page.failed);
                assert_eq!(page.key.read(cx).text(cx), "test-only-key");
                assert!(page.provider().unwrap().keys.is_empty());
                assert!(!page.key_saving);
                assert!(page.pending_probe.is_none());
                assert!(page.dialog.is_none());
            });
        }
    }
}


#[gpui::test]
async fn empty_directory_opens_with_saved_disabled_presets_and_preserves_user_edits(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| { cx.set_global(db::AppDatabase::test_new()); workspace::AppState::test(cx); editor::init(cx); });
    let directory = tempfile::tempdir().unwrap();
    crate::session::tests::new_store(directory.path(), cx);
    cx.update(ProviderServices::install);
    let (page, cx) = cx.add_window_view(|window, cx| ModelServicesView::new(window, cx));
    for _ in 0..100 {
        cx.run_until_parked();
        if !page.read_with(cx, |page, _| page.busy || page.draft_saving) { break; }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let original = page.read_with(cx, |page, _| {
        assert_eq!(page.providers.iter().map(|provider| provider.preset_id.as_deref()).collect::<Vec<_>>(), vec![Some("qiniu"), Some("siliconflow"), Some("fenno")]);
        assert!(page.providers.iter().all(|provider| !provider.enabled && provider.keys.is_empty() && provider.models.is_empty()));
        assert_eq!(page.provider().unwrap().name, "七牛云");
        assert_eq!(model_provider::provider_preset(page.provider().unwrap().preset_id.as_deref().unwrap()).unwrap().base_url, page.provider().unwrap().endpoints[0].base_url);
        page.provider().unwrap().id.clone()
    });
    page.update_in(cx, |page, window, cx| {
        page.open_provider(false, window, cx);
        ModelServicesView::set_text(&page.name, "我的七牛云", window, cx);
        page.save_provider_dialog(window, cx);
    });
    for _ in 0..100 { cx.run_until_parked(); if !page.read_with(cx, |page, _| page.busy || page.draft_saving) { break; } std::thread::sleep(std::time::Duration::from_millis(5)); }
    page.update_in(cx, |page, window, cx| page.reload(window, cx));
    for _ in 0..100 { cx.run_until_parked(); if !page.read_with(cx, |page, _| page.busy || page.draft_saving) { break; } std::thread::sleep(std::time::Duration::from_millis(5)); }
    page.read_with(cx, |page, _| {
        assert_eq!(page.providers.len(), 3);
        let saved = page.providers.iter().find(|p| p.id == original).unwrap();
        assert_eq!(saved.name, "我的七牛云");
        assert_eq!(saved.preset_id.as_deref(), Some("qiniu"));
    });
}

#[gpui::test]
async fn deleting_another_provider_preserves_selection_on_cancel_and_confirmation(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| { cx.set_global(db::AppDatabase::test_new()); workspace::AppState::test(cx); editor::init(cx); });
    let directory = tempfile::tempdir().unwrap();
    crate::session::tests::new_store(directory.path(), cx);
    cx.update(ProviderServices::install);
    let (page, cx) = cx.add_window_view(|window, cx| ModelServicesView::new(window, cx));
    for _ in 0..100 { cx.run_until_parked(); if !page.read_with(cx, |p, _| p.busy || p.draft_saving) { break; } std::thread::sleep(std::time::Duration::from_millis(5)); }
    let preset = page.read_with(cx, |p, _| p.selected.clone().unwrap());
    page.update_in(cx, |p, window, cx| {
        p.open_provider(true, window, cx);
        ModelServicesView::set_text(&p.name, "Temporary relay", window, cx);
        ModelServicesView::set_text(&p.endpoint, "http://127.0.0.1:9/v1", window, cx);
        p.save_provider_dialog(window, cx);
    });
    for _ in 0..100 { cx.run_until_parked(); if !page.read_with(cx, |p, _| p.busy || p.draft_saving) { break; } std::thread::sleep(std::time::Duration::from_millis(5)); }
    let custom = page.read_with(cx, |p, _| p.selected.clone().unwrap());
    page.update_in(cx, |p, window, cx| {
        p.select(Some(preset.clone()), window, cx);
        p.open_provider_for(custom.clone(), window, cx);
        assert_eq!(p.selected.as_ref(), Some(&preset));
        assert_eq!(p.name.read(cx).text(cx), "Temporary relay");
        ModelServicesView::set_text(&p.name, "Cancelled edit", window, cx);
        p.close_dialog(window, cx);
        assert_eq!(p.providers.iter().find(|p| p.id == custom).unwrap().name, "Temporary relay");
        p.delete_provider_id(custom.clone(), window, cx);
    });
    assert!(cx.pending_prompt().unwrap().0.contains("Temporary relay"));
    cx.simulate_prompt_answer(&t!("Cancel"));
    cx.run_until_parked();
    page.read_with(cx, |p, _| { assert_eq!(p.selected.as_ref(), Some(&preset)); assert!(p.providers.iter().any(|p| p.id == custom)); });
    page.update_in(cx, |p, window, cx| p.delete_provider_id(custom.clone(), window, cx));
    cx.simulate_prompt_answer(&t!("Delete"));
    for _ in 0..100 { cx.run_until_parked(); if !page.read_with(cx, |p, _| p.busy || p.draft_saving) { break; } std::thread::sleep(std::time::Duration::from_millis(5)); }
    page.read_with(cx, |p, _| { assert_eq!(p.selected.as_ref(), Some(&preset)); assert!(!p.providers.iter().any(|p| p.id == custom)); });
    page.update_in(cx, |p, window, cx| p.delete_provider_id(preset.clone(), window, cx));
    assert!(!cx.has_pending_prompt());
}

#[gpui::test]
async fn endpoint_edits_during_save_are_drained_and_failed_draft_can_retry(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| { cx.set_global(db::AppDatabase::test_new()); workspace::AppState::test(cx); editor::init(cx); });
    let directory = tempfile::tempdir().unwrap();
    crate::session::tests::new_store(directory.path(), cx);
    cx.update(ProviderServices::install);
    let (page, cx) = cx.add_window_view(|window, cx| ModelServicesView::new(window, cx));
    for _ in 0..100 { cx.run_until_parked(); if !page.read_with(cx, |p, _| p.busy || p.draft_saving) { break; } std::thread::sleep(std::time::Duration::from_millis(5)); }
    page.update_in(cx, |p, window, cx| {
        ModelServicesView::set_text(&p.endpoint, "http://127.0.0.1:10001/v1", window, cx); p.commit_endpoint(window, cx);
        ModelServicesView::set_text(&p.endpoint, "http://127.0.0.1:10002/v1", window, cx); p.commit_endpoint(window, cx);
        assert!(p.draft_saving); assert!(p.draft_dirty);
    });
    for _ in 0..100 { cx.run_until_parked(); if !page.read_with(cx, |p, _| p.busy || p.draft_saving) { break; } std::thread::sleep(std::time::Duration::from_millis(5)); }
    page.read_with(cx, |p, _| assert_eq!(p.provider().unwrap().endpoints[0].base_url, "http://127.0.0.1:10002/v1"));
    page.update_in(cx, |p, window, cx| { ModelServicesView::set_text(&p.endpoint, "file:///invalid", window, cx); p.commit_endpoint(window, cx); assert!(p.save_failed); });
    page.update_in(cx, |p, window, cx| { ModelServicesView::set_text(&p.endpoint, "http://127.0.0.1:10003/v1", window, cx); p.commit_endpoint(window, cx); });
    for _ in 0..100 { cx.run_until_parked(); if !page.read_with(cx, |p, _| p.busy || p.draft_saving) { break; } std::thread::sleep(std::time::Duration::from_millis(5)); }
    page.read_with(cx, |p, _| { assert!(!p.save_failed); assert_eq!(p.provider().unwrap().endpoints[0].base_url, "http://127.0.0.1:10003/v1"); });
    let (service, snapshot) = page.read_with(cx, |p, _| (p.service.clone(), p.provider().unwrap().clone()));
    service.update_provider(&snapshot.id, snapshot.revision, draft_of(&snapshot)).await.unwrap();
    page.update_in(cx, |p, window, cx| { ModelServicesView::set_text(&p.endpoint, "http://127.0.0.1:10004/v1", window, cx); p.commit_endpoint(window, cx); });
    for _ in 0..100 { cx.run_until_parked(); if !page.read_with(cx, |p, _| p.busy || p.draft_saving) { break; } std::thread::sleep(std::time::Duration::from_millis(5)); }
    page.read_with(cx, |p, cx| { assert!(p.save_failed); assert_eq!(p.endpoint.read(cx).text(cx), "http://127.0.0.1:10004/v1"); });
    page.update_in(cx, |p, window, cx| p.save_draft(window, cx));
    for _ in 0..100 { cx.run_until_parked(); if !page.read_with(cx, |p, _| p.busy || p.draft_saving) { break; } std::thread::sleep(std::time::Duration::from_millis(5)); }
    page.read_with(cx, |p, _| { assert!(!p.save_failed); assert_eq!(p.provider().unwrap().endpoints[0].base_url, "http://127.0.0.1:10004/v1"); });
}
