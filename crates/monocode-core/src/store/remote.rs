//! Host-owned sessions keep only their desktop identity and command receipts here.
//! Transcripts remain on the host and use the same lossless block projection as
//! desktop conversations; no remote row is inserted into the local session table.
use super::{shared, Session, Store};
use crate::{
    agent::{Settings, TurnInfo},
    remote::{Connections, Machine},
    transcript::Transcript,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

const PROJECTS: &str = "remote-projects.v2";
const SESSIONS: &str = "remote-sessions.v1";

// GPUI enables serde_json/preserve_order; canonicalize settings HashMaps
// independently of their fresh random seeds (including nested turn metadata).
pub(super) fn encode_settings(settings: &Settings) -> Result<String, String> {
    let mut value = serde_json::to_value(settings).map_err(|e| e.to_string())?;
    value.sort_all_objects();
    serde_json::to_string(&value).map_err(|e| e.to_string())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProject {
    pub key: String,
    pub environment_id: String,
    pub project_id: String,
    pub cwd: String,
}

/// Local bytes needed to retry the same journaled attachment references.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteUpload {
    pub command_id: String,
    pub id: String,
    pub path: std::path::PathBuf,
    pub size: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteSession {
    pub summary: Session,
    pub project: RemoteProject,
    pub session_id: Option<String>,
    /// Host parent identity; resolved only within this binding's environment.
    #[serde(default)]
    pub parent_session_id: Option<String>,
    /// Recorded before dispatch, and removed only after a definite receipt.
    #[serde(default)]
    pub outbox: Vec<Value>,
    #[serde(default)]
    pub uploads: Vec<RemoteUpload>,
    /// Definite host rejections retain the original text for explicit recovery.
    #[serde(default)]
    pub rejected: Option<Value>,
    /// Last full host snapshot, used to reconcile changes from other desktops.
    #[serde(default)]
    pub configuration: Option<Value>,
    #[serde(default)]
    pub running: bool,
    #[serde(default)]
    pub needs_input: bool,
}
/// Daemon-authoritative UI metadata is reconstructed after reconnect, not journaled.
#[derive(Default)]
pub(super) struct StatusCache {
    environments: std::collections::HashSet<String>,
    values: HashMap<i64, (u64, bool, bool)>,
}
impl RemoteSession {
    pub fn machine(&self, client: &Connections) -> Result<Machine, String> {
        client
            .list()?
            .into_iter()
            .find(|machine| machine.environment_id == self.project.environment_id)
            .ok_or_else(|| {
                "Machine is no longer connected. Reconnect in Settings → Connections.".into()
            })
    }
}
fn host_settings(project: &RemoteProject, host: &Value) -> Settings {
    let mut settings = Settings::default();
    settings.model = host["model"].as_str().map(str::to_owned);
    settings.access =
        crate::agent::AccessMode::from_id(host["runtimeMode"].as_str().unwrap_or("supervised"));
    settings.model_settings = host
        .get("modelSettings")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    settings.effort = settings
        .model_settings
        .get("reasoningEffort")
        .or_else(|| settings.model_settings.get("effort"))
        .cloned();
    settings.fast = settings
        .model_settings
        .get("fast")
        .is_some_and(|value| value == "true");
    settings.service_tier = settings.model_settings.get("serviceTier").cloned();
    settings.thinking = settings
        .model_settings
        .get("thinking")
        .and_then(|value| value.parse::<bool>().ok());
    if let Some(cwd) = host["cwd"].as_str().filter(|cwd| *cwd != project.cwd) {
        settings.worktree_cwd = Some(crate::remote::remote_path(&project.environment_id, cwd));
    }
    settings
}

fn adopt_host_configuration(settings: &mut Settings, host: Settings) {
    settings.model = host.model;
    settings.access = host.access;
    settings.model_settings = host.model_settings;
    settings.effort = host.effort;
    settings.fast = host.fast;
    settings.service_tier = host.service_tier;
    settings.thinking = host.thinking;
    settings.worktree_cwd = host.worktree_cwd;
    settings.revision = settings.revision.saturating_add(1);
}

impl Store {
    pub(super) fn remote_started_links(&self) -> Result<HashMap<i64, i64>, String> {
        let sessions = self.remote_sessions()?;
        let mut ids = HashMap::new();
        for remote in sessions.values() {
            if let Some(id) = &remote.session_id {
                ids.entry((remote.project.environment_id.clone(), id.clone()))
                    .or_insert(remote.summary.id);
            }
        }
        let links: HashMap<_, _> = sessions.values().filter_map(|remote| {
            let parent = remote.parent_session_id.as_ref()?;
            let parent = ids.get(&(remote.project.environment_id.clone(), parent.clone()))?;
            (*parent != remote.summary.id).then_some((remote.summary.id, *parent))
        }).collect();
        Ok(links.iter().filter_map(|(child, parent)| {
            let mut seen = std::collections::HashSet::from([*child]);
            let mut current = *parent;
            loop {
                if !seen.insert(current) {
                    return None;
                }
                match links.get(&current) {
                    Some(parent) => current = *parent,
                    None => return Some((*child, *parent)),
                }
            }
        }).collect())
    }

    /// Opt in only for daemon-authoritative, isolated clients.
    pub fn ephemeral_remote_status(&self, environment: &str) {
        self.remote_status
            .borrow_mut()
            .environments
            .insert(environment.into());
    }
    fn daemon_status(&self, project: &RemoteProject) -> bool {
        self.remote_status
            .borrow()
            .environments
            .contains(&project.environment_id)
    }
    fn commit_remote_sessions(
        &self,
        sessions: &BTreeMap<i64, RemoteSession>,
        values: Vec<(String, String)>,
    ) -> Result<(), String> {
        self.commit_remote_sessions_patch(sessions, values, &[])
    }
    fn commit_remote_sessions_patch(
        &self,
        sessions: &BTreeMap<i64, RemoteSession>,
        mut values: Vec<(String, String)>,
        remove: &[String],
    ) -> Result<(), String> {
        let mut durable = sessions.clone();
        let mut status = HashMap::new();
        for (id, remote) in &mut durable {
            // Host attempt hashes depend on outbox/rejected command key order.
            // Only reconstructed metadata may be sorted, never journal payloads.
            if let Some(configuration) = remote.configuration.as_mut() {
                configuration.sort_all_objects();
            }
            if self.daemon_status(&remote.project) {
                status.insert(
                    *id,
                    (
                        remote.summary.updated_at,
                        remote.running,
                        remote.needs_input,
                    ),
                );
                remote.summary.updated_at = 0;
                remote.running = false;
                remote.needs_input = false;
            }
        }
        values.push((
            SESSIONS.into(),
            serde_json::to_string(&durable).map_err(|e| e.to_string())?,
        ));
        self.patch_settings(&values, remove)?;
        // Failed durable commits must not publish any part of the new UI state.
        self.remote_status.borrow_mut().values = status;
        Ok(())
    }
    /// Evict reconstructible bundle caches, never local drafts or command recovery.
    /// Project descriptors remain available for Archived/on-demand reload.
    pub fn compact_remote_cache(
        &self,
        environment: &str,
        visible: &std::collections::HashSet<String>,
    ) -> Result<(), String> {
        if !self
            .remote_status
            .borrow()
            .environments
            .contains(environment)
        {
            return Err("Cache eviction requires daemon-authoritative status".into());
        }
        let mut sessions = self.stored_remote_sessions()?;
        let mut remove = Vec::new();
        let defaults = serde_json::to_value(Settings::default()).map_err(|e| e.to_string())?;
        sessions.retain(|id, remote| {
            if remote.project.environment_id != environment
                || remote.session_id.is_none()
                || !remote.outbox.is_empty()
                || !remote.uploads.is_empty()
                || remote.rejected.is_some()
            {
                return true;
            }
            let key = format!("remote-agent:{id}");
            // Only the exact old summary-import shape is disposable. Keep explicit
            // edits, full configurations, unknown fields and malformed settings.
            let disposable = if remote.configuration.is_none() {
                match self.setting(&key) {
                    Ok(None) => true,
                    Ok(Some(raw)) => {
                        serde_json::from_str::<Value>(&raw)
                            .ok()
                            .is_some_and(|mut value| {
                                if !value.is_object() {
                                    return false;
                                }
                                for field in [
                                    "model",
                                    "access",
                                    "model_settings",
                                    "effort",
                                    "fast",
                                    "service_tier",
                                    "thinking",
                                    "worktree_cwd",
                                ] {
                                    value[field] = defaults[field].clone();
                                }
                                value == defaults
                            })
                    }
                    Err(_) => false,
                }
            } else {
                false
            };
            if disposable {
                remove.push(key);
            }
            visible.contains(&remote.project.key) || !disposable
        });
        self.commit_remote_sessions_patch(&sessions, Vec::new(), &remove)
    }
    pub fn remote_projects(&self) -> Result<BTreeMap<String, RemoteProject>, String> {
        let mut projects: BTreeMap<String, RemoteProject> = self
            .desktop_preferences()
            .and_then(|prefs| prefs.get("monocode.remote-projects.v2"))
            .map(|raw| {
                serde_json::from_str(raw)
                    .map_err(|e| format!("Cannot read desktop remote projects: {e}"))
            })
            .unwrap_or_else(|| Ok(BTreeMap::new()))?;
        if let Some(raw) = self.setting(PROJECTS)? {
            projects.extend(
                serde_json::from_str::<BTreeMap<String, RemoteProject>>(&raw)
                    .map_err(|e| format!("Cannot read remote projects: {e}"))?,
            );
        }
        Ok(projects)
    }
    pub fn remember_remote_project(
        &self,
        environment_id: &str,
        project: &Value,
    ) -> Result<RemoteProject, String> {
        let cwd = project["cwd"]
            .as_str()
            .ok_or("Host project has no directory")?;
        let id = project["id"]
            .as_str()
            .ok_or("Host project has no identity")?;
        let remote = RemoteProject {
            key: crate::remote::remote_path(
                environment_id,
                cwd.replace('\\', "/").trim_end_matches('/'),
            ),
            environment_id: environment_id.into(),
            project_id: id.into(),
            cwd: cwd.into(),
        };
        let mut projects = self.remote_projects()?;
        projects.insert(remote.key.clone(), remote.clone());
        self.save_setting(
            PROJECTS,
            &serde_json::to_string(&projects).map_err(|e| e.to_string())?,
        )?;
        Ok(remote)
    }
    fn stored_remote_sessions(&self) -> Result<BTreeMap<i64, RemoteSession>, String> {
        let mut sessions: BTreeMap<i64, RemoteSession> = self
            .setting(SESSIONS)?
            .map(|raw| {
                serde_json::from_str(&raw).map_err(|e| format!("Cannot read remote sessions: {e}"))
            })
            .unwrap_or_else(|| Ok(BTreeMap::new()))?;
        let status = self.remote_status.borrow();
        for (id, remote) in &mut sessions {
            if let Some((updated, running, needs_input)) = status.values.get(id) {
                remote.summary.updated_at = *updated;
                remote.running = *running;
                remote.needs_input = *needs_input;
            }
        }
        Ok(sessions)
    }
    fn remote_sessions(&self) -> Result<BTreeMap<i64, RemoteSession>, String> {
        let hidden = self.hidden_legacy_remote_sessions()?;
        let mut sessions = self.legacy_remote_sessions()?;
        sessions.retain(|id, _| !hidden.contains(id));
        for (id, mut saved) in self.stored_remote_sessions()? {
            let canonical = saved
                .session_id
                .as_ref()
                .and_then(|host| {
                    sessions.values().find(|legacy| {
                        legacy.summary.id >= 0
                            && legacy.project.environment_id == saved.project.environment_id
                            && legacy.session_id.as_ref() == Some(host)
                    })
                })
                .map(|legacy| legacy.summary.id)
                .unwrap_or(id);
            saved.summary.id = canonical;
            sessions.insert(canonical, saved);
        }
        Ok(sessions)
    }
    pub fn remote_session(&self, id: i64) -> Result<Option<RemoteSession>, String> {
        let mut stored = self.stored_remote_sessions()?;
        if let Some(remote) = stored.remove(&id) {
            if remote
                .rejected
                .as_ref()
                .is_some_and(|recovery| recovery["legacyRecovery"] == true)
            {
                // Re-check immutable source preferences on a subsequent store
                // open after the original app has completed recovery.
                if let Some(legacy) = self.legacy_remote_session(id)? {
                    return Ok(Some(legacy));
                }
            }
            return Ok(Some(remote));
        }
        if id >= 0 && !self.hidden_legacy_remote_sessions()?.contains(&id) {
            if let Some(legacy) = self.legacy_remote_session(id)? {
                if let Some(mut saved) = legacy
                    .session_id
                    .as_ref()
                    .and_then(|host| {
                        stored.values().find(|saved| {
                            saved.project.environment_id == legacy.project.environment_id
                                && saved.session_id.as_ref() == Some(host)
                        })
                    })
                    .cloned()
                {
                    saved.summary.id = id;
                    return Ok(Some(saved));
                }
                return Ok(Some(legacy));
            }
        }
        Ok(None)
    }
    pub fn save_remote_session(&self, session: &RemoteSession) -> Result<(), String> {
        self.claim_legacy_remote_commands(session)?;
        let mut sessions = self.stored_remote_sessions()?;
        if session.summary.id >= 0 && session.session_id.is_some() {
            sessions.retain(|id, existing| {
                *id == session.summary.id
                    || existing.project.environment_id != session.project.environment_id
                    || existing.session_id != session.session_id
            });
        }
        sessions.insert(session.summary.id, session.clone());
        self.commit_remote_sessions(&sessions, Vec::new())
    }
    pub fn sessions(&self) -> Result<Vec<Session>, String> {
        let hidden = self.hidden_legacy_remote_sessions()?;
        let mut sessions: BTreeMap<_, _> = self
            .local_sessions()?
            .into_iter()
            .filter(|summary| !hidden.contains(&summary.id))
            .map(|summary| (summary.id, summary))
            .collect();
        sessions.extend(
            self.remote_sessions()?
                .into_iter()
                .map(|(id, remote)| (id, remote.summary)),
        );
        let mut sessions: Vec<_> = sessions.into_values().collect();
        sessions.sort_by_key(|s| (std::cmp::Reverse(s.updated_at), std::cmp::Reverse(s.id)));
        Ok(sessions)
    }
    pub fn create(&self, provider: &str, cwd: &str) -> Result<Session, String> {
        if !cwd.starts_with("remote://") {
            return self.local_create(provider, cwd);
        }
        let kind = crate::provider::Kind::parse(provider).ok_or("Unknown remote provider")?;
        let project = self
            .remote_projects()?
            .into_values()
            .find(|project| {
                project.key.trim_end_matches('/') == cwd.trim_end_matches('/')
                    || crate::remote::remote_path(
                        &project.environment_id,
                        project.cwd.replace('\\', "/").trim_end_matches('/'),
                    )
                    .trim_end_matches('/')
                        == cwd.trim_end_matches('/')
            })
            .ok_or("Open this folder on its machine again to restore its project identity")?;
        let id = -shared::handle(&format!("remote:blank:{}", uuid::Uuid::new_v4()));
        let summary = Session {
            id,
            title: format!("New {} session", kind.label()),
            provider: provider.into(),
            cwd: project.key.clone(),
            provider_id: None,
            has_messages: false,
            updated_at: crate::time::now_ms(),
            archived: false,
            pinned: false,
            work_item: None,
        };
        self.save_remote_session(&RemoteSession {
            summary: summary.clone(),
            project,
            session_id: None,
            parent_session_id: None,
            outbox: vec![],
            uploads: vec![],
            rejected: None,
            configuration: None,
            running: false,
            needs_input: false,
        })?;
        Ok(summary)
    }
    /// Reconcile only after a complete, successful host session listing. Draft
    /// desktop handles and commands awaiting receipts survive external deletion.
    pub fn prune_remote_sessions(
        &self,
        project: &RemoteProject,
        live_host_ids: &std::collections::HashSet<String>,
    ) -> Result<(), String> {
        let mut sessions = self.remote_sessions()?;
        let mut values = Vec::new();
        self.stage_remote_prune(&mut sessions, project, live_host_ids, &mut values)?;
        self.commit_remote_sessions(&sessions, values)
    }
    fn stage_remote_prune(
        &self,
        sessions: &mut BTreeMap<i64, RemoteSession>,
        project: &RemoteProject,
        live_host_ids: &std::collections::HashSet<String>,
        values: &mut Vec<(String, String)>,
    ) -> Result<(), String> {
        let mut removed = Vec::new();
        sessions.retain(|id, remote| {
            let keep = remote.project.environment_id != project.environment_id
                || remote.project.project_id != project.project_id
                || remote
                    .session_id
                    .as_ref()
                    .is_none_or(|id| live_host_ids.contains(id))
                || !remote.outbox.is_empty()
                || !remote.uploads.is_empty();
            if !keep && *id >= 0 {
                removed.push(*id);
            }
            keep
        });
        if !removed.is_empty() {
            let mut hidden = self.hidden_legacy_remote_sessions()?;
            let before = hidden.len();
            hidden.extend(removed);
            if hidden.len() != before {
                let sorted = hidden
                    .into_iter()
                    .collect::<std::collections::BTreeSet<_>>();
                values.push((
                    "remote-hidden-legacy.v1".into(),
                    serde_json::to_string(&sorted).map_err(|e| e.to_string())?,
                ));
            }
        }
        Ok(())
    }

    pub fn register_remote_summary(
        &self,
        project: &RemoteProject,
        value: &Value,
    ) -> Result<Session, String> {
        Ok(self
            .register_remote_summaries(project, std::slice::from_ref(value))?
            .remove(0)
            .summary)
    }
    pub fn register_remote_summaries(
        &self,
        project: &RemoteProject,
        values: &[Value],
    ) -> Result<Vec<RemoteSession>, String> {
        self.update_remote_summaries(project, values, false)
    }
    /// Full project directory refresh: prune, settings and summaries share one commit.
    pub fn sync_remote_summaries(
        &self,
        project: &RemoteProject,
        values: &[Value],
    ) -> Result<Vec<RemoteSession>, String> {
        self.update_remote_summaries(project, values, true)
    }
    fn update_remote_summaries(
        &self,
        project: &RemoteProject,
        values: &[Value],
        prune: bool,
    ) -> Result<Vec<RemoteSession>, String> {
        let mut sessions = self.remote_sessions()?;
        let mut ids = HashMap::new();
        // Preserve the former BTreeMap::values().find() handle when two local
        // tabs refer to the same host session.
        for remote in sessions.values() {
            if let Some(id) = &remote.session_id {
                ids.entry((remote.project.environment_id.clone(), id.clone()))
                    .or_insert(remote.summary.id);
            }
        }
        let mut settings_to_commit = Vec::new();
        let mut result = Vec::with_capacity(values.len());
        for value in values {
            let remote = self.upsert_remote_summary(
                project,
                value,
                &mut sessions,
                &mut ids,
                &mut settings_to_commit,
            )?;
            sessions.insert(remote.summary.id, remote.clone());
            result.push(remote);
        }
        if prune {
            let live = result.iter().filter_map(|s| s.session_id.clone()).collect();
            self.stage_remote_prune(&mut sessions, project, &live, &mut settings_to_commit)?;
        }
        self.commit_remote_sessions(&sessions, settings_to_commit)?;
        Ok(result)
    }
    fn upsert_remote_summary(
        &self,
        project: &RemoteProject,
        value: &Value,
        sessions: &mut BTreeMap<i64, RemoteSession>,
        ids: &mut HashMap<(String, String), i64>,
        settings_to_commit: &mut Vec<(String, String)>,
    ) -> Result<RemoteSession, String> {
        let host = value.get("session").unwrap_or(value);
        let host_id = host["id"]
            .as_str()
            .ok_or("Host session identity is missing")?;
        let identity = (project.environment_id.clone(), host_id.to_owned());
        let id = *ids.entry(identity).or_insert_with(|| {
            -shared::handle(&format!("remote:{}:{host_id}", project.environment_id))
        });
        let prior = sessions.remove(&id);
        let summary = Session {
            id,
            title: host["title"].as_str().unwrap_or("Remote session").into(),
            provider: host["harness"].as_str().unwrap_or("codex").into(),
            cwd: project.key.clone(),
            provider_id: host.get("providerSessionId").map_or_else(
                || prior.as_ref().and_then(|r| r.summary.provider_id.clone()),
                |id| id.as_str().map(str::to_owned),
            ),
            has_messages: host
                .get("blocks")
                .and_then(Value::as_array)
                .map(|b| !b.is_empty())
                .or_else(|| host["hasMessages"].as_bool())
                .unwrap_or_else(|| prior.as_ref().is_none_or(|r| r.summary.has_messages)),
            updated_at: value["updatedAt"].as_u64().unwrap_or(0),
            archived: value["archived"].as_bool().unwrap_or(false),
            pinned: value["pinned"].as_bool().unwrap_or(false),
            work_item: host
                .get("linkedWorkItem")
                .filter(|v| !v.is_null())
                .and_then(|v| serde_json::from_value(v.clone()).ok()),
        };
        let imported = host_settings(project, host);
        let full_snapshot = value.get("session").is_some_and(Value::is_object);
        let configuration = if full_snapshot {
            Some(crate::remote::protocol::settings_configuration(
                &summary.provider,
                &imported,
            ))
        } else {
            prior
                .as_ref()
                .and_then(|remote| remote.configuration.clone())
        };
        let existing = self.agent_settings(id)?;
        let mut settings_to_save = None;
        match existing {
            // A directory row is not a conversation configuration. The canonical
            // snapshot (or an explicit draft edit) supplies settings when opened.
            None if full_snapshot || !self.daemon_status(project) => {
                settings_to_save = Some(imported)
            }
            Some(mut settings) if full_snapshot => {
                let pending = prior
                    .as_ref()
                    .is_some_and(|remote| !remote.outbox.is_empty() || !remote.uploads.is_empty());
                let local =
                    crate::remote::protocol::settings_configuration(&summary.provider, &settings);
                let unchanged = prior
                    .as_ref()
                    .and_then(|remote| remote.configuration.as_ref())
                    .map_or(settings.revision == 0, |baseline| baseline == &local);
                if !pending
                    && unchanged
                    && (configuration.as_ref() != Some(&local)
                        || settings.worktree_cwd != imported.worktree_cwd)
                {
                    adopt_host_configuration(&mut settings, imported);
                    settings_to_save = Some(settings);
                }
            }
            _ => {}
        }
        if let Some(settings) = settings_to_save {
            settings_to_commit.push((format!("remote-agent:{id}"), encode_settings(&settings)?));
        }
        let remote = RemoteSession {
            summary: summary.clone(),
            project: project.clone(),
            session_id: Some(host_id.into()),
            parent_session_id: value.get("paseoParentAgentId")
                .map(|parent| parent.as_str().filter(|id| !id.is_empty()).map(str::to_owned))
                .unwrap_or_else(|| prior.as_ref().and_then(|r| r.parent_session_id.clone())),
            outbox: prior.as_ref().map(|r| r.outbox.clone()).unwrap_or_default(),
            uploads: prior
                .as_ref()
                .map(|r| r.uploads.clone())
                .unwrap_or_default(),
            rejected: prior.as_ref().and_then(|remote| remote.rejected.clone()),
            configuration,
            running: value["status"] == "running",
            needs_input: host["needsInput"] == true
                || host["pendingQuestion"].is_object()
                || host["blocks"].as_array().is_some_and(|blocks| {
                    blocks.iter().any(|b| {
                        b["approval"].is_object()
                            && b["approval"].get("decided").is_none_or(Value::is_null)
                    })
                }),
        };
        Ok(remote)
    }
    pub fn project_remote_history(
        snapshot: &Value,
    ) -> Result<(Transcript, HashMap<usize, TurnInfo>), String> {
        let blocks = snapshot["session"]["blocks"]
            .as_array()
            .ok_or("Host transcript is missing")?;
        let blocks: Vec<_> = blocks
            .iter()
            .cloned()
            .map(|mut block| {
                if block["role"] == "plan" {
                    block["nativeRemotePlan"] = Value::Bool(true);
                }
                block
            })
            .collect();
        Ok((
            Transcript {
                rows: shared::blocks::project(&blocks)?,
            },
            shared::project_turns(&blocks)?,
        ))
    }
    pub fn patch_remote_session(&self, id: i64, patch: Value) -> Result<(), String> {
        let mut remote = self
            .remote_session(id)?
            .ok_or("Remote session no longer exists")?;
        if let Some(session_id) = &remote.session_id {
            let client = crate::remote::default_connections()?;
            let machine = remote.machine(&client)?;
            let mut params = patch.clone();
            params["sessionId"] = json!(session_id);
            params["projectId"] = json!(remote.project.project_id);
            client.request(&machine.id, "sessions.update", params)?;
        }
        if let Some(title) = patch["title"].as_str() {
            remote.summary.title = title.into();
        }
        if let Some(archived) = patch["archived"].as_bool() {
            remote.summary.archived = archived;
        }
        if let Some(pinned) = patch["pinned"].as_bool() {
            remote.summary.pinned = pinned;
        }
        if let Some(item) = patch.get("linkedWorkItem") {
            remote.summary.work_item = serde_json::from_value(item.clone()).ok();
        }
        self.save_remote_session(&remote)
    }
    pub fn delete_remote_session(&self, id: i64) -> Result<(), String> {
        let remote = self
            .remote_session(id)?
            .ok_or("Remote session no longer exists")?;
        if let Some(session_id) = &remote.session_id {
            let client = crate::remote::default_connections()?;
            let machine = remote.machine(&client)?;
            client.request(
                &machine.id,
                "sessions.delete",
                json!({"sessionId": session_id,"projectId": remote.project.project_id}),
            )?;
        }
        let mut sessions = self.remote_sessions()?;
        sessions.remove(&id);
        self.hide_legacy_remote_sessions([id])?;
        self.commit_remote_sessions(&sessions, Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn project(store: &Store, environment: &str) -> RemoteProject {
        store
            .remember_remote_project(environment, &json!({"id":"project","cwd":"/host/repo"}))
            .unwrap()
    }
    fn snapshot(id: &str) -> Value {
        json!({"projectId":"project","revision":4,"status":"idle","updatedAt":20,
            "session":{"id":id,"harness":"codex","title":"Host conversation","model":"codex:live-model",
                "runtimeMode":"auto","modelSettings":{"reasoningEffort":"high"},"cwd":"/host/repo/worktree",
                "blocks":[{"id":"prompt","role":"user","text":"hello"}]}})
    }

    #[test]
    fn remote_parent_links_survive_restart_and_follow_authoritative_refresh() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("native.db");
        let store = Store::open(&path).unwrap();
        let project = project(&store, "env");
        let mut child = snapshot("child");
        child["paseoParentAgentId"] = json!("parent");
        let parent = snapshot("parent");
        let registered = store.sync_remote_summaries(&project, &[child.clone(), parent]).unwrap();
        let (child_id, parent_id) = (registered[0].summary.id, registered[1].summary.id);
        let links = store.session_sidebar_metadata(&[project.key.clone()]).started_by;
        assert_eq!(links.get(&child_id), Some(&parent_id));
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.session_sidebar_metadata(&[project.key.clone()]).started_by.get(&child_id), Some(&parent_id));
        child["paseoParentAgentId"] = Value::Null;
        store.register_remote_summary(&project, &child).unwrap();
        assert!(!store.session_sidebar_metadata(&[project.key]).started_by.contains_key(&child_id));
    }

    #[test]
    fn remote_parent_links_never_cross_environments_or_accept_cycles() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let one = project(&store, "one");
        let two = project(&store, "two");
        let mut child = snapshot("child");
        child["paseoParentAgentId"] = json!("parent");
        let child_id = store.register_remote_summary(&one, &child).unwrap().id;
        store.register_remote_summary(&two, &snapshot("parent")).unwrap();
        assert!(!store.session_sidebar_metadata(&[one.key.clone()]).started_by.contains_key(&child_id));
        let parent_id = store.register_remote_summary(&one, &snapshot("parent")).unwrap().id;
        assert_eq!(store.session_sidebar_metadata(&[one.key.clone()]).started_by.get(&child_id), Some(&parent_id));
        let mut parent = snapshot("parent");
        parent["paseoParentAgentId"] = json!("child");
        store.register_remote_summary(&one, &parent).unwrap();
        let links = store.session_sidebar_metadata(&[one.key]).started_by;
        assert!(!links.contains_key(&child_id));
        assert!(!links.contains_key(&parent_id));
    }

    #[test]
    fn remote_metadata_and_original_outbox_survive_restart_without_local_sessions_or_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("native.db");
        let store = Store::open(&path).unwrap();
        let project = project(&store, "env");
        let summary = store.create("codex", &project.key).unwrap();
        assert!(summary.id < 0);
        let mut binding = store.remote_session(summary.id).unwrap().unwrap();
        binding.outbox = vec![
            json!({"type":"create","commandId":"original-create"}),
            json!({"type":"send","commandId":"original-send","text":"keep me"}),
        ];
        binding.uploads = vec![RemoteUpload {
            command_id: "original-send".into(),
            id: uuid::Uuid::new_v4().to_string(),
            path: dir.path().join("photo.png"),
            size: 3,
        }];
        store.save_remote_session(&binding).unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        let restored = store.remote_session(summary.id).unwrap().unwrap();
        assert_eq!(restored.outbox, binding.outbox);
        assert_eq!(restored.uploads[0].id, binding.uploads[0].id);
        assert_eq!(store.sessions().unwrap()[0].id, summary.id);
        for table in ["native_sessions", "native_rows"] {
            let count: i64 = store
                .conn
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0);
        }
    }

    #[test]
    fn host_refresh_preserves_selected_handle_pending_commands_and_unsent_settings() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let project = project(&store, "env");
        let summary = store.create("codex", &project.key).unwrap();
        let mut binding = store.remote_session(summary.id).unwrap().unwrap();
        binding.session_id = Some("host-session".into());
        binding
            .outbox
            .push(json!({"type":"send","commandId":"pending","text":"unsent"}));
        store.save_remote_session(&binding).unwrap();
        let settings = Settings {
            model: Some("codex:unsent-choice".into()),
            revision: 7,
            ..Default::default()
        };
        store.save_agent_settings(summary.id, &settings).unwrap();
        let refreshed = store
            .register_remote_summary(&project, &snapshot("host-session"))
            .unwrap();
        assert_eq!(refreshed.id, summary.id);
        assert_eq!(
            store.remote_session(summary.id).unwrap().unwrap().outbox,
            binding.outbox
        );
        assert_eq!(
            store.agent_settings(summary.id).unwrap().unwrap().model,
            settings.model
        );
        assert_eq!(store.sessions().unwrap().len(), 1);
    }

    #[test]
    fn imported_host_handles_are_scoped_to_environment_and_use_remote_worktree_paths() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let one = project(&store, "one");
        let two = project(&store, "two");
        let first = store
            .register_remote_summary(&one, &snapshot("same-host-id"))
            .unwrap();
        let second = store
            .register_remote_summary(&two, &snapshot("same-host-id"))
            .unwrap();
        assert_ne!(first.id, second.id);
        let settings = store.agent_settings(first.id).unwrap().unwrap();
        assert_eq!(
            settings.worktree_cwd.as_deref(),
            Some("remote://one/host/repo/worktree")
        );
        assert_eq!(settings.access, crate::agent::AccessMode::Auto);
        assert_eq!(settings.model_settings["reasoningEffort"], "high");
    }

    #[test]
    fn resolved_approval_strings_do_not_keep_needs_input_set() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let project = project(&store, "env");
        for decision in [
            Value::Null,
            json!("allow"),
            json!("deny"),
            json!("cancelled"),
        ] {
            let mut value = snapshot("session");
            value["session"]["blocks"] =
                json!([{"id":"tool","role":"tool","approval":{"requestId":3,"decided":decision}}]);
            let summary = store.register_remote_summary(&project, &value).unwrap();
            assert_eq!(
                store
                    .remote_session(summary.id)
                    .unwrap()
                    .unwrap()
                    .needs_input,
                decision.is_null()
            );
        }
    }

    #[test]
    fn pruning_external_deletions_keeps_pending_and_unbound_sessions() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let project = project(&store, "env");
        let blank = store.create("codex", &project.key).unwrap();
        let gone = store
            .register_remote_summary(&project, &snapshot("gone"))
            .unwrap();
        let pending = store
            .register_remote_summary(&project, &snapshot("pending"))
            .unwrap();
        let mut binding = store.remote_session(pending.id).unwrap().unwrap();
        binding.outbox.push(json!({"commandId":"keep-original"}));
        store.save_remote_session(&binding).unwrap();
        let present = store
            .register_remote_summary(&project, &snapshot("present"))
            .unwrap();
        let other = self::project(&store, "other-env");
        let elsewhere = store
            .register_remote_summary(&other, &snapshot("gone"))
            .unwrap();
        store
            .prune_remote_sessions(
                &project,
                &std::collections::HashSet::from(["present".into()]),
            )
            .unwrap();
        assert!(store.remote_session(gone.id).unwrap().is_none());
        for id in [blank.id, pending.id, present.id, elsewhere.id] {
            assert!(store.remote_session(id).unwrap().is_some());
        }
    }

    #[test]
    fn windows_project_keys_normalize_trailing_separators_and_find_legacy_unc_keys() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let project = store
            .remember_remote_project("env", &json!({"id":"project","cwd":r"C:\repo\"}))
            .unwrap();
        assert_eq!(project.key, "remote://env/C:/repo");
        let legacy = RemoteProject {
            key: "remote://env/server/share/repo".into(),
            environment_id: "env".into(),
            project_id: "unc".into(),
            cwd: r"\\server\share\repo".into(),
        };
        store
            .save_setting(
                PROJECTS,
                &serde_json::to_string(&BTreeMap::from([(legacy.key.clone(), legacy)])).unwrap(),
            )
            .unwrap();
        assert!(store
            .create("codex", "remote://env//server/share/repo")
            .is_ok());
    }
}

#[cfg(test)]
mod configuration_tests {
    use super::*;
    use std::path::Path;
    fn host(model: &str, settings: Value) -> Value {
        json!({"projectId":"project","revision":3,"status":"idle","updatedAt":1,"session":{
            "id":"host-session","harness":"claude","title":"Host","model":model,"runtimeMode":"auto",
            "modelSettings":settings,"cwd":"/host/repo","blocks":[]}})
    }
    fn project(store: &Store) -> RemoteProject {
        store
            .remember_remote_project("env", &json!({"id":"project","cwd":"/host/repo"}))
            .unwrap()
    }
    #[test]
    fn imported_common_controls_match_host_instead_of_overwriting_fast_and_effort() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let project = project(&store);
        let value = host(
            "claude:opus",
            json!({"fast":"true","effort":"max","thinking":"true","context":"1m"}),
        );
        let summary = store.register_remote_summary(&project, &value).unwrap();
        let settings = store.agent_settings(summary.id).unwrap().unwrap();
        assert!(settings.fast);
        assert_eq!(settings.effort.as_deref(), Some("max"));
        assert_eq!(settings.thinking, Some(true));
        let wire = crate::remote::protocol::wire_model_settings("claude", &settings);
        assert_eq!(wire["fast"], "true");
        assert_eq!(wire["effort"], "max");
        assert_eq!(wire["context"], "1m");
    }
    #[test]
    fn external_host_configuration_updates_unmodified_native_settings_and_revision() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let project = project(&store);
        let first = host("claude:sonnet", json!({}));
        let summary = store.register_remote_summary(&project, &first).unwrap();
        let revision = store.agent_settings(summary.id).unwrap().unwrap().revision;
        let mut next = host("claude:opus", json!({"fast":"true","effort":"high"}));
        next["session"]["runtimeMode"] = json!("full-access");
        store.register_remote_summary(&project, &next).unwrap();
        let settings = store.agent_settings(summary.id).unwrap().unwrap();
        assert_eq!(settings.model.as_deref(), Some("claude:opus"));
        assert!(settings.fast);
        assert_eq!(settings.access, crate::agent::AccessMode::FullAccess);
        assert!(settings.revision > revision);
        // Summary listings omit modelSettings and must not erase the baseline.
        let before = store
            .remote_session(summary.id)
            .unwrap()
            .unwrap()
            .configuration;
        store.register_remote_summary(&project,&json!({"id":"host-session","harness":"claude","title":"Listed","model":"claude:opus","runtimeMode":"full-access","updatedAt":10})).unwrap();
        assert_eq!(
            store
                .remote_session(summary.id)
                .unwrap()
                .unwrap()
                .configuration,
            before
        );
        assert!(store.agent_settings(summary.id).unwrap().unwrap().fast);
    }
    #[test]
    fn unsent_override_survives_host_changes_then_external_updates_resume_after_acceptance() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let project = project(&store);
        let summary = store
            .register_remote_summary(&project, &host("claude:sonnet", json!({})))
            .unwrap();
        let mut local = store.agent_settings(summary.id).unwrap().unwrap();
        local.model = Some("claude:local-choice".into());
        local.revision += 1;
        store.save_agent_settings(summary.id, &local).unwrap();
        store
            .register_remote_summary(
                &project,
                &host("claude:external-choice", json!({"effort":"high"})),
            )
            .unwrap();
        assert_eq!(
            store.agent_settings(summary.id).unwrap().unwrap().model,
            local.model
        );
        let accepted = crate::remote::protocol::wire_model_settings("claude", &local);
        store
            .register_remote_summary(&project, &host("claude:local-choice", json!(accepted)))
            .unwrap();
        store
            .register_remote_summary(
                &project,
                &host("claude:next-external", json!({"effort":"max"})),
            )
            .unwrap();
        assert_eq!(
            store
                .agent_settings(summary.id)
                .unwrap()
                .unwrap()
                .model
                .as_deref(),
            Some("claude:next-external")
        );
    }
}

