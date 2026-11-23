use super::*;
use gpui::{PromptButton, PromptLevel};

impl CodingPartnersView {
    fn finish_uninstall(&mut self, result: anyhow::Result<()>, window: &mut Window, cx: &mut Context<Self>) {
        self.installing = None;
        self.removing = false;
        self.status = Some(match result {
            Ok(()) => (t!("Engine removed. Chat history, model services and credentials are unchanged. Another installation may still be detected.").to_string(), false),
            Err(error) => (format!("{}: {error}", t!("Could not uninstall coding partner")), true),
        });
        self.reload(window, cx);
        cx.notify();
    }

    pub(super) fn confirm_uninstall(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.installing.is_some() || self.loading { return; }
        if crate::session::working_agent_count(cx) > 0 {
            self.status = Some((t!("Stop running agent chats before uninstalling a coding partner.").to_string(), true));
            cx.notify();
            return;
        }
        let target = self.selected;
        let Some((_, Some(executable), builtin)) = self.installs.iter().find(|(backend, _, _)| *backend == target).cloned() else { return; };
        self.installing = Some(target);
        self.removing = true;
        self.status = None;
        cx.spawn_in(window, async move |this, cx| {
            let plan = if builtin { Ok(None) } else {
                let discovered = executable.clone();
                cx.background_spawn(async move { crate::engine_uninstall::detect(target, discovered).await.map(Some) }).await
            };
            let plan = match plan {
                Ok(plan) => plan,
                Err(error) => {
                    return this.update_in(cx, |this, window, cx| this.finish_uninstall(Err(error), window, cx));
                }
            };
            let details = plan.as_ref().map(|plan| plan.description()).unwrap_or_else(|| format!("{}\n{}", t!("Built-in"), executable.display()));
            let title = t!("Uninstall {agent}?", agent = name(target));
            let message = format!("{}\n\n{}", details, t!("This removes the engine package from this computer. Chat history, model services, credentials and shared runtimes will be kept. Other applications using this engine may be affected."));
            let answer = this.update_in(cx, |_, window, cx| window.prompt(PromptLevel::Warning, &title, Some(&message), &[PromptButton::cancel(t!("Cancel")), PromptButton::new(t!("Uninstall"))], cx))?;
            if answer.await.ok() != Some(1) {
                return this.update(cx, |this, cx| { this.installing = None; this.removing = false; cx.notify(); });
            }
            let can_remove = this.update(cx, |_, cx| crate::session::working_agent_count(cx) == 0)?;
            let result = if !can_remove {
                Err(anyhow::anyhow!(t!("Stop running agent chats before uninstalling a coding partner.").to_string()))
            } else if let Some(plan) = plan { cx.background_spawn(async move { plan.execute().await }).await }
            else if crate::engine_launch::managed_entry(target).as_ref() != Some(&executable) {
                Err(anyhow::anyhow!(t!("The installation changed. Refresh and confirm uninstall again.").to_string()))
            } else { crate::engine_launch::uninstall(target).await };
            this.update_in(cx, |this, window, cx| this.finish_uninstall(result, window, cx))
        }).detach_and_log_err(cx);
        cx.notify();
    }
}
