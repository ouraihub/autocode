use super::*;
use gpui::{AnyElement, Render, div, px};
use ui::ButtonLike;

fn caption(text: impl Into<SharedString>) -> Label { Label::new(text).size(LabelSize::Small).color(Color::Muted) }
fn avatar(target: BackendTarget) -> impl IntoElement {
    Icon::new(kind(target).icon()).size(IconSize::Medium)
}

impl CodingPartnersView {
    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let target = self.selected;
        let install = self.installs.iter().find(|(backend, _, _)| *backend == target);
        let path = install.and_then(|(_, path, _)| path.as_ref());
        let builtin = install.is_some_and(|(_, _, builtin)| *builtin);
        let is_default = AgentChatSettings::get_global(cx).default_agent == Some(kind(target));
        let busy = self.installing.is_some();
        v_flex().p(px(16.)).gap(px(8.)).rounded(px(8.)).border_1().border_color(cx.theme().colors().border)
            .child(h_flex().gap(px(8.)).child(avatar(target)).child(Label::new(name(target)))
                .when(path.is_some(), |row| row.child(caption(if builtin { t!("Built-in") } else { t!("System") })))
                .when(is_default, |row| row.child(Label::new(t!("Default")).color(Color::Accent)))
                .child(div().flex_1())
                .when(path.is_some() && !is_default, |row| row.child(
                    Button::new("coding-default", t!("Set as default")).style(ButtonStyle::Outlined).disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| this.set_default(cx)))))
                .when(path.is_none(), |row| row.child(
                    Button::new("coding-install", if self.installing == Some(target) { t!("Installing…") } else { t!("Install") })
                        .style(ButtonStyle::Outlined).disabled(busy || self.loading)
                        .on_click(cx.listener(|this, _, window, cx| this.install(false, window, cx)))))
                .when(builtin, |row| row.child(
                    Button::new("coding-uninstall", t!("Uninstall")).style(ButtonStyle::Outlined).disabled(busy)
                        .on_click(cx.listener(|this, _, window, cx| this.install(true, window, cx))))))
            .children(path.map(|path| div().min_w_0().truncate().child(caption(path.display().to_string()))))
            .when(self.installing == Some(target), |column| column.child(caption(t!("First installation downloads a runtime and can take several minutes."))))
            .when_some(self.status.clone(), |column, (message, failed)| column.child(
                Label::new(message).size(LabelSize::Small).color(if failed { Color::Error } else { Color::Success })))
            .into_any_element()
    }

    fn render_provider_row(&self, index: usize, title: String, description: Option<SharedString>, choice: EngineProviderChoice, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.preferences.iter().find(|p| p.target == self.selected).and_then(|p| p.provider_choice.as_ref()) == Some(&choice);
        h_flex().min_h(px(52.)).px(px(12.)).py(px(8.)).gap(px(12.)).rounded(px(8.)).border_1()
            .border_color(if selected { cx.theme().colors().border_focused } else { cx.theme().colors().border })
            .child(v_flex().flex_1().min_w_0().child(Label::new(title)).children(description.map(caption)))
            .child(Button::new(("coding-enable", index), if selected { t!("Enabled") } else { t!("Enable") })
                .style(ButtonStyle::Outlined).disabled(selected || self.saving || self.loading)
                .on_click(cx.listener(move |this, _, window, cx| this.choose(Some(choice.clone()), window, cx))))
            .into_any_element()
    }

    fn render_providers(&self, cx: &mut Context<Self>) -> AnyElement {
        let query = self.search.read(cx).text(cx).trim().to_lowercase();
        let chosen = self.preferences.iter().find(|p| p.target == self.selected).and_then(|p| p.provider_choice.as_ref());
        let mut options = vec![
            (t!("Unified gateway").to_string(), Some(t!("All providers use a local gateway; real keys never enter the engine process.")), EngineProviderChoice::Gateway),
            (t!("{agent} official", agent = name(self.selected)).to_string(), Some(t!("Use models and login from the engine's own configuration directory.")), EngineProviderChoice::Official),
        ];
        options.extend(self.providers.iter().filter(|p| p.enabled).map(|p| (p.name.clone(), None, EngineProviderChoice::Provider(p.id.clone()))));
        options.retain(|(title, _, _)| title.to_lowercase().contains(&query));
        let empty = options.is_empty();
        v_flex().gap(px(8.))
            .child(h_flex().gap(px(8.)).child(Label::new(t!("Model providers"))).child(div().flex_1())
                .child(div().w(px(220.)).min_w_0().px(px(8.)).py(px(6.)).rounded(px(6.)).border_1().border_color(cx.theme().colors().border).child(self.search.clone())))
            .child(h_flex().gap(px(8.)).child(caption(if chosen.is_none() {
                t!("Not selected: use all enabled providers with their own access mode.")
            } else { t!("Changes apply to new chats.") }))
                .when(chosen.is_some(), |row| row.child(Button::new("coding-reset-source", t!("Reset")).size(ButtonSize::Compact)
                    .disabled(self.saving || self.loading).on_click(cx.listener(|this, _, window, cx| this.choose(None, window, cx))))))
            .children(options.into_iter().enumerate().map(|(index, (title, description, choice))| self.render_provider_row(index, title, description, choice, cx)))
            .when(empty, |column| column.child(caption(t!("No matching providers"))))
            .child(Button::new("coding-open-providers", t!("Add providers in Settings → Model Services")).full_width()
                .style(ButtonStyle::OutlinedGhost).on_click(cx.listener(|this, _, _, cx| { this.open_model_services = true; cx.notify(); })))
            .into_any_element()
    }
}

