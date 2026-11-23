use std::collections::{BTreeMap, BTreeSet, HashSet};

use editor::{Editor, EditorEvent};
use gpui::{AnyElement, App, Focusable, Context, Entity, IntoElement, ParentElement, Styled, Window, div, px};
use model_provider::{ModelType, ProviderModel, Protocol, ThinkingLevel, model_group, model_hints, model_type};
use ui::prelude::*;
use ui::{ButtonLike, Checkbox, ContextMenu, DropdownMenu, DropdownStyle};
use i18n::t;
use ui::ToggleState;

use super::{Dialog, ModelServicesView};
use super::view::{caption, field, form_field, icon_button};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Capability { Vision, Reasoning, Tools }

impl Capability {
    fn has(self, model: &ProviderModel) -> bool {
        match self { Self::Vision => model.supports_vision, Self::Reasoning => model.supports_reasoning, Self::Tools => model.supports_tools }
    }
    fn title(self) -> SharedString {
        match self { Self::Vision => t!("Images"), Self::Reasoning => t!("Reasoning"), Self::Tools => t!("Tools") }
    }
    fn icon(self) -> IconName {
        match self { Self::Vision => IconName::Eye, Self::Reasoning => IconName::Sparkle, Self::Tools => IconName::Settings }
    }
}

pub(super) struct ModelUi {
    pub(super) query: Entity<Editor>,
    cap: Option<Capability>,
    folded: HashSet<String>,
    syncing: bool,
    sync_error: Option<String>,
    found: Vec<ProviderModel>,
    picked: BTreeSet<String>,
    picker_query: Entity<Editor>,
    picker_filter: Option<ModelType>,
    picker_folded: HashSet<String>,
    add_id: Entity<Editor>,
    model_name: Entity<Editor>,
    context_window: Entity<Editor>,
    max_output: Entity<Editor>,
    editing: Option<String>,
    model_type: ModelType,
    vision: bool,
    tools: bool,
    reasoning: bool,
    efforts: Vec<ThinkingLevel>,
    protocols: Vec<Protocol>,
    _subscriptions: Vec<gpui::Subscription>,
}

impl ModelUi {
    pub(super) fn new(window: &mut Window, cx: &mut Context<ModelServicesView>) -> Self {
        let query = super::input(&t!("Search models…"), false, window, cx);
        let picker_query = super::input(&t!("Search discovered models…"), false, window, cx);
        let subscriptions = vec![
            cx.subscribe(&query, |_, _, event: &EditorEvent, cx| { if matches!(event, EditorEvent::BufferEdited) { cx.notify(); } }),
            cx.subscribe(&picker_query, |_, _, event: &EditorEvent, cx| { if matches!(event, EditorEvent::BufferEdited) { cx.notify(); } }),
        ];
        Self {
            query, cap: None, folded: HashSet::new(), syncing: false, sync_error: None,
            found: Vec::new(), picked: BTreeSet::new(),
            picker_query, picker_filter: None, picker_folded: HashSet::new(),
            add_id: super::input(&t!("Model ID"), false, window, cx),
            model_name: super::input(&t!("Model display name"), false, window, cx),
            context_window: super::input(&t!("Context window (tokens, optional)"), false, window, cx),
            max_output: super::input(&t!("Maximum output (tokens, optional)"), false, window, cx),
            editing: None, model_type: ModelType::Chat, vision: false, tools: false, reasoning: false, efforts: Vec::new(), protocols: Vec::new(), _subscriptions: subscriptions,
        }
    }

    pub(super) fn reset(&mut self, window: &mut Window, cx: &mut Context<ModelServicesView>) {
        self.cap = None; self.folded.clear(); self.syncing = false; self.sync_error = None;
        self.found.clear(); self.picked.clear(); self.picker_filter = None; self.picker_folded.clear();
        self.editing = None; self.model_type = ModelType::Chat; self.vision = false; self.tools = false; self.reasoning = false; self.efforts.clear();
        for editor in [&self.add_id, &self.model_name, &self.context_window, &self.max_output, &self.picker_query] { super::ModelServicesView::set_text(editor, "", window, cx); }
    }
    pub(super) fn set_read_only(&self, busy: bool, cx: &mut App) {
        for editor in [&self.add_id, &self.model_name, &self.context_window, &self.max_output] { editor.update(cx, |editor, _| editor.set_read_only(busy)); }
    }
}

