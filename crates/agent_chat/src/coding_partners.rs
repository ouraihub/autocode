use std::{path::PathBuf, sync::Arc};

use editor::{Editor, EditorEvent};
use gpui::{App, AppContext as _, Context, Entity, FocusHandle, Focusable, Subscription, Window};
use i18n::t;
use model_provider::{AgentPreferences, BackendTarget, ConfigSource, EngineProviderChoice, ModelSelection, ModelType, ProviderSnapshot};
use settings::Settings as _;
use ui::prelude::*;

use crate::{AgentChatSettings, AgentKind, provider_services::ProviderServices};

mod view;
mod uninstall;
#[cfg(test)]
mod tests;

type Service = model_provider::ProviderService<crate::provider_store::ProviderDb, crate::provider_services::GpuiCredentialBroker>;

/// Settings for installed coding engines and their model-service source.
pub struct CodingPartnersView {
    service: Arc<Service>,
    selected: BackendTarget,
    providers: Vec<ProviderSnapshot>,
    preferences: Vec<AgentPreferences>,
    installs: Vec<(BackendTarget, Option<PathBuf>, bool)>,
    search: Entity<Editor>,
    loading: bool,
    saving: bool,
    installing: Option<BackendTarget>,
    removing: bool,
    status: Option<(String, bool)>,
    focus: FocusHandle,
    pub open_model_services: bool,
    _subscriptions: Vec<Subscription>,
}

pub(super) fn kind(target: BackendTarget) -> AgentKind {
    match target { BackendTarget::Pi => AgentKind::Pi, BackendTarget::Omp => AgentKind::Omp, BackendTarget::Dsh => AgentKind::Dsh }
}
fn name(target: BackendTarget) -> &'static str {
    match target { BackendTarget::Pi => "Pi", BackendTarget::Omp => "Oh My Pi", BackendTarget::Dsh => "DeepSeek Harness" }
}

impl CodingPartnersView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text(&t!("Search providers…"), window, cx);
            editor
        });
        let subscriptions = vec![cx.subscribe(&search, |_, _, event, cx| {
            if matches!(event, EditorEvent::BufferEdited) { cx.notify(); }
        })];
        let mut this = Self {
            service: ProviderServices::global(cx).service(), selected: BackendTarget::Pi,
            providers: Vec::new(), preferences: Vec::new(), installs: Vec::new(), search,
            loading: false, saving: false, installing: None, removing: false, status: None,
            focus: cx.focus_handle(), open_model_services: false, _subscriptions: subscriptions,
        };
        let mut changes = this.service.subscribe_changes();
        cx.spawn_in(window, async move |this, cx| {
            while changes.recv().await.is_ok() {
                this.update_in(cx, |this, window, cx| this.reload(window, cx))?;
            }
            anyhow::Ok(())
        }).detach_and_log_err(cx);
        this.reload(window, cx);
        this
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading { return; }
        self.loading = true;
        let service = self.service.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result: anyhow::Result<_> = async {
                let providers = service.list_providers().await?;
                let mut preferences = Vec::new();
                for target in BackendTarget::ALL { preferences.push(service.load_preferences(target).await?); }
                Ok((providers, preferences))
            }.await;
            let installs = cx.background_spawn(async {
                BackendTarget::ALL.into_iter().map(|target| {
                    let managed = crate::engine_launch::managed_entry(target);
                    let builtin = managed.is_some();
                    (target, managed.or_else(|| crate::engine_launch::discover(target)), builtin)
                }).collect::<Vec<_>>()
            }).await;
            this.update(cx, |this, cx| {
                this.loading = false;
                this.installs = installs;
                match result {
                    Ok((providers, preferences)) => { this.providers = providers; this.preferences = preferences; }
                    Err(_) => this.status = Some((t!("Could not load coding partners").to_string(), true)),
                }
                cx.notify();
            })
        }).detach_and_log_err(cx);
    }

    fn choose(&mut self, choice: Option<EngineProviderChoice>, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || self.loading { return; }
        let Some(mut preferences) = self.preferences.iter().find(|p| p.target == self.selected).cloned() else { return; };
        let default = match &choice {
            Some(EngineProviderChoice::Official) => None,
            _ => self.providers.iter().filter(|provider| provider.enabled && match &choice {
                Some(EngineProviderChoice::Provider(id)) => id == &provider.id,
                _ => true,
            }).find_map(|provider| {
                provider.models.iter().find(|model| model.enabled && model.model_type == ModelType::Chat)
                    .map(|model| ModelSelection::Managed { provider_id: provider.id.clone(), model_id: model.model_id.clone() })
            }),
        };
        preferences.source = if matches!(choice, Some(EngineProviderChoice::Official)) { ConfigSource::Native } else { ConfigSource::Managed };
        preferences.provider_choice = choice;
        preferences.default_model = default;
        self.saving = true;
        self.status = None;
        let service = self.service.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = service.save_preferences(preferences.target, preferences.revision, preferences).await;
            this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match result {
                    Ok(saved) => {
                        if let Some(slot) = this.preferences.iter_mut().find(|p| p.target == saved.target) { *slot = saved; }
                        this.status = Some((t!("Changes apply to new chats.").to_string(), false));
                    }
                    Err(_) => { this.status = Some((t!("Could not save coding partner. Reload and retry.").to_string(), true)); this.reload(window, cx); }
                }
                cx.notify();
            })
        }).detach_and_log_err(cx);
        cx.notify();
    }

    fn set_default(&mut self, cx: &mut Context<Self>) {
        let agent = kind(self.selected).to_setting();
        let completion = settings::update_settings_file_with_completion(<dyn fs::Fs>::global(cx), cx, move |settings, _| {
            settings.agent_chat.get_or_insert_default().default_agent = Some(agent);
        });
        cx.spawn(async move |this, cx| {
            let saved = matches!(completion.await, Ok(Ok(())));
            this.update(cx, |this, cx| {
                this.status = Some((if saved { t!("Changes apply to new chats.") } else { t!("Could not save coding partner. Reload and retry.") }.to_string(), !saved));
                cx.notify();
            })
        }).detach_and_log_err(cx);
    }

    fn install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.installing.is_some() { return; }
        let target = self.selected;
        let node = crate::dsh_node_runtime(cx);
        self.installing = Some(target);
        self.status = None;
        cx.spawn_in(window, async move |this, cx| {
            let result = crate::engine_launch::install(target, node).await;
            this.update_in(cx, |this, window, cx| {
                this.installing = None;
                this.status = Some(match result {
                    Ok(()) => (t!("Coding partner installed.").to_string(), false),
                    Err(error) => (format!("{}: {error}", t!("Coding partner operation failed")), true),
                });
                this.reload(window, cx);
                cx.notify();
            })
        }).detach_and_log_err(cx);
        cx.notify();
    }
}

impl Focusable for CodingPartnersView {
    fn focus_handle(&self, _: &App) -> FocusHandle { self.focus.clone() }
}
