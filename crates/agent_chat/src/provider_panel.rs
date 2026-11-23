use gpui::{App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Render, Window, div, px};
use ui::prelude::*;
use ui::Tooltip;
use i18n::t;
use workspace::{ModalView, Workspace};

use crate::ModelServicesView;

pub(crate) fn open(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    workspace.toggle_modal(window, cx, move |window, cx| {
        let view = cx.new(|cx| ModelServicesView::new(window, cx));
        ProviderPanel { view }
    });
}

/// The chat shortcut uses the same editor as the Settings page.
pub(crate) struct ProviderPanel { pub(super) view: Entity<ModelServicesView> }
impl EventEmitter<DismissEvent> for ProviderPanel {}
impl ModalView for ProviderPanel {
    fn fade_out_background(&self) -> bool { true }
}
impl Focusable for ProviderPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle { self.view.focus_handle(cx) }
}
impl Render for ProviderPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let size = window.viewport_size();
        v_flex().id("model-services-modal").debug_selector(|| "model-services-modal".into()).key_context("menu")
            .w((size.width - px(48.)).max(px(0.)).min(px(1100.)))
            .h((size.height - px(112.)).max(px(0.)))
            .overflow_hidden().rounded_lg().border_1().border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().elevated_surface_background).shadow_lg().occlude()
            .on_action(cx.listener(|_, _: &menu::Cancel, _, cx| cx.emit(DismissEvent)))
            .child(h_flex().flex_none().w_full().px_4().py_2().gap_2()
                .border_b_1().border_color(cx.theme().colors().border)
                .child(Label::new(t!("Model Services"))).child(div().flex_1())
                .child(IconButton::new("model-services-modal-close", IconName::Close)
                    .aria_label(t!("Close")).tooltip(Tooltip::text(t!("Close"))).tab_index(0_isize)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))))
            .child(div().flex_1().w_full().min_h_0().min_w_0().child(self.view.clone()))
    }
}
