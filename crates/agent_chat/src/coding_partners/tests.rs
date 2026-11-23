use super::*;

#[gpui::test]
async fn cancelling_uninstall_keeps_the_installation_and_releases_controls(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        cx.set_global(db::AppDatabase::test_new());
        workspace::AppState::test(cx);
        editor::init(cx);
    });
    let directory = tempfile::tempdir().unwrap();
    crate::session::tests::new_store(directory.path(), cx);
    cx.update(ProviderServices::install);
    let (page, cx) = cx.add_window_view(|window, cx| CodingPartnersView::new(window, cx));
    for _ in 0..100 {
        cx.run_until_parked();
        if !page.read_with(cx, |page, _| page.loading) { break; }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let entry = directory.path().join("engine.js");
    std::fs::write(&entry, "retained engine").unwrap();
    page.update_in(cx, |page, window, cx| {
        page.loading = false;
        page.selected = BackendTarget::Dsh;
        page.installs = vec![(BackendTarget::Dsh, Some(entry.clone()), true)];
        page.confirm_uninstall(window, cx);
    });
    cx.run_until_parked();
    assert!(cx.pending_prompt().unwrap().0.contains("DeepSeek Harness"));
    cx.simulate_prompt_answer(&t!("Cancel"));
    cx.run_until_parked();
    assert_eq!(std::fs::read_to_string(&entry).unwrap(), "retained engine");
    page.read_with(cx, |page, _| {
        assert_eq!(page.installing, None);
        assert!(!page.removing);
        assert!(page.status.is_none());
    });
}
