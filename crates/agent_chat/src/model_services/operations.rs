use super::*;
use gpui::{PromptButton, PromptLevel};
use model_provider::CredentialStore;

impl ModelServicesView {
    pub(super) fn open_provider_for(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.draft_saving { return; }
        let Some(provider) = self.providers.iter().find(|provider| provider.id == id) else { return; };
        self.draft = draft_of(provider); self.protocol = self.draft.default_protocol;
        self.edit_target = Some(id);
        Self::set_text(&self.name, &self.draft.name, window, cx); self.sync_endpoint(window, cx);
        self.show_dialog(Dialog::Provider(false), window, cx);
        self.name.focus_handle(cx).focus(window, cx);
    }
    pub(super) fn open_provider(&mut self, creating: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.draft_saving { return; }
        if !creating { if let Some(id) = self.selected.clone() { self.open_provider_for(id, window, cx); } return; }
        self.edit_target = None;
        self.draft = if creating { empty_draft() } else { self.provider().map(draft_of).unwrap_or_else(empty_draft) };
        self.protocol = self.draft.default_protocol;
        Self::set_text(&self.name, &self.draft.name, window, cx);
        self.sync_endpoint(window, cx);
        self.show_dialog(Dialog::Provider(creating), window, cx);
        self.name.focus_handle(cx).focus(window, cx);
    }
    pub(super) fn save_provider_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.draft_saving { return; }
        let Some(Dialog::Provider(creating)) = self.dialog else { return; };
        self.draft.name = self.name.read(cx).text(cx).trim().to_owned();
        if creating {
            self.draft.default_protocol = self.protocol;
            self.draft.endpoints = vec![EndpointSnapshot { protocol: self.protocol, base_url: self.endpoint.read(cx).text(cx).trim().to_owned() }];
        }
        let draft = self.draft.clone();
        if let Err(error) = model_provider::validate_draft(&draft) { self.report(error.to_string(), true, cx); return; }
        let service = self.service.clone();
        let existing = if creating { None } else { self.edit_target.as_ref().and_then(|id| self.providers.iter().find(|provider| &provider.id == id)).map(|p| (p.id.clone(), p.revision)) };
        self.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = match existing { Some((id, revision)) => service.update_provider(&id, revision, draft).await, None => service.create_provider(draft).await };
            this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(provider) => { let id = provider.id.clone(); this.stored(provider, cx); this.close_dialog(window, cx); if creating { this.select(Some(id), window, cx); } }
                    Err(_) => this.report(t!("Could not save changes. Your draft is retained; reload if the provider changed elsewhere.").to_string(), true, cx),
                }
            })
        }).detach_and_log_err(cx);
        cx.notify();
    }
    pub(super) fn save_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy { return; }
        self.draft_dirty = true; self.save_failed = false;
        if !self.draft_saving { self.drain_draft(window, cx); }
    }
    fn drain_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(provider) = self.provider() else { return; };
        let (id, revision, draft, service) = (provider.id.clone(), provider.revision, self.draft.clone(), self.service.clone());
        if let Err(error) = model_provider::validate_draft(&draft) { self.save_failed = true; self.report(error.to_string(), true, cx); return; }
        self.draft_dirty = false; self.draft_saving = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = service.update_provider(&id, revision, draft).await;
            let latest = if result.is_err() { service.get_provider(&id).await.ok().flatten() } else { None };
            this.update_in(cx, |this, window, cx| {
                this.draft_saving = false;
                match result {
                    Ok(snapshot) => {
                        this.stored(snapshot, cx); this.save_failed = false; this.status = None; this.failed = false;
                        if this.draft_dirty { this.drain_draft(window, cx); }
                        else if this.dialog.is_some() { this.close_dialog(window, cx); }
                    }
                    Err(_) => {
                        if let Some(snapshot) = latest { if let Some(slot) = this.providers.iter_mut().find(|p| p.id == snapshot.id) { *slot = snapshot; } }
                        this.save_failed = true; this.report(t!("Could not save changes. Your draft is retained; reload if the provider changed elsewhere.").to_string(), true, cx);
                    }
                }
                cx.notify();
            })
        }).detach_and_log_err(cx); cx.notify();
    }
    pub(super) fn commit_endpoint(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.provider().is_none() { return; }
        let value = self.endpoint.read(cx).text(cx).trim().to_owned();
        if !self.save_failed && self.draft.endpoints.iter().find(|e| e.protocol == self.protocol).is_some_and(|e| e.base_url == value) { return; }
        if let Some(endpoint) = self.draft.endpoints.iter_mut().find(|e| e.protocol == self.protocol) { endpoint.base_url = value; }
        self.save_draft(window, cx);
    }
    pub(super) fn change_protocol(&mut self, protocol: Protocol, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.draft_saving { return; }
        self.generation = self.generation.wrapping_add(1);
        self.protocol = protocol; self.revealed = None; self.status = None; self.key_checks.clear();
        self.models.reset(window, cx); self.sync_endpoint(window, cx); cx.notify();
    }
    pub(super) fn open_line(&mut self, editing: Option<Protocol>, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.draft_saving { return; }
        self.draft = self.provider().map(draft_of).unwrap_or_else(empty_draft);
        let current_protocol = self.protocol;
        self.protocol = editing.unwrap_or_else(|| [Protocol::OpenaiChat, Protocol::OpenaiResponses, Protocol::AnthropicMessages].into_iter().find(|p| !self.draft.endpoints.iter().any(|e| e.protocol == *p)).unwrap_or(current_protocol));
        self.sync_endpoint(window, cx);
        self.headers = self.provider().map(|p| p.headers.clone()).unwrap_or_default().into_iter().map(|h| {
            let name = input("X-Custom-Header", false, window, cx);
            Self::set_text(&name, &h.name, window, cx);
            (Some(h.id), name, input(&t!("Leave empty to keep the saved value"), true, window, cx))
        }).collect();
        self.show_dialog(Dialog::Line(editing), window, cx);
    }
    pub(super) fn save_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.draft_saving { return; }
        let Some(Dialog::Line(editing)) = self.dialog else { return; };
        let Some(provider) = self.provider().cloned() else { return; };
        let url = self.endpoint.read(cx).text(cx).trim().to_owned();
        let protocol = self.protocol;
        let mut draft = draft_of(&provider);
        draft.default_protocol = self.draft.default_protocol;
        if let Some(old) = editing {
            for model in &mut draft.models { for p in &mut model.protocols { if *p == old { *p = protocol; } } }
            draft.endpoints.retain(|e| e.protocol != old);
            if draft.default_protocol == old { draft.default_protocol = protocol; }
        }
        draft.endpoints.retain(|e| e.protocol != protocol);
        draft.endpoints.push(EndpointSnapshot { protocol, base_url: url });
        if let Err(error) = model_provider::validate_draft(&draft) { self.report(error.to_string(), true, cx); return; }
        let mut headers = Vec::new();
        let mut header_names = std::collections::HashSet::new();
        for (id, name, value) in &self.headers {
            let name = name.read(cx).text(cx).trim().to_owned();
            let value = value.read(cx).text(cx);
            if id.is_none() && name.is_empty() && value.is_empty() { continue; }
            if model_provider::validate_header_name(&name).is_err() || (id.is_none() && value.is_empty()) {
                self.report(t!("Header name and value are required").to_string(), true, cx); return;
            }
            if !header_names.insert(name.to_ascii_lowercase()) { self.report(t!("Header names must be unique").to_string(), true, cx); return; }
            if !value.is_empty() && model_provider::validate_header_value(&value).is_err() { self.report(t!("Invalid header value").to_string(), true, cx); return; }
            headers.push((id.clone(), name, value));
        }
        self.draft = draft.clone();
        let service = self.service.clone();
        self.write(async move {
            let mut saved = service.update_provider(&provider.id, provider.revision, draft).await?;
            for old in &provider.headers { if !headers.iter().any(|(id, name, _)| id.as_ref() == Some(&old.id) || (id.is_none() && name.eq_ignore_ascii_case(&old.name))) { saved = service.remove_header(&saved.id, saved.revision, &old.id).await?; } }
            for (id, name, value) in headers {
                let old = provider.headers.iter().find(|h| Some(&h.id) == id.as_ref() || (id.is_none() && h.name.eq_ignore_ascii_case(&name)));
                if value.is_empty() && old.is_some_and(|h| h.name == name) { continue; }
                let secret = if value.is_empty() { service.credentials().get(&old.ok_or_else(|| anyhow::anyhow!("Header unavailable"))?.credential_ref).await?.ok_or_else(|| anyhow::anyhow!("Header unavailable"))? } else { Secret::new(value) };
                saved = service.commit_header_secret(&saved.id, saved.revision, model_provider::HeaderRecord { id: old.map(|h| h.id.clone()).or(id), name, masked: "••••••••".into(), credential_ref: String::new(), enabled: true }, secret).await?;
            }
            Ok(saved)
        }, true, window, cx);
    }
    pub(super) fn remove_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Dialog::Line(Some(protocol))) = self.dialog else { return; };
        if self.draft.endpoints.len() <= 1 || self.busy { return; }
        let answer = window.prompt(PromptLevel::Warning, &t!("Remove endpoint"), Some(&t!("Models used only by this endpoint will also be removed.")), &[PromptButton::new(t!("Delete")), PromptButton::cancel(t!("Cancel"))], cx);
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() == Some(0) { this.update_in(cx, |this, window, cx| {
                this.draft.endpoints.retain(|e| e.protocol != protocol);
                this.draft.models.retain_mut(|m| { m.protocols.retain(|p| *p != protocol); !m.protocols.is_empty() });
                if this.draft.default_protocol == protocol { if let Some(e) = this.draft.endpoints.first() { this.draft.default_protocol = e.protocol; } }
                this.save_draft(window, cx);
            })?; }
            anyhow::Ok(())
        }).detach_and_log_err(cx);
    }
    pub(super) fn delete_provider_id(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.draft_saving { return; }
        let Some(provider) = self.providers.iter().find(|provider| provider.id == id).cloned() else { return; };
        if provider.preset_id.is_some() { self.report(t!("Built-in providers cannot be deleted; disable them instead.").to_string(), true, cx); return; }
        let title = t!("Delete {name}?", name = provider.name.clone());
        let answer = window.prompt(PromptLevel::Warning, &title, Some(&t!("The provider, models and stored credentials will be removed. Chats using it will report an unavailable service.")), &[PromptButton::new(t!("Delete")), PromptButton::cancel(t!("Cancel"))], cx);
        let service = self.service.clone(); self.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = if answer.await.ok() == Some(0) { Some(service.delete_provider(&provider.id, provider.revision).await) } else { None };
            this.update_in(cx, |this, window, cx| { this.busy = false; match result {
                Some(Ok(())) => { this.providers.retain(|p| p.id != provider.id); if this.selected.as_ref() == Some(&provider.id) { this.select(this.providers.first().map(|p| p.id.clone()), window, cx); } else { cx.notify(); } }
                Some(Err(_)) => this.report(t!("Could not delete provider").to_string(), true, cx), None => cx.notify(),
            } })
        }).detach_and_log_err(cx);
    }
    pub(super) fn set_default(&mut self, target: BackendTarget, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy { return; }
        let Some(provider) = self.provider().filter(|p| p.enabled).cloned() else { return; };
        if !provider.models.iter().any(|model| model.model_id == id && model.enabled && model.model_type == model_provider::ModelType::Chat) { return; }
        let service = self.service.clone(); self.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let result: anyhow::Result<()> = async { let mut preferences = service.load_preferences(target).await?; let revision = preferences.revision; preferences.source = ConfigSource::Managed; preferences.default_model = Some(ModelSelection::Managed { provider_id: provider.id, model_id: id }); service.save_preferences(target, revision, preferences).await?; Ok(()) }.await;
            this.update_in(cx, |this, _, cx| { this.busy = false; this.report(if result.is_ok() { t!("Default model saved for new chats").to_string() } else { t!("Could not save default model").to_string() }, result.is_err(), cx); })
        }).detach_and_log_err(cx);
    }
    pub(super) fn check_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.request_probe(false, window, cx);
    }
    pub(super) fn check_connection_saved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy { return; }
        let Some(provider) = self.provider().cloned() else { return; };
        let (service, protocol, generation) = (self.service.clone(), self.protocol, self.generation);
        let http = workspace::AppState::global(cx).client.http_client(); self.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = crate::provider_services::check_provider(&service, http, &provider, protocol).await;
            this.update_in(cx, |this, _, cx| {
                this.busy = false; if generation != this.generation { return; }
                this.key_checks = result; this.status = None; this.failed = false; cx.notify();
            })
        }).detach_and_log_err(cx); cx.notify();
    }
}

