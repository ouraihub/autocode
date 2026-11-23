use super::*;
use gpui::{AnyElement, IntoElement, ParentElement, Render, Styled, div, px};
use ui::{ButtonLike, ContextMenu, DropdownMenu, DropdownStyle, IconButton, PopoverMenu, Tooltip};

pub(super) fn field(editor: Entity<Editor>) -> impl IntoElement {
    div().w_full().min_w_0().min_h(px(32.)).px(px(8.)).py(px(6.)).rounded(px(6.)).border_1().border_color(gpui::hsla(0., 0., 0.5, 0.25)).child(editor)
}
pub(super) fn caption(text: impl Into<SharedString>) -> Label { Label::new(text).size(LabelSize::Small).color(Color::Muted) }
pub(super) fn form_field(title: impl Into<SharedString>, control: impl IntoElement) -> impl IntoElement {
    v_flex().gap(px(4.)).min_w_0().child(caption(title)).child(control)
}
pub(super) fn icon_button(id: impl Into<gpui::ElementId>, icon: IconName, title: impl Into<SharedString>) -> IconButton {
    let title = title.into();
    IconButton::new(id, icon).aria_label(title.clone()).tooltip(Tooltip::text(title.clone())).icon_size(IconSize::Small).tab_index(0_isize)
}
pub(super) fn icon_menu(id: impl Into<gpui::ElementId>, icon: IconName, title: impl Into<SharedString>, menu: Entity<ContextMenu>, selected: bool, disabled: bool) -> impl IntoElement {
    let id = id.into();
    PopoverMenu::new((id.clone(), "popover")).trigger(icon_button(id, icon, title).icon_color(if selected { Color::Accent } else { Color::Default }).disabled(disabled)).menu(move |_, _| Some(menu.clone()))
}
fn avatar(name: &str, cx: &App) -> impl IntoElement {
    div().size(px(24.)).flex_shrink_0().rounded(px(6.)).bg(cx.theme().colors().element_selected)
        .flex().items_center().justify_center().child(caption(name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default()))
}
impl ModelServicesView {
    fn render_rail(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let page = cx.entity(); let selected_filter = self.filter;
        let menu = ContextMenu::build(window, cx, move |mut menu, _, _| {
            for (value, title) in [(None, t!("All providers")), (Some(true), t!("Enabled")), (Some(false), t!("Disabled"))] {
                let page = page.clone(); menu = menu.toggleable_entry(title, selected_filter == value, IconPosition::Start, None, move |_, cx| page.update(cx, |this, cx| { this.filter = value; cx.notify(); }));
            } menu
        });
        let query = self.search.read(cx).text(cx).to_lowercase();
        let rows = self.providers.iter().filter(|p| p.name.to_lowercase().contains(&query) && self.filter.is_none_or(|enabled| p.enabled == enabled)).map(|provider| {
            let id = provider.id.clone(); let edit = id.clone(); let del = id.clone();
            let built_in = provider.preset_id.is_some();
            let page = cx.entity();
            let menu = ContextMenu::build(window, cx, move |menu, _, _| {
                let (page_edit, page_delete) = (page.clone(), page.clone()); let (edit, del) = (edit.clone(), del.clone());
                menu.entry(t!("Edit"), None, move |window, cx| page_edit.update(cx, |this, cx| this.open_provider_for(edit.clone(), window, cx)))
                    .toggleable_entry_disabled_when(t!("Delete"), false, built_in, IconPosition::Start, None, move |window, cx| page_delete.update(cx, |this, cx| this.delete_provider_id(del.clone(), window, cx)))
            });
            h_flex().w_full().min_h(px(36.)).gap(px(4.)).rounded(px(6.)).when(self.selected.as_ref() == Some(&id), |d| d.bg(cx.theme().colors().element_selected))
                .child(div().flex_1().min_w_0().child(ButtonLike::new(SharedString::from(format!("provider-{id}"))).aria_label(provider.name.clone()).disabled(self.busy || self.draft_saving).child(h_flex().w_full().min_w_0().gap(px(8.)).px(px(6.))
                    .child(avatar(&provider.name, cx)).child(div().flex_1().min_w_0().truncate().child(Label::new(provider.name.clone())))
                    .when(provider.enabled, |d| d.child(div().size(px(6.)).rounded_full().bg(if provider.keys.iter().any(|k| k.enabled) { cx.theme().status().success } else { cx.theme().status().warning }))))
                    .on_click(cx.listener(move |this, _, window, cx| this.select(Some(id.clone()), window, cx)))))
                .child(div().flex_shrink_0().child(icon_menu(SharedString::from(format!("provider-menu-{}", provider.id)), IconName::Ellipsis, t!("Provider settings"), menu, false, self.busy || self.draft_saving)))
                .into_any_element()
        }).collect::<Vec<_>>();
        let empty = rows.is_empty() && self.loaded;
        v_flex().w_full().min_w_0().gap(px(4.))
            .child(h_flex().gap(px(6.)).child(h_flex().flex_1().min_w_0().gap(px(4.)).child(Icon::new(IconName::MagnifyingGlass).size(IconSize::Small)).child(field(self.search.clone()))).child(div().flex_shrink_0().child(icon_menu("provider-filter", IconName::Filter, t!("All providers"), menu, self.filter.is_some(), false))))
            .when(empty, |d| d.child(div().py(px(24.)).border_1().border_dashed().border_color(cx.theme().colors().border).rounded(px(6.)).child(caption(t!("No matching providers")))))
            .children(rows).child(Button::new("new-model-service", t!("New Provider")).start_icon(Icon::new(IconName::Plus)).full_width().disabled(self.busy).on_click(cx.listener(|this, _, window, cx| this.open_provider(true, window, cx))))
            .into_any_element()
    }
    fn render_detail(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(provider) = self.provider() else {
            return v_flex().flex_1().items_center().py(px(40.)).gap(px(4.)).child(Label::new(if self.loaded { t!("No provider selected") } else { t!("Loading…") }))
                .child(caption(t!("Add a provider to configure its API key, endpoint and models."))).into_any_element();
        };
        let protocol = self.protocol;
        let page = cx.entity(); let endpoints = provider.endpoints.clone();
        let menu = ContextMenu::build(window, cx, move |mut menu, _, _| {
            for endpoint in &endpoints { let page = page.clone(); let p = endpoint.protocol; menu = menu.toggleable_entry(format!("{} · {}", protocol_label(p), endpoint.base_url), protocol == p, IconPosition::Start, None, move |window, cx| page.update(cx, |this, cx| this.change_protocol(p, window, cx))); } menu
        });
        v_flex().flex_1().min_w_0().gap(px(16.))
            .child(h_flex().gap(px(8.)).child(div().min_w_0().truncate().child(Label::new(provider.name.clone()).size(LabelSize::Large)))
                .child(icon_button("provider-settings", IconName::Settings, t!("Provider settings")).disabled(self.busy).on_click(cx.listener(|this, _, window, cx| this.open_provider(false, window, cx))))
                .child(div().flex_1()).child(Switch::new("provider-enabled", if self.dialog.is_none() { if self.draft.enabled { ToggleState::Selected } else { ToggleState::Unselected } } else if provider.enabled { ToggleState::Selected } else { ToggleState::Unselected }).aria_label(t!("Enabled")).color(ui::SwitchColor::Custom(cx.theme().status().success)).disabled(self.busy || self.dialog.is_some()).on_click(cx.listener(|this, state, window, cx| { this.draft.enabled = *state == ToggleState::Selected; this.save_draft(window, cx); }))))
            .when_some(self.status.clone(), |d, status| d.child(h_flex().gap(px(8.)).child(Label::new(status).size(LabelSize::Small).color(if self.failed { Color::Error } else { Color::Success })).when(self.save_failed, |row| row.child(Button::new("provider-retry", t!("Retry")).disabled(self.draft_saving || self.busy).on_click(cx.listener(|this, _, window, cx| this.save_draft(window, cx)))))))
            .child(self.render_key_section(cx))
            .child(v_flex().gap(px(4.)).child(caption(t!("Key usage strategy"))).child(Label::new(t!("Round robin"))).child(caption(if provider.routing == RoutingMode::Gateway { t!("Rotate enabled keys for each request.") } else { t!("Rotate enabled keys between agent launches.") })))
            .child(v_flex().gap(px(6.))
                .child(h_flex().gap(px(8.)).child(caption(t!("API endpoint"))).when(provider.endpoints.len() < 3, |d| d.child(Button::new("add-line", t!("Add endpoint")).size(ButtonSize::Compact).disabled(self.busy).on_click(cx.listener(|this, _, window, cx| this.open_line(None, window, cx))))))
                .when(provider.endpoints.len() > 1, |d| d.child(DropdownMenu::new("provider-line", format!("{} · {}", protocol_label(protocol), self.endpoint.read(cx).text(cx)), menu).full_width(true).style(DropdownStyle::Outlined)))
                .child(h_flex().gap(px(8.)).child(div().flex_1().min_w_0().child(field(self.endpoint.clone())))
                    .child(icon_button("line-settings", IconName::Settings, t!("Endpoint settings")).disabled(self.busy).on_click(cx.listener(move |this, _, window, cx| this.open_line(Some(protocol), window, cx)))))
                .children(crate::provider_services::models_url(&self.endpoint.read(cx).text(cx)).ok().map(|url| caption(format!("GET {url}")))))
            .child(self.render_models(window, cx)).into_any_element()
    }
    fn protocol_control(&self, allowed: Vec<Protocol>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let page = cx.entity(); let current = self.protocol;
        let menu = ContextMenu::build(window, cx, move |mut menu, _, _| { for protocol in &allowed {
            let (protocol, page) = (*protocol, page.clone()); menu = menu.toggleable_entry(protocol_label(protocol), current == protocol, IconPosition::Start, None, move |_, cx| page.update(cx, |this, cx| { this.protocol = protocol; cx.notify(); }));
        } menu });
        DropdownMenu::new("edit-protocol", protocol_label(self.protocol), menu).full_width(true).style(DropdownStyle::Outlined).into_any_element()
    }
    fn render_provider_form(&self, creating: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let page = cx.entity();
        let route = ContextMenu::build(window, cx, move |menu, _, _| { let direct = page.clone(); menu.entry(t!("Direct"), None, move |_, cx| direct.update(cx, |this, cx| { this.draft.routing = RoutingMode::Direct; cx.notify(); }))
            .entry(t!("Gateway"), None, { let page = page.clone(); move |_, cx| page.update(cx, |this, cx| { this.draft.routing = RoutingMode::Gateway; cx.notify(); }) }) });
        let page = cx.entity();
        let auth = ContextMenu::build(window, cx, move |menu, _, _| { let key = page.clone(); menu.entry(t!("API key"), None, move |_, cx| key.update(cx, |this, cx| { this.draft.auth = AuthMode::ApiKey; cx.notify(); }))
            .entry(t!("No authentication"), None, { let page = page.clone(); move |_, cx| page.update(cx, |this, cx| { this.draft.auth = AuthMode::None; cx.notify(); }) }) });
        v_flex().gap(px(16.)).child(form_field(t!("Provider name"), field(self.name.clone())))
            .when(creating, |d| d.child(form_field(t!("Protocol"), self.protocol_control(vec![Protocol::OpenaiChat, Protocol::OpenaiResponses, Protocol::AnthropicMessages], window, cx))).child(form_field(t!("API endpoint"), field(self.endpoint.clone()))))
            .child(form_field(t!("Access mode"), DropdownMenu::new("provider-routing", if self.draft.routing == RoutingMode::Direct { t!("Direct") } else { t!("Gateway") }, route).full_width(true).style(DropdownStyle::Outlined)))
            .child(form_field(t!("Authentication"), DropdownMenu::new("provider-auth", if self.draft.auth == AuthMode::ApiKey { t!("API key") } else { t!("No authentication") }, auth).full_width(true).style(DropdownStyle::Outlined)))
            .child(caption(t!("Gateway uses a local authenticated endpoint and rotates keys per request."))).into_any_element()
    }
    fn render_line_form(&self, editing: Option<Protocol>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let allowed = [Protocol::OpenaiChat, Protocol::OpenaiResponses, Protocol::AnthropicMessages].into_iter().filter(|p| Some(*p) == editing || !self.draft.endpoints.iter().any(|e| e.protocol == *p)).collect();
        v_flex().gap(px(16.)).child(form_field(t!("Protocol"), self.protocol_control(allowed, window, cx)))
            .child(form_field(t!("API endpoint"), field(self.endpoint.clone())))
            .child(v_flex().gap(px(6.)).child(caption(t!("Request headers"))).children(self.headers.iter().enumerate().map(|(index, (_, name, value))| {
                h_flex().gap(px(6.)).child(div().flex_1().min_w_0().child(field(name.clone()))).child(div().flex_1().min_w_0().child(field(value.clone())))
                    .child(icon_button(("remove-header", index), IconName::Close, t!("Delete")).disabled(self.busy).on_click(cx.listener(move |this, _, _, cx| { this.headers.remove(index); cx.notify(); })))
            })).child(Button::new("add-header", t!("Add header")).start_icon(Icon::new(IconName::Plus)).disabled(self.busy).on_click(cx.listener(|this, _, window, cx| {
                this.headers.push((None, input("X-Custom-Header", false, window, cx), input(&t!("Header value"), true, window, cx))); cx.notify();
            }))))
            .when(editing.is_some(), |d| d.child(h_flex().gap(px(8.)).child(Button::new("default-line", t!("Use this protocol by default")).toggle_state(self.draft.default_protocol == self.protocol).disabled(self.busy).on_click(cx.listener(|this, _, _, cx| { this.draft.default_protocol = this.protocol; cx.notify(); })))
                .child(div().flex_1()).child(Button::new("remove-line", t!("Remove endpoint")).style(ButtonStyle::Tinted(ui::TintColor::Error)).disabled(self.busy || self.draft.endpoints.len() <= 1).on_click(cx.listener(|this, _, window, cx| this.remove_line(window, cx))))))
            .into_any_element()
    }
    fn submit_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.dialog { Some(Dialog::Provider(_)) => self.save_provider_dialog(window, cx), Some(Dialog::Line(_)) => self.save_line(window, cx), Some(Dialog::AddModel | Dialog::Model) => self.save_model_dialog(window, cx), Some(Dialog::Keys) => self.save_key(window, cx), _ => {} }
    }
    fn render_dialog(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(dialog) = self.dialog else { return div().into_any_element(); };
        let (title, width, body) = match dialog {
            Dialog::Provider(creating) => (if creating { t!("New Provider") } else { t!("Provider settings") }, 440., self.render_provider_form(creating, window, cx)),
            Dialog::Line(editing) => (t!("Endpoint settings"), 520., self.render_line_form(editing, window, cx)),
            Dialog::Keys => (t!("Manage keys"), 520., self.render_keys_dialog(cx)),
            Dialog::AddModel => (t!("Add model"), 420., self.render_add_model(window, cx)),
            Dialog::Model => (t!("Model settings"), 520., self.render_model_settings(window, cx)),
            Dialog::Discovery => (t!("Sync models"), 640., self.render_discovery(window, cx)),
        };
        div().absolute().inset_0().flex().items_center().justify_center().bg(cx.theme().colors().editor_background.opacity(0.75))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(v_flex().id("provider-dialog").track_focus(&self.dialog_focus).key_context("ModelServiceDialog").w(px(width).min(window.viewport_size().width - px(64.))).max_h(window.viewport_size().height - px(96.)).min_h_0().p(px(24.)).gap(px(16.)).rounded(px(8.)).border_1().border_color(cx.theme().colors().border).bg(cx.theme().colors().elevated_surface_background).shadow_lg()
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" { cx.stop_propagation(); this.close_dialog(window, cx); }
                    else if event.keystroke.key == "enter" && !matches!(this.dialog, Some(Dialog::Discovery)) { cx.stop_propagation(); this.submit_dialog(window, cx); }
                }))
                .child(Label::new(title)).child(div().id("provider-form-scroll").min_h_0().overflow_y_scroll().child(body))
                .when(self.failed, |d| d.children(self.status.clone().map(|status| Label::new(status).color(Color::Error).size(LabelSize::Small))))
                .child(h_flex().gap(px(8.)).child(div().flex_1()).child(Button::new("provider-dialog-close", if matches!(dialog, Dialog::Keys | Dialog::Discovery) { t!("Close") } else { t!("Cancel") }).disabled(self.busy || self.draft_saving).on_click(cx.listener(|this, _, window, cx| this.close_dialog(window, cx))))
                    .when(!matches!(dialog, Dialog::Keys | Dialog::Discovery), |d| d.child(Button::new("provider-dialog-save", t!("Save")).style(ButtonStyle::Filled).disabled(self.busy || self.draft_saving).on_click(cx.listener(|this, _, window, cx| this.submit_dialog(window, cx)))))))
            .into_any_element()
    }
}
impl Render for ModelServicesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let locked = self.busy || (self.dialog.is_some() && self.draft_saving);
        for editor in [&self.name, &self.key, &self.key_label] { editor.update(cx, |editor, _| editor.set_read_only(self.busy || self.draft_saving)); }
        self.endpoint.update(cx, |editor, _| editor.set_read_only(locked));
        self.models.set_read_only(locked, cx);
        for (_, name, value) in &self.headers { for editor in [name, value] { editor.update(cx, |editor, _| editor.set_read_only(locked)); } }
        let viewport = window.viewport_size().width;
        let content_width = viewport.min(px(1040.)) - px(64.) - if viewport < px(700.) { px(0.) } else { px(192.) };
        let wide = content_width >= px(700.);
        let rail = self.render_rail(window, cx); let detail = self.render_detail(window, cx);
        div().relative().id("model-services-page").track_focus(&self.focus).size_full().min_w_0().min_h_0()
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if this.dialog.is_none() && event.keystroke.key == "enter" { if this.endpoint.focus_handle(cx).is_focused(window) { this.commit_endpoint(window, cx); } else if this.key.focus_handle(cx).is_focused(window) { this.add_first_key(window, cx); } }
            }))
            .child(div().id("model-services-scroll").size_full().overflow_y_scroll().p(px(16.)).child(div().flex().w_full().min_w_0().gap(px(16.)).when(wide, |d| d.flex_row().items_start()).when(!wide, |d| d.flex_col())
                .child(div().flex_shrink_0().when(wide, |d| d.w(px(208.))).when(!wide, |d| d.w_full()).child(rail)).child(detail)))
            .when(self.dialog.is_some(), |d| d.child(self.render_dialog(window, cx)))
    }
}