#[cfg(test)]
mod refresh_write_tests {
    use super::*;
    use std::path::Path;
    #[test]
    fn metadata_canonicalization_preserves_pending_and_rejected_command_wire_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let store = Store::open_monocode(&path).unwrap();
        store.ephemeral_remote_status("fixture");
        let p = store
            .remember_remote_project("fixture", &json!({"id":"p","cwd":"/fixture"}))
            .unwrap();
        let snapshot = json!({"session":{"id":"a","harness":"hermes","modelSettings":{"z":"last","a":"first"},"blocks":[]}});
        let summary = store.register_remote_summary(&p, &snapshot).unwrap();
        let mut binding = store.remote_session(summary.id).unwrap().unwrap();
        let command = json!({"type":"send","commandId":"pending","sessionId":"a","text":"Synthetic","modelSettings":{"z":"last","a":"first"}});
        let wire = serde_json::to_string(&command).unwrap();
        binding.outbox.push(command.clone());
        binding.rejected = Some(json!({"commands":[command]}));
        store.save_remote_session(&binding).unwrap();
        store.register_remote_summary(&p, &snapshot).unwrap();
        drop(store);
        let store = Store::open_monocode(&path).unwrap();
        let restored = store.remote_session(summary.id).unwrap().unwrap();
        assert_eq!(
            serde_json::to_string(&restored.outbox[0]).unwrap(),
            wire,
            "Host attempt hashes include original wire order"
        );
        assert_eq!(
            serde_json::to_string(&restored.rejected.unwrap()["commands"][0]).unwrap(),
            wire
        );
    }
    #[test]
    fn repeated_configuration_snapshot_is_byte_stable_with_production_json_features() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_monocode(&dir.path().join("db")).unwrap();
        store.ephemeral_remote_status("fixture");
        let project = store
            .remember_remote_project("fixture", &json!({"id":"p","cwd":"/fixture"}))
            .unwrap();
        let snapshot = json!({"updatedAt":1000,"status":"idle","session":{"id":"a","harness":"hermes","title":"Synthetic","model":"hermes:exact","modelSettings":{"effort":"high","fast":"false","thinking":"true","custom":"value"},"blocks":[]}});
        store.register_remote_summary(&project, &snapshot).unwrap();
        let before = store.sidecar_write_metrics();
        let expected = store.setting(SESSIONS).unwrap().unwrap();
        let expected_value: Value = serde_json::from_str(&expected).unwrap();
        for _ in 0..128 {
            store.register_remote_summary(&project, &snapshot).unwrap();
            store
                .sync_remote_summaries(&project, std::slice::from_ref(&snapshot))
                .unwrap();
            let id = store.sessions().unwrap()[0].id;
            let settings = store.agent_settings(id).unwrap().unwrap();
            store.save_agent_settings(id, &settings).unwrap();
            let encoded = store.setting(SESSIONS).unwrap().unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&encoded).unwrap(),
                expected_value,
                "no semantic change"
            );
        }
        let after = store.sidecar_write_metrics();
        eprintln!(
            "r10c identical snapshots=128 writes={} bytes={}",
            after.0 - before.0,
            after.1 - before.1
        );
        assert_eq!(
            after, before,
            "randomized HashMap seeds caused semantically identical writes"
        );
        assert_eq!(store.setting(SESSIONS).unwrap().unwrap(), expected);
        let id = store.sessions().unwrap()[0].id;
        let expected_settings = store.setting(&format!("remote-agent:{id}")).unwrap();
        for run in 0..16 {
            let other = Store::open_monocode(&dir.path().join(format!("run-{run}"))).unwrap();
            other.ephemeral_remote_status("fixture");
            other
                .remember_remote_project("fixture", &json!({"id":"p","cwd":"/fixture"}))
                .unwrap();
            other.register_remote_summary(&project, &snapshot).unwrap();
            assert_eq!(
                other.setting(SESSIONS).unwrap().unwrap(),
                expected,
                "fresh HashMap seeds must encode identically"
            );
            assert_eq!(
                other.setting(&format!("remote-agent:{id}")).unwrap(),
                expected_settings
            );
        }
    }
    #[test]
    fn volatile_directory_ticks_update_ui_without_persisting_and_structural_change_commits_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_monocode(&dir.path().join("db")).unwrap();
        store.ephemeral_remote_status("fixture");
        let p = store
            .remember_remote_project("fixture", &json!({"id":"p","cwd":"/fixture"}))
            .unwrap();
        let mut values = (0..758).map(|i|json!({"id":format!("a-{i}"),"harness":"hermes","title":format!("Synthetic {i}"),"updatedAt":1000,"status":"idle"})).collect::<Vec<_>>();
        let rows = store.sync_remote_summaries(&p, &values).unwrap();
        let before = store.sidecar_write_metrics();
        for tick in 1..=10 {
            for v in &mut values[..2] {
                v["updatedAt"] = json!(1000 + tick);
                v["status"] = json!(if tick % 2 == 0 { "running" } else { "idle" });
                v["needsInput"] = json!(tick % 2 != 0);
                v["paseoBucket"] = json!(if tick % 2 == 0 {
                    "running"
                } else {
                    "attention"
                });
                v["paseoStatusEnteredAt"] = json!(1000 + tick);
            }
            store.sync_remote_summaries(&p, &values).unwrap();
            for row in &rows[..2] {
                let live = store.remote_session(row.summary.id).unwrap().unwrap();
                assert_eq!(live.summary.updated_at, 1000 + tick);
                assert_eq!(live.running, tick % 2 == 0);
                assert_eq!(live.needs_input, tick % 2 != 0);
            }
        }
        let after = store.sidecar_write_metrics();
        eprintln!(
            "r10b volatile_ticks=10 sessions=758 writes={} bytes={}",
            after.0 - before.0,
            after.1 - before.1
        );
        assert_eq!(after, before, "volatile directory fields rewrote extras");
        // The live worker journals metadata through the same API as durable
        // sends; volatility must not bypass or erase a real outbox transition.
        let mut live = store.remote_session(rows[0].summary.id).unwrap().unwrap();
        for tick in 11..=20 {
            live.summary.updated_at = 1000 + tick;
            live.running = tick % 2 == 0;
            store.save_remote_session(&live).unwrap();
        }
        assert_eq!(store.sidecar_write_metrics(), after);
        live.outbox
            .push(json!({"type":"send","commandId":"unknown-ack","text":"Synthetic"}));
        store.save_remote_session(&live).unwrap();
        let journaled = store.sidecar_write_metrics();
        assert_eq!(journaled.0, after.0 + 1);
        values[0]["title"] = json!("Structural rename");
        store.sync_remote_summaries(&p, &values).unwrap();
        assert_eq!(store.sidecar_write_metrics().0, journaled.0 + 1);
        drop(store);
        let store = Store::open_monocode(&dir.path().join("db")).unwrap();
        let restored = store.remote_session(rows[0].summary.id).unwrap().unwrap();
        assert_eq!(restored.summary.title, "Structural rename");
        assert_eq!(restored.outbox, live.outbox);
        assert_eq!(restored.summary.updated_at, 0);
    }
    #[test]
    fn daemon_cache_compaction_preserves_drafts_commands_explicit_settings_and_reload_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let store = Store::open_monocode(&path).unwrap();
        let visible = store
            .remember_remote_project("fixture", &json!({"id":"p","cwd":"/visible"}))
            .unwrap();
        let hidden = store
            .remember_remote_project("fixture", &json!({"id":"h","cwd":"/hidden"}))
            .unwrap();
        let a = store
            .register_remote_summary(&visible, &json!({"id":"a","model":"hermes:exact"}))
            .unwrap();
        let b = store
            .register_remote_summary(&hidden, &json!({"id":"b","model":"hermes:exact"}))
            .unwrap();
        let c = store
            .register_remote_summary(&hidden, &json!({"id":"c","model":"hermes:exact"}))
            .unwrap();
        let d = store
            .register_remote_summary(&hidden, &json!({"id":"d","model":"hermes:exact"}))
            .unwrap();
        let draft = store.create("hermes", &hidden.key).unwrap();
        let mut pending = store.remote_session(c.id).unwrap().unwrap();
        pending.outbox = vec![json!({"type":"send","commandId":"pending","text":"Synthetic"})];
        store.save_remote_session(&pending).unwrap();
        let mut settings = store.agent_settings(d.id).unwrap().unwrap();
        settings.revision = 1;
        store.save_agent_settings(d.id, &settings).unwrap();
        store.ephemeral_remote_status("fixture");
        let before = store.sidecar_write_metrics();
        let keys = std::collections::HashSet::from([visible.key.clone()]);
        store.compact_remote_cache("fixture", &keys).unwrap();
        assert_eq!(store.sidecar_write_metrics().0, before.0 + 1);
        assert!(store.remote_session(b.id).unwrap().is_none());
        assert!(store
            .setting(&format!("remote-agent:{}", a.id))
            .unwrap()
            .is_none());
        assert!(store
            .setting(&format!("remote-agent:{}", b.id))
            .unwrap()
            .is_none());
        assert_eq!(
            store.remote_session(c.id).unwrap().unwrap().outbox,
            pending.outbox
        );
        assert!(store.remote_session(d.id).unwrap().is_some());
        assert!(store.remote_session(draft.id).unwrap().is_some());
        let after = store.sidecar_write_metrics();
        store.compact_remote_cache("fixture", &keys).unwrap();
        assert_eq!(store.sidecar_write_metrics(), after);
        let restored = store
            .register_remote_summary(
                &hidden,
                &json!({"id":"b","model":"hermes:exact","updatedAt":123}),
            )
            .unwrap();
        assert_eq!(
            restored.id, b.id,
            "Archived reload must keep the same local handle"
        );
        assert_eq!(restored.updated_at, 123);
        assert!(
            store.agent_settings(b.id).unwrap().is_none(),
            "list must not recreate settings"
        );
        let snapshot =
            json!({"updatedAt":124,"session":{"id":"b","model":"hermes:exact","blocks":[]}});
        store.register_remote_summary(&hidden, &snapshot).unwrap();
        assert_eq!(
            store
                .agent_settings(b.id)
                .unwrap()
                .unwrap()
                .model
                .as_deref(),
            Some("hermes:exact")
        );
        assert!(store.remote_projects().unwrap().contains_key(&hidden.key));
    }
    #[test]
    fn bulk_refresh_one_transaction_and_unchanged_snapshot_zero_writes_matches_single_path() {
        let directory = tempfile::tempdir().unwrap();
        let batch = Store::open_monocode(&directory.path().join("batch.db")).unwrap();
        let serial = Store::open_monocode(Path::new(":memory:")).unwrap();
        let project = batch
            .remember_remote_project("fixture", &json!({"id":"p","cwd":"/fixture"}))
            .unwrap();
        serial
            .remember_remote_project("fixture", &json!({"id":"p","cwd":"/fixture"}))
            .unwrap();
        let values = (0..64).map(|i| json!({"id":format!("a-{i}"),"harness":"hermes","title":format!("Synthetic {i}"),"model":"hermes:exact","updatedAt":1000+i,"status":"idle"})).collect::<Vec<_>>();
        let before = batch.sidecar_write_metrics();
        batch.register_remote_summaries(&project, &values).unwrap();
        let after = batch.sidecar_write_metrics();
        eprintln!(
            "r10 first refresh writes={} payload_bytes={}",
            after.0 - before.0,
            after.1 - before.1
        );
        assert!(directory.path().join("batch.db.gpui.sqlite3").exists());
        assert!(!directory.path().join("batch.db.gpui.json").exists());
        assert!(
            after.0 - before.0 <= 1,
            "bulk refresh committed {} settings transactions",
            after.0 - before.0
        );
        for v in &values {
            serial.register_remote_summary(&project, v).unwrap();
        }
        assert_eq!(
            batch.setting(SESSIONS).unwrap(),
            serial.setting(SESSIONS).unwrap()
        );
        for s in batch.sessions().unwrap() {
            assert_eq!(
                serde_json::to_value(batch.agent_settings(s.id).unwrap()).unwrap(),
                serde_json::to_value(serial.agent_settings(s.id).unwrap()).unwrap()
            );
        }
        batch.register_remote_summaries(&project, &values).unwrap();
        assert_eq!(batch.sidecar_write_metrics(), after);
        let snapshot = json!({"updatedAt":1000,"status":"idle","session":{"id":"a-0","harness":"hermes","title":"Synthetic 0","model":"hermes:exact","modelSettings":{"effort":"high"},"blocks":[]}});
        batch.register_remote_summary(&project, &snapshot).unwrap();
        let warm = batch.sidecar_write_metrics();
        for _ in 0..10 {
            batch.register_remote_summary(&project, &snapshot).unwrap();
        }
        assert_eq!(
            batch.sidecar_write_metrics(),
            warm,
            "unchanged active-agent poll rewrote extras"
        );
    }
    #[test]
    fn directory_and_snapshot_pollers_do_not_oscillate_missing_metadata_or_rewrite_outbox() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_monocode(&dir.path().join("db")).unwrap();
        let p = store
            .remember_remote_project("fixture", &json!({"id":"p","cwd":"/fixture"}))
            .unwrap();
        let list = json!({"id":"a","harness":"hermes","title":"Empty","model":"hermes:exact","updatedAt":1,"providerSessionId":"native-id"});
        let mut snapshot = json!({"updatedAt":1,"status":"idle","session":{"id":"a","harness":"hermes","title":"Empty","model":"hermes:exact","blocks":[]}});
        let summary = store.register_remote_summary(&p, &list).unwrap();
        store.register_remote_summary(&p, &snapshot).unwrap();
        let mut pending = store.remote_session(summary.id).unwrap().unwrap();
        pending
            .outbox
            .push(json!({"commandId":"unknown-send","type":"send","text":"Synthetic"}));
        pending.uploads.push(RemoteUpload {
            command_id: "unknown-send".into(),
            id: "upload".into(),
            path: "/fixture/attachment".into(),
            size: 10,
        });
        store.save_remote_session(&pending).unwrap();
        let before = store.sidecar_write_metrics();
        for _ in 0..10 {
            store
                .sync_remote_summaries(&p, std::slice::from_ref(&list))
                .unwrap();
            store.register_remote_summary(&p, &snapshot).unwrap();
            store.save_remote_session(&pending).unwrap();
        }
        assert_eq!(store.sidecar_write_metrics(), before);
        let latest = store.remote_session(summary.id).unwrap().unwrap();
        assert_eq!(latest.outbox, pending.outbox);
        assert_eq!(latest.uploads.len(), 1);
        assert!(!latest.summary.has_messages);
        assert_eq!(latest.summary.provider_id.as_deref(), Some("native-id"));
        snapshot["session"]["providerSessionId"] = Value::Null;
        store.register_remote_summary(&p, &snapshot).unwrap();
        assert!(store
            .remote_session(summary.id)
            .unwrap()
            .unwrap()
            .summary
            .provider_id
            .is_none());
        // Deletion must preserve the same durable outbox/upload identity.
        store.sync_remote_summaries(&p, &[]).unwrap();
        assert_eq!(
            store.remote_session(summary.id).unwrap().unwrap().outbox,
            pending.outbox
        );
    }
    #[test]
    fn steady_777_session_refreshes_and_agent_polls_write_zero_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open_monocode(&directory.path().join("db")).unwrap();
        let p = store
            .remember_remote_project("fixture", &json!({"id":"p","cwd":"/fixture"}))
            .unwrap();
        let values=(0..777).map(|i|json!({"id":format!("a-{i}"),"harness":"hermes","title":format!("Synthetic conversation {i}"),"updatedAt":1000+i,"model":"hermes:route:exact","providerSessionId":format!("native-{i}")})).collect::<Vec<_>>();
        let start = store.sidecar_write_metrics();
        let rows = store.sync_remote_summaries(&p, &values).unwrap();
        let warm = store.sidecar_write_metrics();
        assert_eq!(warm.0 - start.0, 1);
        let mut snapshot = json!({"updatedAt":1000,"status":"idle","session":values[0].clone()});
        snapshot["session"]["blocks"] = json!([]);
        store.register_remote_summary(&p, &snapshot).unwrap();
        let before = store.sidecar_write_metrics();
        let mut max_ms = 0f64;
        for _ in 0..10 {
            let started = std::time::Instant::now();
            store.sync_remote_summaries(&p, &values).unwrap();
            max_ms = max_ms.max(started.elapsed().as_secs_f64() * 1000.);
            store.register_remote_summary(&p, &snapshot).unwrap();
        }
        let after = store.sidecar_write_metrics();
        assert_eq!(after, before);
        assert_eq!(rows.len(), 777);
        eprintln!("r10 777 sessions: warm_writes={} warm_payload={} stable_ticks=10 stable_writes={} stable_bytes={} max_batch_ms={max_ms:.3}",warm.0-start.0,warm.1-start.1,after.0-before.0,after.1-before.1);
    }
    #[test]
    fn failed_batch_commit_keeps_both_summaries_and_settings_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("db");
        let store = Store::open_monocode(&path).unwrap();
        let p = store
            .remember_remote_project("fixture", &json!({"id":"p","cwd":"/fixture"}))
            .unwrap();
        let before = store.setting(SESSIONS).unwrap();
        let metrics = store.sidecar_write_metrics();
        let settings =
            rusqlite::Connection::open(directory.path().join("db.gpui.sqlite3")).unwrap();
        settings.execute_batch("CREATE TRIGGER reject_batch BEFORE INSERT ON settings BEGIN SELECT RAISE(ABORT, 'synthetic write failure'); END;").unwrap();
        let result = store.register_remote_summaries(
            &p,
            &[json!({"id":"new","harness":"hermes","model":"hermes:exact"})],
        );
        assert!(result.is_err());
        assert_eq!(store.setting(SESSIONS).unwrap(), before);
        assert_eq!(store.sidecar_write_metrics(), metrics);
        assert!(store.sessions().unwrap().is_empty());
    }
    #[test]
    fn batch_keeps_the_same_existing_handle_as_the_per_item_lookup() {
        let store = Store::open_monocode(Path::new(":memory:")).unwrap();
        let p = store
            .remember_remote_project("fixture", &json!({"id":"p","cwd":"/fixture"}))
            .unwrap();
        let value = json!({"id":"a","harness":"hermes","model":"hermes:exact"});
        let original = store.register_remote_summary(&p, &value).unwrap();
        let mut alias = store.remote_session(original.id).unwrap().unwrap();
        alias.summary.id = original.id - 1;
        store.save_remote_session(&alias).unwrap();
        let result = store.register_remote_summaries(&p, &[value]).unwrap();
        assert_eq!(result[0].summary.id, alias.summary.id);
    }
}