fn type_label(kind: ModelType) -> SharedString {
    match kind { ModelType::Chat => t!("Chat"), ModelType::Image => t!("Image"), ModelType::Video => t!("Video"), ModelType::Tts => t!("TTS"), ModelType::Stt => t!("STT"), ModelType::Embedding => t!("Embedding"), ModelType::Rerank => t!("Rerank"), ModelType::Unknown => t!("Unknown") }
}
fn effort_label(level: ThinkingLevel) -> SharedString { match level { ThinkingLevel::Off => t!("Off"), ThinkingLevel::Minimal => t!("Minimal"), ThinkingLevel::Low => t!("Low"), ThinkingLevel::Medium => t!("Medium"), ThinkingLevel::High => t!("High"), ThinkingLevel::Xhigh => t!("X-High"), ThinkingLevel::Max => t!("Max") } }
fn avatar(id: &str, cx: &App) -> impl IntoElement { div().size(px(24.)).flex_shrink_0().rounded(px(6.)).bg(cx.theme().colors().element_selected).flex().items_center().justify_center().child(caption(id.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default())) }
fn model_row(content: impl IntoElement, actions: impl IntoElement, cx: &App) -> AnyElement {
    h_flex().w_full().min_w_0().min_h(px(32.)).gap(px(8.)).px(px(12.)).border_t_1().border_color(cx.theme().colors().border).child(content).child(actions).into_any_element()
}

fn apply_discovered(models: &mut Vec<ProviderModel>, found: &[ProviderModel], ids: &[String], protocol: Protocol, selected: bool) {
    if selected {
        for id in ids {
            if let Some(model) = models.iter_mut().find(|model| model.model_id == *id) {
                if !model.protocols.contains(&protocol) { model.protocols.push(protocol); }
            } else if let Some(model) = found.iter().find(|model| model.model_id == *id) {
                let mut model = model.clone();
                model.protocols = vec![protocol];
                models.push(model);
            }
        }
    } else {
        models.retain_mut(|model| {
            if ids.contains(&model.model_id) { model.protocols.retain(|line| *line != protocol); }
            !model.protocols.is_empty()
        });
    }
}

impl ModelServicesView {
    pub(super) fn render_models(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(draft) = self.provider().map(|_| &self.draft) else { return div().into_any_element(); };
        let line = self.protocol;
        let query = self.models.query.read(cx).text(cx).trim().to_lowercase();
        let cap = self.models.cap;
        let total = draft.models.iter().filter(|m| m.protocols.contains(&line)).count();
        let mut groups: BTreeMap<String, Vec<ProviderModel>> = BTreeMap::new();
        for model in draft.models.iter().filter(|m| m.protocols.contains(&line)) {
            if (query.is_empty() || model.model_id.to_lowercase().contains(&query) || model.name.to_lowercase().contains(&query)) && cap.is_none_or(|c| c.has(model)) {
                groups.entry(model_group(&model.model_id).to_owned()).or_default().push(model.clone());
            }
        }
        let names = groups.keys().cloned().collect::<Vec<_>>();
        let all_folded = !names.is_empty() && names.iter().all(|name| self.models.folded.contains(name));
        let page = cx.entity();
        let cap_menu = ContextMenu::build(window, cx, move |mut menu, _, _| {
            for (value, title) in [(None, t!("All types")), (Some(Capability::Vision), t!("Images")), (Some(Capability::Reasoning), t!("Reasoning")), (Some(Capability::Tools), t!("Tools"))] {
                let page = page.clone(); menu = menu.toggleable_entry(title, cap == value, IconPosition::Start, None, move |_, cx| page.update(cx, |this, cx| { this.models.cap = value; cx.notify(); }));
            } menu
        });
        let header = h_flex().w_full().flex_wrap().gap(px(4.)).child(Label::new(t!("Models")))
            .child(icon_button("models-fold", if all_folded { IconName::ChevronUpDown } else { IconName::ChevronDownUp }, if all_folded { t!("Expand all") } else { t!("Collapse all") }).disabled(names.is_empty()).on_click(cx.listener(move |this, _, _, cx| { if all_folded { this.models.folded.clear(); } else { this.models.folded.extend(names.iter().cloned()); } cx.notify(); })))
            .child(div().w(px(160.)).max_w_full().min_w_0().child(field(self.models.query.clone())))
            .child(DropdownMenu::new("models-cap", cap.map_or_else(|| t!("All types"), Capability::title), cap_menu).style(DropdownStyle::Outlined))
            .child(div().flex_1())
            .child(Button::new("models-sync", t!("Sync models")).start_icon(Icon::new(IconName::RotateCw)).disabled((self.busy && !self.key_saving) || self.models.syncing).on_click(cx.listener(|this, _, window, cx| this.sync_models(window, cx))))
            .child(icon_button("models-add", IconName::Plus, t!("Add model")).disabled(self.busy).on_click(cx.listener(|this, _, window, cx| this.open_add_model(window, cx))));
        let body = if groups.is_empty() { div().py(px(24.)).border_1().border_dashed().rounded(px(6.)).border_color(cx.theme().colors().border).child(caption(if total == 0 { t!("No models") } else { t!("No model matches") })).into_any_element() } else {
            v_flex().w_full().rounded(px(8.)).border_1().border_color(cx.theme().colors().border).overflow_hidden().children(groups.into_iter().enumerate().map(|(index, (group, models))| self.render_group(group, models, line, window, cx, index == 0))).into_any_element()
        };
        v_flex().w_full().gap(px(8.)).child(header).when_some(self.models.sync_error.clone(), |d, error| d.child(caption(error))).child(body).into_any_element()
    }

