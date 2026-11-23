use super::*;
use gpui::{ClipboardItem, IntoElement, px};
use super::view::{field, caption, icon_button};

#[derive(IntoElement)]
struct HoverKeyAction(ui::IconButton);

impl RenderOnce for HoverKeyAction {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus = window.use_keyed_state((self.0.id().clone(), "hover-focus"), cx, |_, cx| cx.focus_handle());
        let focus = focus.read(cx).clone();
        let keyboard_focus = window.last_input_was_keyboard() && focus.is_focused(window);
        div().opacity(if keyboard_focus { 1. } else { 0. })
            .group_hover("saved-key", |style| style.opacity(1.))
            .child(self.0.track_focus(&focus).tab_index(0_isize))
    }
}

impl ModelServicesView {
    // A click may follow Blur while the credential vault write is still running.
    pub(super) fn request_probe(&mut self, discover: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.draft_saving { return; }
        if self.key_saving { self.pending_probe = Some(discover); return; }
        if self.busy { return; }
        let Some(provider) = self.provider() else { return; };
        if provider.auth == AuthMode::ApiKey && provider.keys.is_empty() && !self.key.read(cx).text(cx).trim().is_empty() {
            self.pending_probe = Some(discover);
            self.save_key(window, cx);
            return;
        }
        if provider.auth == AuthMode::ApiKey && !provider.keys.iter().any(|key| key.enabled) {
            self.report(t!("No enabled API key. Save a key or enable one in Manage keys.").to_string(), true, cx);
            return;
        }
        if discover { self.sync_models_saved(window, cx); } else { self.check_connection_saved(window, cx); }
    }
    pub(super) fn open_keys(&mut self, edit: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.draft_saving { return; }
        self.editing_key = edit;
        let label = self.provider().and_then(|p| p.keys.iter().find(|k| Some(&k.id) == self.editing_key.as_ref())).and_then(|k| k.label.as_deref()).unwrap_or("");
        Self::set_text(&self.key_label, label, window, cx);
        Self::set_text(&self.key, "", window, cx);
        self.show_dialog(Dialog::Keys, window, cx);
    }
    pub(super) fn add_first_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.provider().is_none_or(|p| !p.keys.is_empty()) { return; }
        if self.key.read(cx).text(cx).trim().is_empty() { return; }
        self.save_key(window, cx);
    }
    pub(super) fn save_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.draft_saving { return; }
        let Some(provider) = self.provider().cloned() else { return; };
        let value = self.key.read(cx).text(cx).trim().to_owned();
        let label = self.key_label.read(cx).text(cx).trim().to_owned();
        let editing = self.editing_key.clone();
        if value.trim().is_empty() && editing.is_none() { self.report(t!("API key is required").to_string(), true, cx); return; }
        self.key_checks.clear();
        let service = self.service.clone(); self.busy = true; self.key_saving = true;
        self.status = None; self.failed = false;
        cx.spawn_in(window, async move |this, cx| {
            let result: anyhow::Result<ProviderSnapshot> = async {
                let old = provider.keys.iter().find(|k| Some(&k.id) == editing.as_ref());
                let secret = if value.is_empty() { service.reveal_key(&provider.id, editing.as_deref().ok_or_else(|| anyhow::anyhow!("Key unavailable"))?).await?.ok_or_else(|| anyhow::anyhow!("Key unavailable"))? } else { Secret::new(value) };
                Ok(service.commit_key_secret(&provider.id, provider.revision, KeyRecord { id: editing, label: (!label.is_empty()).then_some(label), masked: "••••••••".into(), credential_ref: String::new(), enabled: old.is_none_or(|k| k.enabled), sort: old.map_or(provider.keys.len() as i64, |k| k.sort) }, secret).await?)
            }.await;
            this.update_in(cx, |this, window, cx| {
                this.busy = false; this.key_saving = false;
                let pending = this.pending_probe.take();
                match result {
                    Ok(provider) => {
                        this.stored(provider, cx); this.editing_key = None;
                        Self::set_text(&this.key, "", window, cx); Self::set_text(&this.key_label, "", window, cx);
                        this.status = None; this.failed = false;
                        if let Some(discover) = pending { this.request_probe(discover, window, cx); }
                    }
                    Err(_) => this.report(t!("Could not save the API key to the system credential vault. Your input was retained; no connection request was sent.").to_string(), true, cx),
                }
                cx.notify();
            })
        }).detach_and_log_err(cx); cx.notify();
    }
    pub(super) fn key_enabled(&mut self, id: String, enabled: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.draft_saving { return; }
        self.key_checks.clear();
        let Some(provider) = self.provider().cloned() else { return; }; let service = self.service.clone();
        self.write(async move { Ok(service.set_key_enabled(&provider.id, provider.revision, &id, enabled).await?) }, false, window, cx);
    }
    pub(super) fn remove_key(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.draft_saving { return; }
        self.key_checks.clear();
        let Some(provider) = self.provider().cloned() else { return; }; let service = self.service.clone(); self.revealed = None;
        self.write(async move { Ok(service.remove_key(&provider.id, provider.revision, &id).await?) }, false, window, cx);
    }
    pub(super) fn access_key(&mut self, id: String, copy: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.draft_saving { return; }
        if !copy && self.revealed.as_ref().is_some_and(|(shown, _)| shown == &id) { self.revealed = None; cx.notify(); return; }
        let Some(provider) = self.provider() else { return; };
        let (service, provider_id, generation) = (self.service.clone(), provider.id.clone(), self.generation);
        self.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = service.reveal_key(&provider_id, &id).await;
            this.update_in(cx, |this, _, cx| { this.busy = false; if this.generation != generation { return; } match result {
                Ok(Some(secret)) => { if copy { cx.write_to_clipboard(ClipboardItem::new_string(secret.expose().to_owned())); } else { this.revealed = Some((id, secret)); } cx.notify(); }
                _ => this.report(t!("Credential is unavailable").to_string(), true, cx),
            } })
        }).detach_and_log_err(cx);
    }
    pub(super) fn render_key_section(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let keys = self.provider().map(|p| p.keys.as_slice()).unwrap_or_default();
        let enabled_count = keys.iter().filter(|key| key.enabled).count();
        let key = keys.iter().find(|k| k.enabled).or(keys.first());
        let key_url = self.provider().and_then(|provider| provider.preset_id.as_deref()).and_then(model_provider::provider_preset).map(|preset| preset.api_key_url);
        let value = if let Some(key) = key {
            let (copy, edit, reveal) = (key.id.clone(), key.id.clone(), key.id.clone());
            let shown = self.revealed.as_ref().filter(|(id, _)| id == &key.id);
            let text = shown.map(|(_, s)| s.expose()).unwrap_or(&key.masked).to_owned();
            h_flex().group("saved-key").flex_1().min_w_0().min_h(px(32.)).px(px(8.)).gap(px(8.)).rounded(px(6.)).border_1().border_color(cx.theme().colors().border)
                .when_some(key.label.clone(), |row, label| row.child(caption(label)))
                .child(div().flex_1().min_w_0().truncate().child(Label::new(text)))
                .when(enabled_count > 1, |row| row.child(caption(format!("+{}", enabled_count - 1))))
                .child(HoverKeyAction(icon_button("copy-key", IconName::Copy, t!("Copy")).disabled(self.busy || self.draft_saving).on_click(cx.listener(move |this, _, window, cx| this.access_key(copy.clone(), true, window, cx)))))
                .child(HoverKeyAction(icon_button("edit-key", IconName::Pencil, t!("Edit")).disabled(self.busy || self.draft_saving).on_click(cx.listener(move |this, _, window, cx| this.open_keys(Some(edit.clone()), window, cx)))))
                .child(icon_button("reveal-key", if shown.is_some() { IconName::EyeOff } else { IconName::Eye }, t!("Reveal")).disabled(self.busy || self.draft_saving).on_click(cx.listener(move |this, _, window, cx| this.access_key(reveal.clone(), false, window, cx))))
                .into_any_element()
        } else { div().flex_1().min_w_0().child(field(self.key.clone())).into_any_element() };
        v_flex().gap(px(6.)).min_w_0().child(h_flex().gap(px(8.)).child(caption(t!("API key"))).when_some(key_url, |row, url| row.child(Button::new("get-provider-key", t!("Get API key")).size(ButtonSize::Compact).on_click(move |_, _, cx| cx.open_url(url)))))
            .child(h_flex().gap(px(8.)).flex_wrap().child(value)
                .child(icon_button("manage-keys", IconName::Lock, t!("Manage keys")).disabled(self.busy || self.draft_saving).on_click(cx.listener(|this, _, window, cx| this.open_keys(None, window, cx))))
                .child(Button::new("check-provider", t!("Check connection")).style(ButtonStyle::Outlined).disabled(self.draft_saving || (self.busy && !self.key_saving)).on_click(cx.listener(|this, _, window, cx| this.check_connection(window, cx)))))
            .children(self.key_checks.iter().map(|check| {
                let message = match (check.count, check.error.as_ref()) {
                    (Some(count), _) => t!("Connection and authentication succeeded: {count} models", count = count).to_string(),
                    (None, Some(error)) => error.clone(),
                    (None, None) => t!("Connection failed").to_string(),
                };
                let text = if check.label.is_empty() { message } else { format!("{} — {message}", check.label) };
                h_flex().gap(px(8.)).child(div().flex_1().min_w_0().child(Label::new(text).size(LabelSize::Small).color(if check.count.is_some() { Color::Success } else { Color::Error })))
                    .when_some(check.key_id.as_ref().and_then(|id| keys.iter().find(|key| &key.id == id)), |row, key| {
                        let id = key.id.clone();
                        row.child(Switch::new(SharedString::from(format!("checked-key-{id}")), if key.enabled { ToggleState::Selected } else { ToggleState::Unselected })
                            .aria_label(format!("{}: {}", t!("API key"), check.label))
                            .disabled(self.busy || self.draft_saving)
                            .on_click(cx.listener(move |this, state, window, cx| this.key_enabled(id.clone(), *state == ToggleState::Selected, window, cx))))
                    })
            }))
            .into_any_element()
    }
    pub(super) fn render_keys_dialog(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let keys = self.provider().map(|p| p.keys.clone()).unwrap_or_default();
        v_flex().gap(px(8.)).child(caption(t!("Keys are stored in the system credential vault.")))
            .children(keys.iter().map(|key| {
                let (copy, edit, del, toggle) = (key.id.clone(), key.id.clone(), key.id.clone(), key.id.clone());
                h_flex().min_h(px(32.)).gap(px(6.)).px(px(8.)).border_1().border_color(cx.theme().colors().border).rounded(px(6.))
                    .child(div().w(px(120.)).truncate().child(caption(key.label.clone().unwrap_or_else(|| t!("API key").to_string()))))
                    .child(div().flex_1().child(Label::new(key.masked.clone())))
                    .child(icon_button(SharedString::from(format!("copy-{copy}")), IconName::Copy, t!("Copy")).disabled(self.busy || self.draft_saving).on_click(cx.listener(move |this, _, window, cx| this.access_key(copy.clone(), true, window, cx))))
                    .child(icon_button(SharedString::from(format!("edit-{edit}")), IconName::Pencil, t!("Edit")).disabled(self.busy || self.draft_saving).on_click(cx.listener(move |this, _, window, cx| this.open_keys(Some(edit.clone()), window, cx))))
                    .child(icon_button(SharedString::from(format!("delete-{del}")), IconName::Trash, t!("Delete")).disabled(self.busy || self.draft_saving).on_click(cx.listener(move |this, _, window, cx| this.remove_key(del.clone(), window, cx))))
                    .child(Switch::new(SharedString::from(format!("enabled-{toggle}")), if key.enabled { ToggleState::Selected } else { ToggleState::Unselected }).aria_label(format!("{}: {}", t!("API key"), key.label.as_deref().unwrap_or(&key.masked))).disabled(self.busy || self.draft_saving).on_click(cx.listener(move |this, state, window, cx| this.key_enabled(toggle.clone(), *state == ToggleState::Selected, window, cx))))
            }))
            .child(h_flex().gap(px(6.)).child(div().w(px(120.)).child(field(self.key_label.clone()))).child(div().flex_1().min_w_0().child(field(self.key.clone())))
                .child(Button::new("submit-key", if self.editing_key.is_some() { t!("Save") } else { t!("Add") }).style(ButtonStyle::Filled).disabled(self.busy || self.draft_saving).on_click(cx.listener(|this, _, window, cx| this.save_key(window, cx)))))
            .when(self.editing_key.is_some(), |d| d.child(caption(t!("Leave empty to keep the saved value"))))
            .into_any_element()
    }
}
