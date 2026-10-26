//! Project indexes are fetched on workers; opening a row materializes the chat.
use super::*;

impl Shell {
    pub(super) fn refresh_remote_projects(&mut self, cx: &mut Context<Self>) {
        if self.remote_refreshing || !self.workspace.project.starts_with("remote://") {
            return;
        }
        self.remote_refreshing = true;
        let project_key = self.workspace.project.clone();
        let store = self.store.clone();
        let client = self.remote.clone();
        let revision = self.remote_revisions.get(&project_key).cloned();
        let conditional = crate::paseo_host::active();
        let work = cx.background_executor().spawn(async move {
            crate::runtime::remote::refresh_project_index(
                &store,
                &client,
                &project_key,
                revision.as_deref(),
                conditional,
            )
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.remote_refreshing = false;
                match result {
                    Ok(Some(index)) => {
                        this.remote_details.extend(index.presentation);
                        let project = index.project;
                        if let Some(revision) = index.revision {
                            this.remote_revisions.insert(project.clone(), revision);
                        } else {
                            this.remote_revisions.remove(&project);
                        }
                        let summaries = index.summaries;
                        let retained = index.retained;
                        let removed: Vec<_> = this
                            .order
                            .iter()
                            .copied()
                            .filter(|id| {
                                this.session_summaries
                                    .get(id)
                                    .is_some_and(|s| s.cwd == project && !retained.contains(id))
                            })
                            .collect();
                        for id in removed {
                            this.remote_marks.remove(&id);
                            this.started_by.remove(&id);
                            this.order.retain(|existing| *existing != id);
                            if let Some(view) = this.sessions.get(&id) {
                                view.update(cx, |view, cx| {
                                    view.remote_connection_error(
                                        "This conversation was deleted on the host.",
                                        cx,
                                    )
                                });
                            }
                            // Open tabs retain an explanatory placeholder. A stale
                            // tab must not be rebound to a new host conversation.
                            if this.workspace.tab_of(id).is_none() {
                                this.session_summaries.remove(&id);
                                this.sessions.remove(&id);
                            }
                        }
                        for (summary, running, waiting, draft) in summaries {
                            let id = summary.id;
                            this.started_by.remove(&id);
                            if let Some(parent) = index.started_by.get(&id) {
                                this.started_by.insert(id, *parent);
                            }
                            this.remote_marks.insert(id, (running, waiting));
                            if draft {
                                this.draft_sessions.insert(id);
                            } else {
                                this.draft_sessions.remove(&id);
                            }
                            if !this.session_summaries.contains_key(&id) {
                                this.order.push(id);
                            }
                            if let Some(view) = this.sessions.get(&id) {
                                view.update(cx, |view, cx| {
                                    if !view.busy {
                                        view.summary = summary.clone();
                                        cx.notify();
                                    }
                                });
                            }
                            this.session_summaries.insert(id, summary);
                        }
                        if this.workspace.project == project {
                            this.invalidate_chrome(cx);
                            this.refresh_sidebar_structure(cx);
                        }
                    }
                    Ok(None) => return,
                    Err(error) => {
                        // A transient disconnect retains the last host index.
                        // The active conversation exposes the error and Retry.
                        if let Some(id) = this.workspace.active
                            && let Some(view) =
                                this.sessions.get(&this.workspace.session_for_pane(id))
                        {
                            view.update(cx, |view, cx| view.remote_connection_error(&error, cx));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