    fn render_group(&self, group: String, models: Vec<ProviderModel>, line: Protocol, window: &mut Window, cx: &mut Context<Self>, _first: bool) -> AnyElement {
        let folded = self.models.query.read(cx).text(cx).trim().is_empty() && self.models.folded.contains(&group); let ids = models.iter().map(|m| m.model_id.clone()).collect::<Vec<_>>(); let toggle = group.clone();
        let header = h_flex().w_full().min_h(px(32.)).gap(px(8.)).px(px(12.)).bg(cx.theme().colors().elevated_surface_background).items_center()
            .child(div().flex_1().min_w_0().child(ButtonLike::new(SharedString::from(format!("group-{group}"))).aria_label(t!("Toggle group")).child(h_flex().w_full().gap(px(8.)).child(Icon::new(if folded { IconName::ChevronRight } else { IconName::ChevronDown }).size(IconSize::Small)).child(Label::new(if group.is_empty() { t!("Other").to_string() } else { group.clone() }).truncate()).child(caption(models.len().to_string()))).on_click(cx.listener(move |this, _, _, cx| { if !this.models.folded.remove(&toggle) { this.models.folded.insert(toggle.clone()); } cx.notify(); })).full_width().tab_index(0_isize)))
            .child(icon_button(SharedString::from(format!("remove-group-{group}")), IconName::Dash, t!("Remove group")).disabled(self.busy).on_click(cx.listener(move |this, _, window, cx| { let ids = ids.clone(); this.draft.models.retain_mut(|model| { if ids.contains(&model.model_id) { model.protocols.retain(|p| *p != line); } !model.protocols.is_empty() }); this.save_draft(window, cx); })));
        v_flex().w_full().child(header).when(!folded, |d| d.children(models.into_iter().map(|model| self.render_model(model, window, cx)))).into_any_element()
    }

    fn render_model(&self, model: ProviderModel, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let id = model.model_id.clone(); let edit = id.clone(); let remove = id.clone(); let page = cx.entity();
        let allowed = model.model_type == ModelType::Chat && model.enabled;
        let default_id = id.clone();
        let menu = ContextMenu::build(window, cx, move |mut menu, _, _| {
            for target in model_provider::BackendTarget::ALL {
                let (page, id) = (page.clone(), default_id.clone());
                menu = menu.toggleable_entry_disabled_when(t!("Use for {agent}", agent = target.label()), false, !allowed, IconPosition::Start, None, move |window, cx| page.update(cx, |this, cx| this.set_default(target, id.clone(), window, cx)));
            } menu
        });
        let caps = [Capability::Vision, Capability::Reasoning, Capability::Tools].into_iter().filter(|c| c.has(&model)).map(|cap| {
            let color = match cap { Capability::Vision => cx.theme().status().success, Capability::Reasoning => cx.theme().status().info, Capability::Tools => cx.theme().status().warning };
            div().size(px(24.)).rounded(px(6.)).bg(color.opacity(0.15)).flex().items_center().justify_center().child(Icon::new(cap.icon()).size(IconSize::XSmall))
        }).collect::<Vec<_>>();
        let content = h_flex().flex_1().min_w_0().gap(px(8.)).child(avatar(&model.model_id, cx)).child(div().flex_1().min_w_0().truncate().child(Label::new(model.model_id.clone()))).when(model.model_type != ModelType::Chat, |d| d.child(caption(type_label(model.model_type)))).child(h_flex().gap(px(4.)).children(caps));
        let actions = h_flex().gap(px(2.))
            .child(icon_button(SharedString::from(format!("model-settings-{id}")), IconName::Settings, t!("Model settings")).disabled(self.busy).on_click(cx.listener(move |this, _, window, cx| this.open_model_settings(edit.clone(), window, cx))))
            .child(icon_button(SharedString::from(format!("model-remove-{id}")), IconName::Dash, t!("Remove model")).disabled(self.busy).on_click(cx.listener(move |this, _, window, cx| { let line = this.protocol; this.draft.models.retain_mut(|m| { if m.model_id == remove { m.protocols.retain(|p| *p != line); } !m.protocols.is_empty() }); this.save_draft(window, cx); })))
            .child(super::view::icon_menu(SharedString::from(format!("model-menu-{id}")), IconName::Ellipsis, t!("Use model"), menu, false, self.busy));
        model_row(content, actions, cx).into_any_element()
    }

