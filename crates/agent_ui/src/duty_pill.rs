use crate::{AgentPanel, AgentPanelEvent};
use agent_client_protocol::v2::schema as acp_v2;
use agent_settings::AgentSettings;
use chrono::{DateTime, Utc};
use gpui::{
    App, Context, Entity, EntityId, IntoElement, ParentElement, Render, SharedString, Styled,
    Subscription, WeakEntity, Window,
};
use project::DisableAiSettings;
use settings::Settings;
use std::collections::HashSet;
use ui::{Tooltip, prelude::*};
use workspace::{HideStatusItem, PathList, StatusItemView, Workspace, item::ItemHandle};

/// The oldest waiting thread across the workspace group: `(created_at,
/// session id, work dirs, title)`.
type OldestWaitingThread = (
    DateTime<Utc>,
    acp_v2::SessionId,
    Option<PathList>,
    Option<SharedString>,
);

/// Right-side status bar item aggregating live agent thread states across the
/// workspace's sidebar threads. Renders `N waiting · N running · N error`
/// indicators, skipping zero counts, and nothing at all when every count is
/// zero. Clicking opens the agent panel and jumps to the oldest thread
/// waiting for confirmation.
pub struct DutyPill {
    workspace: WeakEntity<Workspace>,
    /// Panels across the workspace group that already have subscriptions
    /// attached, tracked by id because panels register asynchronously.
    observed_panels: HashSet<EntityId>,
    _workspace_observation: Subscription,
    _workspace_subscription: Subscription,
    _panel_subscriptions: Vec<Subscription>,
}

impl DutyPill {
    pub fn new(workspace: &Entity<Workspace>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            workspace: workspace.downgrade(),
            observed_panels: HashSet::new(),
            _workspace_observation: cx.observe(workspace, |this, _, cx| {
                this.sync_panel_subscriptions(cx);
                cx.notify();
            }),
            // Panels are registered asynchronously after the status bar is
            // built, so watch for panel additions explicitly, mirroring the
            // threads sidebar.
            _workspace_subscription: cx.subscribe(
                workspace,
                |this: &mut DutyPill,
                 _: Entity<Workspace>,
                 event: &workspace::Event,
                 cx: &mut Context<Self>| {
                    if matches!(event, workspace::Event::PanelAdded(_)) {
                        this.sync_panel_subscriptions(cx);
                        cx.notify();
                    }
                },
            ),
            _panel_subscriptions: Vec::new(),
        };
        this.sync_panel_subscriptions(cx);
        this
    }

    /// Panels register asynchronously, so (re)attach to every panel across
    /// the workspace group that isn't subscribed yet, mirroring the threads
    /// sidebar's panel subscription handling.
    fn sync_panel_subscriptions(&mut self, cx: &mut Context<Self>) {
        for workspace in self.group_workspaces(cx) {
            let Some(panel) = workspace.read(cx).panel::<AgentPanel>(cx) else {
                continue;
            };
            if !self.observed_panels.insert(panel.entity_id()) {
                continue;
            }
            self._panel_subscriptions
                .push(cx.observe(&panel, |_, _, cx| cx.notify()));
            self._panel_subscriptions.push(cx.subscribe(
                &panel,
                |_: &mut DutyPill,
                 _: Entity<AgentPanel>,
                 _: &AgentPanelEvent,
                 cx: &mut Context<Self>| {
                    cx.notify();
                },
            ));
        }
    }

    /// All workspaces in this workspace's project group, matching the threads
    /// sidebar's aggregation.
    fn group_workspaces(&self, cx: &App) -> Vec<Entity<Workspace>> {
        let Some(workspace) = self.workspace.upgrade() else {
            return Vec::new();
        };
        workspace
            .read(cx)
            .multi_workspace()
            .and_then(|multi_workspace| multi_workspace.upgrade())
            .map(|multi_workspace| multi_workspace.read(cx).workspaces().cloned().collect())
            .unwrap_or_else(|| vec![workspace])
    }

    fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // If clicked before the agent panel has registered (panels are added
        // asynchronously), both `open_panel` and `toggle_panel_focus` are
        // no-ops, so this click is dead; the workspace observation re-syncs
        // once the panel appears and the pill renders again afterwards.
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let mut oldest_waiting: Option<OldestWaitingThread> = None;
        for workspace in &self.group_workspaces(cx) {
            let Some(panel) = workspace.read(cx).panel::<AgentPanel>(cx) else {
                continue;
            };
            if let Some(candidate) = panel.read(cx).oldest_waiting_thread(cx)
                && oldest_waiting
                    .as_ref()
                    .is_none_or(|(current, ..)| candidate.0 < *current)
            {
                oldest_waiting = Some(candidate);
            }
        }
        workspace.update(cx, |workspace, cx| {
            if let Some((_, session_id, work_dirs, title)) = oldest_waiting {
                workspace.open_panel::<AgentPanel>(window, cx);
                if let Some(panel) = workspace.panel::<AgentPanel>(cx) {
                    panel.update(cx, |panel, cx| {
                        panel.open_thread(session_id, work_dirs, title, window, cx)
                    });
                }
            } else {
                workspace.toggle_panel_focus::<AgentPanel>(window, cx);
            }
        });
    }
}

