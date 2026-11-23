use std::{future::Future, sync::Arc};
use editor::{Editor, EditorEvent};
use gpui::{App, AppContext as _, Context, Entity, FocusHandle, Focusable, Subscription, Window};
use i18n::t;
use model_provider::{AuthMode, BackendTarget, ConfigSource, EndpointSnapshot, KeyRecord, ModelSelection, Protocol, ProviderDraft, ProviderSnapshot, RoutingMode, Secret};
use ui::prelude::*;
use ui::{Switch, ToggleState};
use crate::provider_services::ProviderServices;

type Service = model_provider::ProviderService<crate::provider_store::ProviderDb, crate::provider_services::GpuiCredentialBroker>;
mod view;
mod operations;
mod keys;
mod models;
#[cfg(test)] mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialog { Provider(bool), Line(Option<Protocol>), Keys, AddModel, Model, Discovery }

/// Shared settings and chat management surface. Secrets never enter settings.json.
pub struct ModelServicesView {
    service: Arc<Service>,
    providers: Vec<ProviderSnapshot>,
    selected: Option<String>,
    draft: ProviderDraft,
    protocol: Protocol,
    search: Entity<Editor>,
    name: Entity<Editor>,
    endpoint: Entity<Editor>,
    key: Entity<Editor>,
    key_label: Entity<Editor>,
    editing_key: Option<String>,
    revealed: Option<(String, Secret)>,
    headers: Vec<(Option<String>, Entity<Editor>, Entity<Editor>)>,
    models: models::ModelUi,
    filter: Option<bool>,
    loaded: bool,
    busy: bool,
    draft_saving: bool,
    draft_dirty: bool,
    save_failed: bool,
    edit_target: Option<String>,
    key_checks: Vec<crate::provider_services::KeyCheck>,
    key_saving: bool,
    pending_probe: Option<bool>,
    status: Option<String>,
    failed: bool,
    generation: u64,
    dialog: Option<Dialog>,
    focus: FocusHandle,
    dialog_focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

pub(super) fn protocol_label(protocol: Protocol) -> &'static str {
    match protocol { Protocol::OpenaiChat => "OpenAI Chat", Protocol::OpenaiResponses => "OpenAI Responses", Protocol::AnthropicMessages => "Anthropic Messages" }
}
fn empty_draft() -> ProviderDraft {
    ProviderDraft { name: String::new(), enabled: true, routing: RoutingMode::Direct, auth: AuthMode::ApiKey,
        default_protocol: Protocol::OpenaiChat, endpoints: vec![EndpointSnapshot { protocol: Protocol::OpenaiChat, base_url: String::new() }], models: Vec::new(), sort: 0 }
}
fn input(placeholder: &str, masked: bool, window: &mut Window, cx: &mut App) -> Entity<Editor> {
    cx.new(|cx| { let mut editor = Editor::single_line(window, cx); editor.set_placeholder_text(placeholder, window, cx); editor.set_masked(masked, cx); editor })
}
fn draft_of(provider: &ProviderSnapshot) -> ProviderDraft {
    ProviderDraft { name: provider.name.clone(), enabled: provider.enabled, routing: provider.routing, auth: provider.auth,
        default_protocol: provider.default_protocol, endpoints: provider.endpoints.clone(), models: provider.models.clone(), sort: provider.sort }
}
impl ModelServicesView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = input(&t!("Search providers…"), false, window, cx);
        let endpoint = input("https://api.example.com/v1", false, window, cx);
        let key = input(&t!("API key"), true, window, cx);
        let mut subscriptions = vec![cx.subscribe(&search, |_, _, event, cx| { if matches!(event, EditorEvent::BufferEdited) { cx.notify(); } })];
        subscriptions.push(cx.subscribe_in(&endpoint, window, |this, _, event, window, cx| {
            if matches!(event, EditorEvent::Blurred) && this.dialog.is_none() { this.commit_endpoint(window, cx); }
            if matches!(event, EditorEvent::BufferEdited) && this.dialog.is_none() && this.draft_saving { this.commit_endpoint(window, cx); }
        }));
        subscriptions.push(cx.subscribe_in(&key, window, |this, _, event, window, cx| {
            if matches!(event, EditorEvent::Blurred) && this.dialog.is_none() { this.add_first_key(window, cx); }
        }));
        let mut this = Self {
            service: ProviderServices::global(cx).service(), providers: Vec::new(), selected: None,
            draft: empty_draft(), protocol: Protocol::OpenaiChat, search, endpoint, key,
            name: input(&t!("Provider name"), false, window, cx), key_label: input(&t!("Key label"), false, window, cx),
            editing_key: None, revealed: None, headers: Vec::new(), models: models::ModelUi::new(window, cx),
            filter: None, loaded: false, busy: false, draft_saving: false, draft_dirty: false, save_failed: false, edit_target: None, key_checks: Vec::new(), key_saving: false, pending_probe: None, status: None, failed: false, generation: 0, dialog: None,
            focus: cx.focus_handle(), dialog_focus: cx.focus_handle(), _subscriptions: subscriptions,
        };
        this.reload(window, cx);
        this
    }
    fn provider(&self) -> Option<&ProviderSnapshot> {
        self.selected.as_ref().and_then(|id| self.providers.iter().find(|provider| &provider.id == id))
    }
    fn set_text(editor: &Entity<Editor>, text: &str, window: &mut Window, cx: &mut App) {
        editor.update(cx, |editor, cx| editor.set_text(text, window, cx));
    }
    fn select(&mut self, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.draft_saving { return; }
        self.generation = self.generation.wrapping_add(1);
        self.selected = id;
        self.draft = self.provider().map(draft_of).unwrap_or_else(empty_draft);
        self.protocol = self.draft.default_protocol;
        self.status = None; self.failed = false; self.revealed = None; self.dialog = None;
        self.save_failed = false; self.draft_dirty = false; self.key_checks.clear(); self.edit_target = None;
        self.models.reset(window, cx);
        Self::set_text(&self.name, &self.draft.name, window, cx);
        Self::set_text(&self.key, "", window, cx);
        self.sync_endpoint(window, cx);
        cx.notify();
    }
    fn sync_endpoint(&self, window: &mut Window, cx: &mut App) {
        let value = self.draft.endpoints.iter().find(|endpoint| endpoint.protocol == self.protocol).map(|endpoint| endpoint.base_url.as_str()).unwrap_or("");
        Self::set_text(&self.endpoint, value, window, cx);
    }
    fn report(&mut self, message: String, failed: bool, cx: &mut Context<Self>) { self.status = Some(message); self.failed = failed; cx.notify(); }
    fn stored(&mut self, snapshot: ProviderSnapshot, cx: &mut Context<Self>) {
        if self.selected.as_ref() == Some(&snapshot.id) {
            if !self.draft_dirty && self.edit_target.as_ref().is_none_or(|id| self.selected.as_ref() == Some(id)) { self.draft = draft_of(&snapshot); }
            self.key_checks.clear();
        }
        if let Some(existing) = self.providers.iter_mut().find(|provider| provider.id == snapshot.id) { *existing = snapshot; }
        else { self.providers.push(snapshot); }
        cx.notify();
    }
    fn write(&mut self, job: impl Future<Output = anyhow::Result<ProviderSnapshot>> + 'static, close: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.draft_saving { return; }
        self.busy = true;
        let generation = self.generation;
        let (service, selected) = (self.service.clone(), self.selected.clone());
        cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            let latest = if result.is_err() { if let Some(id) = selected { service.get_provider(&id).await.ok().flatten() } else { None } } else { None };
            this.update_in(cx, |this, window, cx| {
                this.busy = false;
                if generation != this.generation { return; }
                match result {
                    Ok(snapshot) => { this.stored(snapshot, cx); this.status = None; this.failed = false; if close { this.close_dialog(window, cx); } }
                    Err(_) => {
                        if let Some(snapshot) = latest { if let Some(slot) = this.providers.iter_mut().find(|provider| provider.id == snapshot.id) { *slot = snapshot; } }
                        this.report(t!("Could not save changes. Your draft is retained; reload if the provider changed elsewhere.").to_string(), true, cx);
                    }
                }
            })
        }).detach_and_log_err(cx);
        cx.notify();
    }
    fn close_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.draft_saving { return; }
        self.dialog = None; self.revealed = None; self.headers.clear(); self.editing_key = None;
        self.edit_target = None;
        self.save_failed = false; self.draft_dirty = false;
        Self::set_text(&self.key, "", window, cx);
        Self::set_text(&self.key_label, "", window, cx);
        self.draft = self.provider().map(draft_of).unwrap_or_else(empty_draft);
        if !self.draft.endpoints.iter().any(|endpoint| endpoint.protocol == self.protocol) { self.protocol = self.draft.default_protocol; }
        self.sync_endpoint(window, cx);
        self.focus.focus(window, cx); cx.notify();
    }
    fn show_dialog(&mut self, dialog: Dialog, window: &mut Window, cx: &mut Context<Self>) {
        self.dialog = Some(dialog); self.status = None; self.failed = false;
        self.dialog_focus.focus(window, cx); cx.notify();
    }
    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.draft_saving { return; }
        self.busy = true;
        let service = self.service.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = service.seed_presets().await;
            this.update_in(cx, |this, window, cx| {
                this.busy = false; this.loaded = true;
                match result {
                    Ok(providers) => { this.providers = providers; let id = this.selected.clone().filter(|id| this.providers.iter().any(|p| &p.id == id)).or_else(|| this.providers.first().map(|p| p.id.clone())); this.select(id, window, cx); }
                    Err(_) => this.report(t!("Could not load model services").to_string(), true, cx),
                }
            })
        }).detach_and_log_err(cx);
    }
}
impl Focusable for ModelServicesView { fn focus_handle(&self, _: &App) -> FocusHandle { self.focus.clone() } }