impl Render for CodingPartnersView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let wide = window.viewport_size().width >= px(1000.);
        let rail = div().flex().gap(px(8.))
            .when(wide, |rail| rail.flex_col())
            .when(!wide, |rail| rail.flex_row().flex_wrap())
            .children(BackendTarget::ALL.into_iter().map(|target| {
                let selected = self.selected == target;
                let install = self.installs.iter().find(|(backend, _, _)| *backend == target);
                let status = if self.installing == Some(target) { t!("Installing…") }
                    else if self.loading { t!("Loading…") }
                    else if let Some((_, Some(_), builtin)) = install { if *builtin { t!("Built-in") } else { t!("System") } }
                    else { t!("Not installed") };
                div().min_w_0()
                    .when(!wide, |card| card.flex_1().min_w(px(170.)))
                    .child(ButtonLike::new(("coding-engine", target as usize))
                        .full_width().height(px(68.).into()).tab_index(0_isize)
                        .aria_label(name(target)).toggle_state(selected)
                        .style(ButtonStyle::Outlined)
                        .selected_style(ButtonStyle::Tinted(ui::TintColor::Accent))
                        .child(h_flex().w_full().min_w_0().px(px(8.)).gap(px(10.))
                            .child(div().flex_shrink_0().child(avatar(target)))
                            .child(v_flex().flex_1().min_w_0().gap(px(4.)).text_left()
                                .child(Label::new(name(target)).truncate())
                                .child(caption(status))))
                        .on_click(cx.listener(move |this, _, _, cx| { this.selected = target; this.status = None; cx.notify(); })))
            }));
        div().id("coding-partners-page").debug_selector(|| "coding-partners-page".into()).track_focus(&self.focus).size_full().min_h_0().min_w_0()
            .child(div().id("coding-partners-scroll").size_full().overflow_y_scroll().p(px(16.))
                .child(div().flex().w_full().min_w_0().gap(px(16.)).when(wide, |d| d.flex_row().items_start()).when(!wide, |d| d.flex_col())
                    .child(div().flex_shrink_0().when(wide, |d| d.w(px(208.))).when(!wide, |d| d.w_full()).child(rail))
                    .child(v_flex().flex_1().min_w_0().gap(px(16.)).child(self.render_header(cx)).child(self.render_providers(cx)))))
    }
}