impl Render for DutyPill {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if DisableAiSettings::get_global(cx).disable_ai || !AgentSettings::get_global(cx).enabled {
            return div().into_any_element();
        }

        let mut waiting = 0;
        let mut running = 0;
        let mut error = 0;
        for workspace in &self.group_workspaces(cx) {
            let Some(panel) = workspace.read(cx).panel::<AgentPanel>(cx) else {
                continue;
            };
            let (panel_waiting, panel_running, panel_error) =
                panel.read(cx).thread_status_counts(cx);
            waiting += panel_waiting;
            running += panel_running;
            error += panel_error;
        }

        if waiting + running + error == 0 {
            return div().into_any_element();
        }

        let mut segments = Vec::new();
        if waiting > 0 {
            segments.push((
                Icon::new(IconName::Warning)
                    .size(IconSize::Small)
                    .color(Color::Warning),
                format!("{waiting} waiting"),
                Color::Warning,
            ));
        }
        if running > 0 {
            segments.push((
                Icon::new(IconName::LoadCircle)
                    .size(IconSize::Small)
                    .color(Color::Muted),
                format!("{running} running"),
                Color::Muted,
            ));
        }
        if error > 0 {
            segments.push((
                Icon::new(IconName::XCircle)
                    .size(IconSize::Small)
                    .color(Color::Error),
                format!("{error} error"),
                Color::Error,
            ));
        }

        h_flex()
            .id("agent-duty-pill")
            .cursor_pointer()
            .gap_1p5()
            .rounded_sm()
            .px_1()
            .hover(|style| style.bg(cx.theme().colors().element_hover))
            .tooltip(Tooltip::text("Agent threads waiting, running, or errored"))
            .on_click(cx.listener(|this, _, window, cx| this.activate(window, cx)))
            .children(
                segments
                    .into_iter()
                    .enumerate()
                    .flat_map(|(index, (icon, label, color))| {
                        let mut items = Vec::new();
                        if index > 0 {
                            items.push(Label::new("·").color(Color::Muted).into_any_element());
                        }
                        items.push(
                            h_flex()
                                .gap_0p5()
                                .child(icon)
                                .child(Label::new(label).color(color))
                                .into_any_element(),
                        );
                        items
                    }),
            )
            .into_any_element()
    }
}

impl StatusItemView for DutyPill {
    fn set_active_pane_item(
        &mut self,
        _active_pane_item: Option<&dyn ItemHandle>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn hide_setting(&self, _cx: &App) -> Option<HideStatusItem> {
        None
    }
}