    pub(super) fn open_add_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.draft_saving { return; }
        self.models.editing = None; self.models.model_type = ModelType::Chat; self.models.vision = false; self.models.tools = false; self.models.reasoning = false; self.models.efforts.clear();
        for editor in [&self.models.add_id, &self.models.model_name, &self.models.context_window, &self.models.max_output] { Self::set_text(editor, "", window, cx); }
        self.show_dialog(Dialog::AddModel, window, cx);
        self.models.add_id.focus_handle(cx).focus(window, cx);
    }

    pub(super) fn open_model_settings(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.draft_saving { return; }
        let Some(model) = self.draft.models.iter().find(|m| m.model_id == id).cloned() else { return; };
        self.models.editing = Some(id); self.models.model_type = model.model_type; self.models.vision = model.supports_vision; self.models.tools = model.supports_tools; self.models.reasoning = model.supports_reasoning; self.models.efforts = model.efforts.clone();
        self.models.protocols = model.protocols.clone();
        Self::set_text(&self.models.add_id, &model.model_id, window, cx); Self::set_text(&self.models.model_name, &model.name, window, cx); Self::set_text(&self.models.context_window, &model.context_window.map(|v| v.to_string()).unwrap_or_default(), window, cx); Self::set_text(&self.models.max_output, &model.max_output_tokens.map(|v| v.to_string()).unwrap_or_default(), window, cx);
        self.show_dialog(Dialog::Model, window, cx);
    }

    pub(super) fn render_add_model(&self, _window: &mut Window, _cx: &mut Context<Self>) -> AnyElement { v_flex().gap(px(12.)).child(form_field(t!("Model ID"), field(self.models.add_id.clone()))).into_any_element() }

    pub(super) fn render_model_settings(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let page = cx.entity();
        let menu = ContextMenu::build(window, cx, move |mut menu, _, _| { for kind in ModelType::ALL { let page = page.clone(); menu = menu.entry(type_label(kind), None, move |_, cx| page.update(cx, |this, cx| { this.models.model_type = kind; cx.notify(); })); } menu });
        let caps = h_flex().gap(px(8.)).flex_wrap().children([Capability::Vision, Capability::Reasoning, Capability::Tools].into_iter().map(|cap| {
            let checked = match cap { Capability::Vision => self.models.vision, Capability::Reasoning => self.models.reasoning, Capability::Tools => self.models.tools };
            let page = cx.entity();
            Checkbox::new(SharedString::from(format!("cap-{cap:?}")), if checked { ToggleState::Selected } else { ToggleState::Unselected }).label(cap.title()).on_click(move |state, _, cx| page.update(cx, |this, cx| { let on = *state == ToggleState::Selected; match cap { Capability::Vision => this.models.vision = on, Capability::Reasoning => this.models.reasoning = on, Capability::Tools => this.models.tools = on }; cx.notify(); }))
        }));
        let efforts = self.models.reasoning.then(|| h_flex().gap(px(4.)).flex_wrap().children(ThinkingLevel::ALL.into_iter().map(|level| {
            let page = cx.entity(); Button::new(SharedString::from(format!("effort-{level:?}")), effort_label(level)).toggle_state(self.models.efforts.contains(&level)).style(if self.models.efforts.contains(&level) { ButtonStyle::Filled } else { ButtonStyle::Outlined }).disabled(self.busy).on_click(move |_, _, cx| page.update(cx, |this, cx| { if this.models.efforts.contains(&level) { this.models.efforts.retain(|v| *v != level); } else { this.models.efforts.push(level); this.models.efforts.sort(); } cx.notify(); }))
        })));
        let lines = h_flex().gap(px(12.)).flex_wrap().children(self.draft.endpoints.iter().map(|endpoint| {
            let protocol = endpoint.protocol; let page = cx.entity();
            h_flex().gap(px(4.)).child(Checkbox::new(SharedString::from(format!("model-line-{protocol:?}")), if self.models.protocols.contains(&protocol) { ToggleState::Selected } else { ToggleState::Unselected }).label(super::protocol_label(protocol)).on_click(move |state, _, cx| page.update(cx, |this, cx| { this.models.protocols.retain(|p| *p != protocol); if *state == ToggleState::Selected { this.models.protocols.push(protocol); } cx.notify(); }))).when(protocol == self.draft.default_protocol, |row| row.child(caption(t!("Default"))))
        }));
        v_flex().gap(px(16.)).child(caption(self.models.add_id.read(cx).text(cx)))
            .child(h_flex().gap(px(12.)).child(div().flex_1().min_w_0().child(form_field(t!("Model display name"), field(self.models.model_name.clone())))).child(form_field(t!("Model type"), DropdownMenu::new("model-type", type_label(self.models.model_type), menu).style(DropdownStyle::Outlined))))
            .child(form_field(t!("Model endpoints"), lines))
            .child(h_flex().gap(px(12.)).child(div().flex_1().min_w_0().child(form_field(t!("Context window"), field(self.models.context_window.clone())))).child(div().flex_1().min_w_0().child(form_field(t!("Maximum output"), field(self.models.max_output.clone())))))
            .child(form_field(t!("Capabilities"), caps)).children(efforts.map(|control| form_field(t!("Reasoning levels"), control))).child(caption(t!("Changes apply to the next run."))).into_any_element()
    }

    pub(super) fn save_model_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.draft_saving { return; }
        let id = self.models.add_id.read(cx).text(cx).trim().to_owned();
        if id.is_empty() { self.report(t!("Model ID is required").to_string(), true, cx); return; }
        if self.models.editing.is_none() {
            if let Some(model) = self.draft.models.iter_mut().find(|m| m.model_id == id) { if !model.protocols.contains(&self.protocol) { model.protocols.push(self.protocol); } }
            else { let hints = model_hints(&id); self.draft.models.push(ProviderModel { model_id: id.clone(), name: id.clone(), model_type: model_type(&id), protocols: vec![self.protocol], context_window: hints.context_window, max_output_tokens: hints.max_output_tokens, supports_vision: hints.supports_vision, supports_tools: hints.supports_tools, supports_reasoning: false, efforts: Vec::new(), enabled: true }); }
            self.save_draft(window, cx); return;
        }
        let number = |editor: &Entity<Editor>| -> Result<Option<u64>, ()> { let raw = editor.read(cx).text(cx); if raw.trim().is_empty() { Ok(None) } else { raw.trim().parse::<u64>().ok().filter(|v| *v > 0).map(Some).ok_or(()) } };
        let (Ok(context_window), Ok(max_output_tokens)) = (number(&self.models.context_window), number(&self.models.max_output)) else { self.report(t!("Token limits must be positive integers").to_string(), true, cx); return; };
        if self.models.reasoning && self.models.efforts.is_empty() { self.report(t!("Select at least one reasoning level").to_string(), true, cx); return; }
        let existing = self.models.editing.as_ref().and_then(|old| self.draft.models.iter().find(|m| &m.model_id == old));
        let model = ProviderModel { model_id: id.clone(), model_type: self.models.model_type, name: { let value = self.models.model_name.read(cx).text(cx).trim().to_owned(); if value.is_empty() { id.clone() } else { value } }, protocols: self.models.protocols.clone(), context_window, max_output_tokens, supports_vision: self.models.vision, supports_tools: self.models.tools, supports_reasoning: self.models.reasoning, efforts: if self.models.reasoning { self.models.efforts.clone() } else { Vec::new() }, enabled: existing.is_none_or(|m| m.enabled) };
        if let Some(old) = self.models.editing.clone() { self.draft.models.retain(|m| m.model_id != old); }
        if let Some(slot) = self.draft.models.iter_mut().find(|m| m.model_id == id) { *slot = model; } else { self.draft.models.push(model); }
        self.save_draft(window, cx);
    }

    pub(super) fn sync_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.request_probe(true, window, cx);
    }
    pub(super) fn sync_models_saved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.draft_saving || self.models.syncing { return; }
        let Some(provider) = self.provider().cloned() else { return; };
        let (service, http, protocol, generation, id) = (self.service.clone(), workspace::AppState::global(cx).client.http_client(), self.protocol, self.generation, provider.id.clone());
        self.models.syncing = true; self.busy = true; self.models.sync_error = None; cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = crate::provider_services::discover_models(&service, http, &provider, protocol).await;
            this.update_in(cx, |this, window, cx| {
                this.busy = false; this.models.syncing = false;
                if generation != this.generation || this.selected.as_deref() != Some(id.as_str()) || this.protocol != protocol { return; }
                match result { Ok(mut found) => {
                    for model in &mut found { if let Some(saved) = this.draft.models.iter().find(|m| m.model_id == model.model_id) { model.model_type = saved.model_type; } }
                    this.models.picked = this.draft.models.iter().filter(|m| m.protocols.contains(&protocol)).map(|m| m.model_id.clone()).collect(); this.models.found = found; this.show_dialog(Dialog::Discovery, window, cx);
                }, Err(error) => { this.models.sync_error = Some(error.to_string()); this.report(error.to_string(), true, cx); } }
            })
        }).detach_and_log_err(cx);
    }

    fn toggle_discovered(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let selected = !self.models.picked.contains(&id);
        self.set_discovered_selection(vec![id], selected, window, cx);
    }

    fn set_discovered_selection(&mut self, ids: Vec<String>, selected: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.draft_saving || ids.is_empty() { return; }
        let Some(provider) = self.provider().cloned() else { return; };
        let protocol = self.protocol; let generation = self.generation;
        let previous = self.models.picked.clone();
        for id in &ids { if selected { self.models.picked.insert(id.clone()); } else { self.models.picked.remove(id); } }
        let found = self.models.found.clone(); let service = self.service.clone(); self.busy = true; cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = async {
                let current = service.get_provider(&provider.id).await?.ok_or_else(|| anyhow::anyhow!("provider was removed"))?;
                let mut draft = super::draft_of(&current);
                apply_discovered(&mut draft.models, &found, &ids, protocol, selected);
                Ok::<_, anyhow::Error>(service.update_provider(&current.id, current.revision, draft).await?)
            }.await;
            this.update_in(cx, |this, _, cx| {
                this.busy = false;
                if generation != this.generation || this.protocol != protocol { return; }
                match result { Ok(snapshot) => this.stored(snapshot, cx), Err(error) => { this.models.picked = previous; this.report(error.to_string(), true, cx); } }
                cx.notify();
            })
        }).detach_and_log_err(cx);
    }

    pub(super) fn render_discovery(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let query = self.models.picker_query.read(cx).text(cx).trim().to_lowercase();
        let filter = self.models.picker_filter;
        let ids = self.models.found.iter().filter(|model| model.model_id.to_lowercase().contains(&query) && filter.is_none_or(|kind| model.model_type == kind)).map(|model| model.model_id.clone()).collect::<Vec<_>>();
        let all_picked = !ids.is_empty() && ids.iter().all(|id| self.models.picked.contains(id));
        let tabs = h_flex().gap(px(4.)).flex_wrap().children(std::iter::once(None).chain(ModelType::ALL.map(Some)).map(|kind| {
            let count = self.models.found.iter().filter(|m| m.model_id.to_lowercase().contains(&query) && kind.is_none_or(|k| m.model_type == k)).count();
            Button::new(SharedString::from(format!("filter-{kind:?}")), format!("{} {count}", kind.map_or_else(|| t!("All"), type_label))).toggle_state(filter == kind).disabled(self.busy || (kind.is_some() && count == 0)).on_click(cx.listener(move |this, _, _, cx| { this.models.picker_filter = kind; cx.notify(); }))
        }));
        let mut groups: BTreeMap<String, Vec<ProviderModel>> = BTreeMap::new();
        for model in &self.models.found { if (query.is_empty() || model.model_id.to_lowercase().contains(&query)) && filter.is_none_or(|kind| model.model_type == kind) { groups.entry(model_group(&model.model_id).to_owned()).or_default().push(model.clone()); } }
        let list = v_flex().id("discovery-list").max_h(px(440.)).min_h_0().overflow_y_scroll().children(groups.into_iter().map(|(group, models)| {
            let folded = query.is_empty() && self.models.picker_folded.contains(&group); let toggle = group.clone();
            v_flex().child(ButtonLike::new(SharedString::from(format!("discovery-group-{group}"))).aria_label(t!("Toggle group")).child(h_flex().w_full().min_h(px(32.)).gap(px(8.)).child(Icon::new(if folded { IconName::ChevronRight } else { IconName::ChevronDown }).size(IconSize::Small)).child(Label::new(if group.is_empty() { t!("Other").to_string() } else { group.clone() })).child(caption(models.len().to_string()))).on_click(cx.listener(move |this, _, _, cx| { if !this.models.picker_folded.remove(&toggle) { this.models.picker_folded.insert(toggle.clone()); } cx.notify(); })).full_width().tab_index(0_isize))
                .when(!folded, |d| d.children(models.into_iter().map(|model| {
                    let id = model.model_id.clone(); let picked = self.models.picked.contains(&id); let page = cx.entity(); let type_id = id.clone(); let current = model.model_type;
                    let menu = ContextMenu::build(window, cx, move |mut menu, _, _| { for kind in ModelType::ALL { let (page, id) = (page.clone(), type_id.clone()); menu = menu.toggleable_entry(type_label(kind), current == kind, IconPosition::Start, None, move |window, cx| page.update(cx, |this, cx| this.set_discovered_type(id.clone(), kind, window, cx))); } menu });
                    div().w_full().when(picked, |row| row.bg(cx.theme().colors().element_selected)).child(model_row(h_flex().flex_1().min_w_0().gap(px(8.)).child(avatar(&id, cx)).child(div().flex_1().min_w_0().truncate().child(Label::new(id.clone()))).when(current == ModelType::Chat && model.supports_vision, |row| row.child(caption(t!("Images")))).when(picked, |row| row.child(caption(t!("Picked")))), h_flex().gap(px(4.)).child(DropdownMenu::new(SharedString::from(format!("discovered-type-{id}")), type_label(current), menu).style(DropdownStyle::Ghost).disabled(self.busy || self.draft_saving)).child(icon_button(SharedString::from(format!("pick-{id}")), if picked { IconName::Dash } else { IconName::Plus }, if picked { t!("Remove model") } else { t!("Add model") }).disabled(self.busy || self.draft_saving).on_click(cx.listener(move |this, _, window, cx| this.toggle_discovered(id.clone(), window, cx)))), cx))
                })))
        }));
        let bulk = Button::new("discovery-bulk", if all_picked { t!("Remove all") } else { t!("Add all") }).disabled(self.busy || self.draft_saving || ids.is_empty()).on_click(cx.listener(move |this, _, window, cx| this.set_discovered_selection(ids.clone(), !all_picked, window, cx)));
        v_flex().gap(px(12.)).child(h_flex().gap(px(8.)).child(div().flex_1().min_w_0().child(field(self.models.picker_query.clone()))).child(bulk)).child(tabs).child(list).child(caption(t!("{count} selected", count = self.models.picked.len()))).into_any_element()
    }
    fn set_discovered_type(&mut self, id: String, kind: ModelType, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.draft_saving { return; }
        if !self.models.picked.contains(&id) { if let Some(model) = self.models.found.iter_mut().find(|m| m.model_id == id) { model.model_type = kind; } cx.notify(); return; }
        let Some(provider) = self.provider().cloned() else { return; }; let service = self.service.clone(); self.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let result: anyhow::Result<_> = async { let current = service.get_provider(&provider.id).await?.ok_or_else(|| anyhow::anyhow!("Provider unavailable"))?; let mut draft = super::draft_of(&current); if let Some(model) = draft.models.iter_mut().find(|m| m.model_id == id) { model.model_type = kind; } Ok(service.update_provider(&current.id, current.revision, draft).await?) }.await;
            this.update_in(cx, |this, _, cx| { this.busy = false; match result { Ok(snapshot) => { this.stored(snapshot, cx); if let Some(model) = this.models.found.iter_mut().find(|m| m.model_id == id) { model.model_type = kind; } }, Err(_) => this.report(t!("Could not save changes. Your draft is retained; reload if the provider changed elsewhere.").to_string(), true, cx) } cx.notify(); })
        }).detach_and_log_err(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filtered_bulk_preserves_unlisted_models_and_other_lines() {
        let model = |id: &str, protocols: Vec<Protocol>| ProviderModel {
            model_id: id.into(), name: format!("Custom {id}"), model_type: ModelType::Chat,
            protocols, context_window: Some(42), max_output_tokens: None,
            supports_vision: true, supports_tools: true, supports_reasoning: false,
            efforts: Vec::new(), enabled: true,
        };
        let unlisted = model("unlisted", vec![Protocol::OpenaiChat]);
        let hidden = model("hidden-by-filter", vec![Protocol::OpenaiChat]);
        let shown = model("shown", vec![Protocol::OpenaiChat, Protocol::OpenaiResponses]);
        let discovered = model("new-shown", vec![Protocol::OpenaiResponses]);
        let found = vec![shown.clone(), hidden.clone(), discovered];
        let mut models = vec![unlisted.clone(), hidden.clone(), shown];
        apply_discovered(&mut models, &found, &["shown".into()], Protocol::OpenaiChat, false);
        assert_eq!(models[0], unlisted);
        assert_eq!(models[1], hidden);
        assert_eq!(models[2].protocols, vec![Protocol::OpenaiResponses]);
        apply_discovered(&mut models, &found, &["shown".into(), "new-shown".into()], Protocol::OpenaiChat, true);
        assert_eq!(models[0], unlisted);
        assert_eq!(models[1], hidden);
        assert_eq!(models[2].name, "Custom shown");
        assert_eq!(models[2].context_window, Some(42));
        assert_eq!(models[2].protocols, vec![Protocol::OpenaiResponses, Protocol::OpenaiChat]);
        assert_eq!(models[3].model_id, "new-shown");
        assert_eq!(models[3].protocols, vec![Protocol::OpenaiChat]);
    }

    #[gpui::test]
    async fn model_dialog_preserves_metadata_when_adding_existing_model_to_another_line(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| { workspace::AppState::test(cx); editor::init(cx); });
        let directory = tempfile::tempdir().unwrap();
        crate::session::tests::new_store(directory.path(), cx);
        cx.update(crate::provider_services::ProviderServices::install);
        let project = project::Project::test(project::FakeFs::new(cx.executor()), [], cx).await;
        let (page, cx) = cx.add_window_view(|window, cx| ModelServicesView::new(window, cx));
        cx.run_until_parked();
        page.update_in(cx, |page, window, cx| {
            page.open_provider(true, window, cx);
            ModelServicesView::set_text(&page.name, "Metadata", window, cx);
            ModelServicesView::set_text(&page.endpoint, "http://127.0.0.1:12345/v1", window, cx);
            page.save_provider_dialog(window, cx);
        });
        for _ in 0..100 { cx.run_until_parked(); if !page.read_with(cx, |page, _| page.busy || page.draft_saving) { break; } std::thread::sleep(std::time::Duration::from_millis(5)); }
        page.update_in(cx, |page, window, cx| {
            page.open_add_model(window, cx);
            ModelServicesView::set_text(&page.models.add_id, "gpt-local", window, cx);
            page.save_model_dialog(window, cx);
        });
        for _ in 0..100 { cx.run_until_parked(); if !page.read_with(cx, |page, _| page.busy || page.draft_saving) { break; } std::thread::sleep(std::time::Duration::from_millis(5)); }
        page.update_in(cx, |page, window, cx| {
            page.open_model_settings("gpt-local".into(), window, cx);
            ModelServicesView::set_text(&page.models.model_name, "My model", window, cx);
            page.models.reasoning = true;
            page.models.efforts = vec![ThinkingLevel::Off, ThinkingLevel::High];
            page.save_model_dialog(window, cx);
        });
        for _ in 0..100 { cx.run_until_parked(); if !page.read_with(cx, |page, _| page.busy || page.draft_saving) { break; } std::thread::sleep(std::time::Duration::from_millis(5)); }
        page.update_in(cx, |page, window, cx| {
            page.open_line(None, window, cx);
            ModelServicesView::set_text(&page.endpoint, "http://127.0.0.1:12346/v1", window, cx);
            page.save_line(window, cx);
        });
        for _ in 0..100 { cx.run_until_parked(); if !page.read_with(cx, |page, _| page.busy || page.draft_saving) { break; } std::thread::sleep(std::time::Duration::from_millis(5)); }
        page.update_in(cx, |page, window, cx| {
            page.change_protocol(Protocol::OpenaiResponses, window, cx);
            page.open_add_model(window, cx);
            ModelServicesView::set_text(&page.models.add_id, "gpt-local", window, cx);
            page.save_model_dialog(window, cx);
        });
        for _ in 0..100 { cx.run_until_parked(); if !page.read_with(cx, |page, _| page.busy || page.draft_saving) { break; } std::thread::sleep(std::time::Duration::from_millis(5)); }
        page.read_with(cx, |page, _| {
            let model = &page.provider().unwrap().models[0];
            assert_eq!(model.name, "My model");
            assert_eq!(model.efforts, vec![ThinkingLevel::Off, ThinkingLevel::High]);
            assert_eq!(model.context_window, Some(128_000));
            assert_eq!(model.protocols, vec![Protocol::OpenaiChat, Protocol::OpenaiResponses]);
        });
        page.update_in(cx, |page, window, cx| {
            let mut found = page.draft.models[0].clone();
            found.model_id = "discovered-only".into();
            found.protocols = vec![Protocol::OpenaiResponses];
            page.models.found = vec![found];
            page.models.picked.clear();
            page.show_dialog(Dialog::Discovery, window, cx);
            page.toggle_discovered("discovered-only".into(), window, cx);
        });
        for _ in 0..100 { cx.run_until_parked(); if !page.read_with(cx, |page, _| page.busy || page.draft_saving) { break; } std::thread::sleep(std::time::Duration::from_millis(5)); }
        page.read_with(cx, |page, _| {
            assert!(page.provider().unwrap().models.iter().any(|m| m.model_id == "gpt-local" && m.name == "My model"));
            assert!(page.provider().unwrap().models.iter().any(|m| m.model_id == "discovered-only"));
        });
        page.update_in(cx, |page, window, cx| page.toggle_discovered("discovered-only".into(), window, cx));
        for _ in 0..100 { cx.run_until_parked(); if !page.read_with(cx, |page, _| page.busy || page.draft_saving) { break; } std::thread::sleep(std::time::Duration::from_millis(5)); }
        page.read_with(cx, |page, _| {
            assert_eq!(page.provider().unwrap().models.len(), 1);
            assert_eq!(page.provider().unwrap().models[0].model_id, "gpt-local");
        });
        drop(project);
    }
}
