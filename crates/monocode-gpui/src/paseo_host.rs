//! Bundle-only bootstrap for the original Shell. Never selects the official Store.
use monocode_core::{remote::Connections, store::Store};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

pub(crate) fn active() -> bool {
    crate::paseo_prototype::is_bundle()
}

/// Personal defaults are runtime data; never bundled into a public build.
pub(crate) fn prototype_config() -> Value {
    let path = std::env::var_os("PASEO_PROTOTYPE_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::paseo_prototype::data_dir().join("prototype.json"));
    std::fs::read(path)
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or_else(|| json!({}))
}

pub(crate) fn terminal_path(
    enabled: bool,
    expected: &str,
    project: &str,
    endpoint: &str,
    cwd: &str,
) -> Result<String, &'static str> {
    let refused = "Terminals are not available on remote projects.";
    let binding = monocode_core::remote::parse_remote_path(project).ok_or(refused)?;
    if !enabled || expected.is_empty() || binding.environment_id != expected || !loopback(endpoint)
    {
        return Err(refused);
    }
    let path = if let Some(remote) = monocode_core::remote::parse_remote_path(cwd) {
        if remote.environment_id != expected {
            return Err(refused);
        }
        remote.host_path
    } else {
        cwd.into()
    };
    let path = Path::new(&path);
    if !path.is_absolute() || !path.is_dir() {
        return Err("Project folder not found on this Mac");
    }
    path.canonicalize()
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|_| "Project folder not found on this Mac")
}

/// The Paseo daemon shares this Mac only behind a loopback endpoint.
fn loopback(endpoint: &str) -> bool {
    let authority = endpoint
        .strip_prefix("http://")
        .or_else(|| endpoint.strip_prefix("https://"))
        .and_then(|s| s.split('/').next())
        .unwrap_or("");
    authority
        .parse::<std::net::SocketAddr>()
        .is_ok_and(|a| a.ip().is_loopback())
        || authority
            .strip_prefix("localhost:")
            .is_some_and(|port| port.parse::<u16>().is_ok())
}

/// A file link in a Paseo conversation names a `remote://` path on the
/// loopback host, which is this Mac; other remote hosts keep their paths.
pub(crate) fn local_file(
    enabled: bool,
    expected: &str,
    endpoint: &str,
    raw: &str,
) -> Option<String> {
    let remote = monocode_core::remote::parse_remote_path(raw)?;
    (enabled && !expected.is_empty() && remote.environment_id == expected && loopback(endpoint))
        .then_some(remote.host_path)
}

/// Resolution outcome for opening a file path from a trusted source conversation/project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OpenFileResolution {
    /// Convert a local Paseo `remote://<mac>/...` path to a local Mac filesystem path.
    LocalMac(String),
    /// Keep the path as-is (relative to the current project or same-host remote path).
    Keep,
    /// Refuse cross-host/local-Mac file access initiated from a remote Devbox session.
    Blocked(&'static str),
}

/// Enforces host isolation on file links: a remote Devbox session may NEVER
/// resolve a link onto this Mac's local filesystem (even if the link itself
/// declares the local Mac `serverId`, `file://`, or a bare absolute `/path`).
pub(crate) fn resolve_open_file(
    enabled: bool,
    expected: &str,
    endpoint: &str,
    source_cwd: &str,
    raw: &str,
) -> OpenFileResolution {
    let source_remote = monocode_core::remote::parse_remote_path(source_cwd);
    let source_is_foreign_remote = source_remote.as_ref().is_some_and(|src| {
        !(enabled && !expected.is_empty() && src.environment_id == expected && loopback(endpoint))
    });
    if source_is_foreign_remote {
        let src_env = source_remote
            .as_ref()
            .map(|r| r.environment_id.as_str())
            .unwrap_or("");
        if let Some(target_remote) = monocode_core::remote::parse_remote_path(raw) {
            if target_remote.environment_id != src_env {
                return OpenFileResolution::Blocked(
                    "Remote sessions cannot open files from another host.",
                );
            }
            return OpenFileResolution::Keep;
        }
        let lower = raw.to_ascii_lowercase();
        let stripped = if lower.starts_with("file://") {
            &raw[7..]
        } else {
            raw
        };
        if lower.starts_with("file:") || stripped.starts_with('/') || raw.contains("://") {
            return OpenFileResolution::Blocked("Remote sessions cannot open local Mac files.");
        }
        return OpenFileResolution::Keep;
    }
    if let Some(local) = local_file(enabled, expected, endpoint, raw) {
        return OpenFileResolution::LocalMac(local);
    }
    OpenFileResolution::Keep
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum StatusBucket {
    NeedsInput,
    Failed,
    Attention,
    Working,
    Done,
}
impl StatusBucket {
    pub(crate) const ALL: [Self; 5] = [
        Self::NeedsInput,
        Self::Failed,
        Self::Attention,
        Self::Working,
        Self::Done,
    ];
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::NeedsInput => "Needs input",
            Self::Failed => "Failed",
            Self::Attention => "Attention",
            Self::Working => "Working",
            Self::Done => "Done",
        }
    }
    pub(crate) fn from_details(details: &Value, busy: bool, approval: bool) -> Self {
        if approval {
            return Self::NeedsInput;
        }
        match details["bucket"].as_str() {
            Some("needs_input") => Self::NeedsInput,
            Some("failed") => Self::Failed,
            Some("attention") => Self::Attention,
            Some("running") => Self::Working,
            _ if busy => Self::Working,
            _ => Self::Done,
        }
    }
}

pub(crate) const PROJECT_SELECTION: &str = "paseo_project_selection";
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct ProjectSelection {
    pub(crate) default_names: Vec<String>,
    pub(crate) added: Vec<String>,
}
impl ProjectSelection {
    pub(crate) fn read(value: Option<&Value>) -> Self {
        value
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default()
    }
    pub(crate) fn includes(&self, key: &str, name: &str, available: bool) -> bool {
        self.added.iter().any(|p| p == key)
            || (available && self.default_names.iter().any(|n| n == name))
    }
}
pub(crate) fn project_ui(preferences: &crate::settings::Preferences) -> bool {
    active() || preferences.extra.contains_key(PROJECT_SELECTION)
}

pub(crate) const PASEO_HOSTS: &str = "paseo_hosts";

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct RemoteHostInfo {
    pub(crate) registration_id: String,
    pub(crate) server_id: Option<String>,
    pub(crate) label: String,
    pub(crate) enabled: bool,
    pub(crate) status: String,
    pub(crate) target_description: String,
    pub(crate) is_remote: bool,
    pub(crate) connected: bool,
}

pub(crate) fn parse_hosts_response(local_server_id: &str, value: &Value) -> Vec<RemoteHostInfo> {
    let self_label = value["self"]["label"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Local Mac")
        .to_owned();
    let mut out = vec![RemoteHostInfo {
        registration_id: "self".into(),
        server_id: Some(local_server_id.to_owned()),
        label: self_label,
        enabled: true,
        status: "online".into(),
        target_description: "127.0.0.1".into(),
        is_remote: false,
        connected: true,
    }];
    if let Some(rows) = value["hosts"].as_array() {
        for row in rows {
            let server_id = row["serverId"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            if server_id.as_deref() == Some(local_server_id) {
                continue;
            }
            let registration_id = row["registrationId"].as_str().unwrap_or("").to_owned();
            let target_description = row["targetDescription"].as_str().unwrap_or("").to_owned();
            let label = row["label"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .or_else(|| (!target_description.is_empty()).then_some(target_description.as_str()))
                .or(server_id.as_deref())
                .unwrap_or(if registration_id.is_empty() {
                    "Remote host"
                } else {
                    &registration_id
                })
                .to_owned();
            let enabled = row["enabled"].as_bool().unwrap_or(true);
            let status = row["status"]
                .as_str()
                .unwrap_or(if enabled { "offline" } else { "disabled" })
                .to_owned();
            let connected = row["connected"].as_bool().unwrap_or(false)
                || row["connectedEndpoint"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty());
            out.push(RemoteHostInfo {
                registration_id,
                server_id,
                label,
                enabled,
                status,
                target_description,
                is_remote: true,
                connected,
            });
        }
    }
    out
}

pub(crate) fn list_hosts(
    connections: &Connections,
    local_server_id: &str,
) -> Result<Vec<RemoteHostInfo>, String> {
    let primary = connections.machine_for_environment(local_server_id)?;
    let value = connections.request(&primary.id, "paseo.hosts.list", json!({}))?;
    Ok(parse_hosts_response(local_server_id, &value))
}

fn cache_project_icon(
    directory: &Path,
    remote_key: &str,
    project: &Value,
) -> Result<Option<String>, String> {
    let Some(icon) = project.get("icon") else {
        return Ok(None);
    };
    let (Some(data), Some(mime)) = (icon["data"].as_str(), icon["mimeType"].as_str()) else {
        return Ok(None);
    };
    if data.len() > 2_000_000 {
        return Ok(None);
    }
    let Some(extension) = (match mime {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/svg+xml" => Some("svg"),
        "image/webp" => Some("webp"),
        _ => None,
    }) else {
        return Ok(None);
    };
    use base64::Engine;
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) else {
        return Ok(None);
    };
    let cache = directory.join("paseo-project-icons");
    std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    // Agent/project strings never become filesystem path segments.
    let path = cache.join(format!(
        "{}.{}",
        {
            use std::hash::{Hash, Hasher};
            let mut h = std::hash::DefaultHasher::new();
            remote_key.hash(&mut h);
            h.finish()
        },
        extension
    ));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

pub(crate) struct ConnectedRemoteHost {
    pub(crate) server_id: String,
    pub(crate) label: String,
    pub(crate) projects: Vec<String>,
    pub(crate) all_projects: Vec<String>,
    pub(crate) labels: std::collections::HashMap<String, String>,
    pub(crate) icons: std::collections::HashMap<String, String>,
    pub(crate) summaries: Vec<(monocode_core::store::Session, bool, bool, bool)>,
    pub(crate) presentation: std::collections::HashMap<i64, Value>,
    pub(crate) started_by: std::collections::HashMap<i64, i64>,
}

pub(crate) fn connect_remote_host(
    directory: &Path,
    store: &Arc<Mutex<Store>>,
    connections: &Connections,
    local_server_id: &str,
    server_id: &str,
    selection: &ProjectSelection,
) -> Result<ConnectedRemoteHost, String> {
    if server_id.is_empty() || server_id == local_server_id {
        return Err("Invalid remote Paseo host".into());
    }
    let primary = connections.machine_for_environment(local_server_id)?;
    let info = connections.request(
        &primary.id,
        "paseo.hosts.connect",
        json!({ "serverId": server_id }),
    )?;
    if info["serverId"].as_str() != Some(server_id) {
        return Err("Remote Paseo daemon identity mismatch".into());
    }
    let endpoint = info["endpoint"]
        .as_str()
        .ok_or("Missing remote host endpoint")?;
    if !loopback(endpoint) {
        return Err("Remote Paseo host proxy must bind loopback".into());
    }
    let token = std::fs::read_to_string(directory.join("paseo-host-token"))
        .map_err(|e| format!("Missing Paseo host token: {e}"))?;
    let label = info["label"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(server_id)
        .to_owned();
    let machine = connections.connect(
        format!("Paseo ({label})"),
        endpoint.to_owned(),
        token.trim().to_owned(),
    )?;
    if machine.environment_id != server_id {
        let _ = connections.disconnect(&machine.id);
        return Err("Remote Paseo daemon identity mismatch".into());
    }
    let visible = connections.request(&machine.id, "projects.list", json!({}))?;
    let projects = connections.request(
        &machine.id,
        "projects.list",
        json!({"includeAll":true,"includeIcons":true}),
    )?;
    let rows = projects.as_array().ok_or("Invalid Paseo projects")?;
    if rows.is_empty() {
        return Err(crate::i18n::t("Remote Paseo host has no projects"));
    }

    let mut labels = std::collections::HashMap::new();
    let mut icons = std::collections::HashMap::new();
    let mut all_projects = Vec::new();
    let mut chosen_projects = Vec::new();

    let remembered = {
        let guard = store.lock().map_err(|_| "Store unavailable")?;
        guard.ephemeral_remote_status(&machine.environment_id);
        let mut items = Vec::with_capacity(rows.len());
        for project in rows {
            let remote = guard.remember_remote_project(&machine.environment_id, project)?;
            let is_visible = visible
                .as_array()
                .is_some_and(|v| v.iter().any(|p| p["id"] == project["id"]));
            let name = project["name"].as_str().unwrap_or("Paseo");
            let chosen = selection.includes(&remote.key, name, is_visible);
            items.push((remote, project.clone(), is_visible, chosen));
        }
        items
    };

    let any_chosen = remembered.iter().any(|(_, _, _, chosen)| *chosen);
    let fallback_idx = if any_chosen {
        None
    } else {
        remembered
            .iter()
            .position(|(_, _, is_visible, _)| *is_visible)
            .or(Some(0))
    };

    let mut fetched_sessions = Vec::new();
    for (idx, (remote, project, _, chosen)) in remembered.into_iter().enumerate() {
        let include = chosen || fallback_idx == Some(idx);
        let raw_name = project["name"].as_str().unwrap_or("Paseo");
        labels.insert(remote.key.clone(), format!("{raw_name} · {label}"));
        all_projects.push(remote.key.clone());
        if let Some(icon_path) = cache_project_icon(directory, &remote.key, &project)? {
            icons.insert(remote.key.clone(), icon_path);
        }
        if include {
            chosen_projects.push(remote.key.clone());
            let response = connections.request(
                &machine.id,
                "sessions.list",
                json!({"projectId": remote.project_id}),
            )?;
            let values = response
                .as_array()
                .or_else(|| response["values"].as_array())
                .ok_or("Invalid Paseo sessions")?
                .clone();
            fetched_sessions.push((remote, values));
        }
    }

    let (summaries, presentation, started_by) = {
        let guard = store.lock().map_err(|_| "Store unavailable")?;
        let mut all_summaries = Vec::new();
        let mut all_presentation = std::collections::HashMap::new();
        for (remote, values) in &fetched_sessions {
            let registered = guard.sync_remote_summaries(remote, values)?;
            for (r, v) in registered.into_iter().zip(values) {
                all_presentation.insert(
                    r.summary.id,
                    json!({
                        "bucket": v["paseoBucket"],
                        "enteredAt": v["paseoStatusEnteredAt"],
                        "createdAt": v["createdAt"],
                        "workspaceAnchor": v["paseoWorkspaceAnchor"],
                    }),
                );
                all_summaries.push((r.summary, r.running, r.needs_input, v["draft"] == true));
            }
        }
        let retained: std::collections::HashSet<_> = chosen_projects.iter().cloned().collect();
        guard.compact_remote_cache(&machine.environment_id, &retained)?;
        let started_by = guard.started_links()?;
        (all_summaries, all_presentation, started_by)
    };

    Ok(ConnectedRemoteHost {
        server_id: machine.environment_id,
        label,
        projects: chosen_projects,
        all_projects,
        labels,
        icons,
        summaries,
        presentation,
        started_by,
    })
}

pub(crate) struct Host {
    pub(crate) child: Arc<Mutex<Child>>,
    pub(crate) cwd: String,
    pub(crate) server_id: String,
    pub(crate) endpoint: String,
    pub(crate) store: Option<Store>,
    pub(crate) selection: ProjectSelection,
    pub(crate) projects: Vec<String>,
    pub(crate) all_projects: Vec<String>,
    pub(crate) icons: std::collections::HashMap<String, String>,
    pub(crate) labels: std::collections::HashMap<String, String>,
    pub(crate) hosts: Vec<RemoteHostInfo>,
}
impl Drop for Host {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
impl Host {
    pub(crate) fn install_quit(child: Arc<Mutex<Child>>, cx: &mut gpui::App) {
        cx.on_app_quit(move |cx| {
            let child = child.clone();
            cx.background_executor().spawn(async move {
                if let Ok(mut child) = child.lock() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            })
        })
        .detach();
    }
}

fn resources() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("PASEO_PROTOTYPE_RESOURCES") {
        return Ok(path.into());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    Ok(exe
        .parent()
        .and_then(Path::parent)
        .ok_or("Missing bundle")?
        .join("Resources"))
}

pub(crate) fn start(directory: &Path) -> Result<Host, String> {
    let resource = resources()?;
    let node = prototype_config()
        .get("nodePath")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .map(Ok)
        .unwrap_or_else(|| {
            std::fs::read_to_string(resource.join("node-path.txt")).map_err(|e| e.to_string())
        })?;
    let mut child = Command::new(node.trim())
        .arg(resource.join("paseo-host.mjs"))
        .arg(directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().ok_or("Missing host output")?;
    let mut host = Host {
        child: Arc::new(Mutex::new(child)),
        cwd: String::new(),
        server_id: String::new(),
        endpoint: String::new(),
        store: None,
        selection: Default::default(),
        labels: Default::default(),
        projects: Vec::new(),
        all_projects: Vec::new(),
        icons: Default::default(),
        hosts: Vec::new(),
    };
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
        let _ = send.send(result);
    });
    let line = receive
        .recv_timeout(Duration::from_secs(40))
        .map_err(|_| "Paseo host startup timed out")?
        .map_err(|e| e.to_string())?;
    let info: Value = serde_json::from_str(&line)
        .map_err(|_| "Paseo host did not start; daemon connection or loopback unavailable")?;
    let connections = Connections::new(directory);
    let remembered = connections
        .list()?
        .first()
        .map(|m| m.environment_id.clone());
    let expected = prototype_config()["serverId"]
        .as_str()
        .map(str::to_owned)
        .or(remembered);
    if info["serverId"].as_str().is_none_or(str::is_empty)
        || expected
            .as_deref()
            .is_some_and(|expected| info["serverId"] != expected)
    {
        return Err("Paseo daemon identity mismatch".into());
    }
    // This directory belongs only to this bundle. Exactly one machine is provisioned.
    for old in connections.list()? {
        connections.disconnect(&old.id)?;
    }
    let machine = connections.connect(
        "Paseo".into(),
        info["endpoint"]
            .as_str()
            .ok_or("Missing host endpoint")?
            .into(),
        info["token"]
            .as_str()
            .ok_or("Missing host credential")?
            .into(),
    )?;
    if machine.environment_id != info["serverId"].as_str().unwrap_or("") {
        return Err("Paseo daemon identity mismatch".into());
    }
    host.server_id = machine.environment_id.clone();
    host.endpoint = machine.endpoint.clone();
    host.hosts = list_hosts(&connections, &host.server_id)
        .unwrap_or_else(|_| parse_hosts_response(&host.server_id, &json!({})));
    let db = Store::open_monocode(&directory.join("monocode.db"))?;
    db.ephemeral_remote_status(&machine.environment_id);
    let visible = connections.request(&machine.id, "projects.list", json!({}))?;
    let projects = connections.request(
        &machine.id,
        "projects.list",
        json!({"includeAll":true,"includeIcons":true}),
    )?;
    let saved = db
        .ui_state()?
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
    host.selection = ProjectSelection::read(
        saved
            .as_ref()
            .and_then(|v| v["preferences"].get(PROJECT_SELECTION)),
    );
    if saved
        .as_ref()
        .and_then(|v| v["preferences"].get(PROJECT_SELECTION))
        .is_none()
    {
        host.selection.default_names = prototype_config()["defaultProjectNames"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        if host.selection.default_names.is_empty()
            && let Some(name) = visible
                .as_array()
                .and_then(|a| a.first())
                .and_then(|p| p["name"].as_str())
        {
            host.selection.default_names.push(name.into());
        }
    }
    let selected = saved
        .as_ref()
        .and_then(|v| v["project"].as_str())
        .map(str::to_owned);
    for project in projects.as_array().ok_or("Invalid Paseo projects")? {
        let remote = db.remember_remote_project(&machine.environment_id, project)?;
        let is_visible = visible
            .as_array()
            .is_some_and(|rows| rows.iter().any(|p| p["id"] == project["id"]));
        let chosen = host.selection.includes(
            &remote.key,
            project["name"].as_str().unwrap_or(""),
            is_visible,
        );
        if chosen {
            let sessions = connections.request(
                &machine.id,
                "sessions.list",
                json!({"projectId":remote.project_id}),
            )?;
            let values = sessions.as_array().ok_or("Invalid Paseo sessions")?;
            db.sync_remote_summaries(&remote, values)?;
        }
        host.labels.insert(
            remote.key.clone(),
            project["name"].as_str().unwrap_or("Paseo").into(),
        );
        host.all_projects.push(remote.key.clone());
        if chosen {
            host.projects.push(remote.key.clone());
        }
        if let Some(icon_path) = cache_project_icon(directory, &remote.key, project)? {
            host.icons.insert(remote.key.clone(), icon_path);
        }
    }
    host.cwd = host
        .projects
        .first()
        .or_else(|| host.all_projects.first())
        .cloned()
        .ok_or("Paseo has no projects")?;
    if host.projects.is_empty() {
        host.projects.push(host.cwd.clone());
    }
    let retained: std::collections::HashSet<_> = host.projects.iter().cloned().collect();
    // Hidden project directories can be fetched on demand; no background registration.
    let _ = selected;
    db.compact_remote_cache(&machine.environment_id, &retained)?;
    host.store = Some(db);
    Ok(host)
}

fn check_scratch(directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let canonical = directory.canonicalize().map_err(|e| e.to_string())?;
    let temporary = std::env::temp_dir()
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !canonical.starts_with(&temporary) && !canonical.starts_with("/private/tmp") {
        return Err("Host checks require an isolated temporary directory".into());
    }
    Ok(())
}

pub(crate) fn check(directory: &Path) -> Result<(), String> {
    check_scratch(directory)?;
    let mut host = start(directory)?;
    let connections = Connections::new(directory);
    let machine = connections.list()?.into_iter().next().ok_or("No machine")?;
    let db = host.store.take().ok_or("Bootstrap store missing")?;
    let sessions = db.sessions()?;
    let selected = sessions
        .iter()
        .find(|s| s.cwd == host.cwd && !s.archived)
        .ok_or("No sessions in default project")?;
    let binding = db
        .remote_session(selected.id)?
        .ok_or("Missing remote identity")?;
    let workspace_choices = connections.request(
        &machine.id,
        "paseo.workspaces",
        json!({"projectId":binding.project.project_id}),
    )?;
    let workspace_choices = workspace_choices
        .as_array()
        .ok_or("Invalid workspace choices")?;
    let id = binding.session_id.ok_or("Missing agent ID")?;
    let snapshot = connections.load_session(&machine.id, &id, None)?;
    let repeated = connections.load_session(&machine.id, &id, Some(&snapshot))?;
    if snapshot["session"]["id"] != id || repeated["session"]["id"] != id {
        return Err("Wrong agent".into());
    }
    let (history, turns) = Store::project_remote_history(&snapshot)?;
    let descriptor = connections.request(&machine.id, "environment.describe", json!({}))?;
    println!(
        "{}",
        json!({"serverId":machine.environment_id,"agentId":id,"sessions":sessions.len(),"nativeRows":history.rows.len(),
        "nativeTurns":turns.len(),"revision":snapshot["revision"],"version":descriptor["name"],"shell":"original",
        "identityVerified":true,"readOnly":true,"workspaceChoicesRead":true,"workspaceChoiceCount":workspace_choices.len(),"visibleProjects":host.projects.len(),"allProjects":host.all_projects.len(),"iconsResolved":host.icons.len(),"visibleProjectNames":host.projects.iter().filter_map(|key|host.labels.get(key)).collect::<Vec<_>>(),"defaultProjectMatchesNewest":host.projects.first()==Some(&host.cwd)})
    );
    Ok(())
}

/// Initial-open background path, twice over the newest sessions. Deliberately
/// omits view/clearAttention and never attaches a runtime or sends a command.
pub(crate) fn switch_check(directory: &Path) -> Result<(), String> {
    check_scratch(directory)?;
    let mut host = start(directory)?;
    let store = Arc::new(Mutex::new(
        host.store.take().ok_or("Bootstrap store missing")?,
    ));
    let client = Arc::new(Connections::new(directory));
    let mut sessions = store.lock().map_err(|e| e.to_string())?.sessions()?;
    sessions.retain(|s| !s.archived);
    sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
    sessions.truncate(10);
    if sessions.is_empty() {
        return Err("No Paseo sessions to measure".into());
    }
    let mut samples = Vec::new();
    for pass in ["cold", "warm"] {
        for (index, session) in sessions.iter().enumerate() {
            let prepared = crate::conversation::remote::prepare_remote_open(
                &store,
                session.id,
                None,
                Some(client.clone()),
                true,
            )?;
            samples.push(json!({"pass":pass,"sessionIndex":index,"phases":prepared.timings}));
        }
    }
    println!(
        "{}",
        json!({"readOnly":true,"sameInitialOpenWorker":true,
        "viewCommands":0,"sessions":sessions.len(),"samples":samples})
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn disk_bytes() -> Option<u64> {
    let mut usage: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::proc_pid_rusage(
            std::process::id() as i32,
            libc::RUSAGE_INFO_V2,
            (&mut usage as *mut libc::rusage_info_v2).cast(),
        )
    };
    (result == 0).then_some(usage.ri_diskio_byteswritten)
}
#[cfg(not(target_os = "macos"))]
fn disk_bytes() -> Option<u64> {
    None
}

/// Same worker as Shell and same single-snapshot persistence as Conversation;
/// metadata/history reads only. No view/clearAttention or agent commands.
pub(crate) fn perf_check(directory: &Path) -> Result<(), String> {
    check_scratch(directory)?;
    let mut host = start(directory)?;
    let client = Connections::new(directory);
    let machine = client.list()?.into_iter().next().ok_or("No machine")?;
    let db = host.store.take().ok_or("Bootstrap store missing")?;
    let sessions = db.sessions()?;
    let selected = sessions
        .iter()
        .find(|s| {
            s.cwd == host.cwd
                && !s.archived
                && db
                    .remote_session(s.id)
                    .ok()
                    .flatten()
                    .is_some_and(|r| !r.running && !r.needs_input)
        })
        .ok_or("No idle session in default project; retry when quiet")?;
    let binding = db
        .remote_session(selected.id)?
        .ok_or("Missing remote binding")?;
    let agent = binding.session_id.clone().ok_or("Missing agent identity")?;
    let store = Arc::new(Mutex::new(db));
    let warm =
        crate::runtime::remote::refresh_project_index(&store, &client, &host.cwd, None, true)?
            .ok_or("Initial index missing")?;
    let mut revision = warm.revision;
    let mut structure = warm
        .structural_revision
        .ok_or("Structural directory revision missing")?;
    let warm_lock_ms = warm.lock_ms;
    let mut snapshot = client.load_session(&machine.id, &agent, None)?;
    store
        .lock()
        .map_err(|_| "Store unavailable")?
        .register_remote_summary(&binding.project, &snapshot)?;
    let before = store
        .lock()
        .map_err(|_| "Store unavailable")?
        .sidecar_write_metrics();
    let os_before = disk_bytes();
    let started = std::time::Instant::now();
    let mut directory_changes = 0;
    let mut snapshot_changes = 0;
    let mut volatile_changes = 0;
    let mut structural_changes = 0;
    let mut max_lock_ms = 0f64;
    let mut max_snapshot_lock_ms = 0f64;
    for tick in 1..=10 {
        let due = Duration::from_secs(tick * 3);
        if let Some(wait) = due.checked_sub(started.elapsed()) {
            std::thread::sleep(wait);
        }
        if let Some(index) = crate::runtime::remote::refresh_project_index(
            &store,
            &client,
            &host.cwd,
            revision.as_deref(),
            true,
        )? {
            revision = index.revision;
            directory_changes += 1;
            let next_structure = index
                .structural_revision
                .ok_or("Structural directory revision missing")?;
            if next_structure != structure {
                structural_changes += 1;
            } else {
                volatile_changes += 1;
            }
            structure = next_structure;
            max_lock_ms = max_lock_ms.max(index.lock_ms);
        }
        let next = client.load_session(&machine.id, &agent, Some(&snapshot))?;
        if next["revision"] != snapshot["revision"] {
            snapshot_changes += 1;
        }
        let guard = store.lock().map_err(|_| "Store unavailable")?;
        let held = std::time::Instant::now();
        guard.register_remote_summary(&binding.project, &next)?;
        max_snapshot_lock_ms = max_snapshot_lock_ms.max(held.elapsed().as_secs_f64() * 1000.);
        drop(guard);
        snapshot = next;
    }
    let after = store
        .lock()
        .map_err(|_| "Store unavailable")?
        .sidecar_write_metrics();
    let os_written = os_before
        .zip(disk_bytes())
        .map(|(before, after)| after.saturating_sub(before));
    let logical = after.1 - before.1;
    println!(
        "{}",
        json!({"readOnlyDaemon":true,"windowOpened":false,"serverId":machine.environment_id,
        "sessions":sessions.len(),"visibleProjects":host.projects.len(),"allProjects":host.all_projects.len(),
        "seconds":started.elapsed().as_secs_f64(),"ticks":10,"sidecarWrites":after.0-before.0,"sidecarPayloadBytes":logical,
        "nativeDiskBytesWritten":os_written,"warmRefreshLockMs":warm_lock_ms,"maxRefreshLockMs":max_lock_ms,
        "maxSnapshotLockMs":max_snapshot_lock_ms,"directoryChanges":directory_changes,"volatileDirectoryChanges":volatile_changes,"structuralDirectoryChanges":structural_changes,"snapshotChanges":snapshot_changes,
        "steady":structural_changes==0 && snapshot_changes==0,"targetBytes":1_000_000,
        "targetMet":logical<1_000_000 && os_written.is_some_and(|bytes|bytes<1_000_000)})
    );
    Ok(())
}

/// Pure UI restrictions for this bundle; never changes real-host defaults.
pub(crate) fn restrict_workspace(w: &mut crate::workspace::Workspace) {
    if !w.preferences.extra.contains_key("paseo_local_ui_v1") {
        w.preferences.notes = true;
        w.sidebar_notes_visible = true;
        w.preferences.working_agents = true;
        w.sidebar_activity_visible = true;
        w.preferences
            .extra
            .insert("paseo_local_ui_v1".into(), json!(true));
    }

    w.sidebar_automations_visible = false;
    w.sidebar_inbox_visible = false;
    w.preferences.monos = false;
    w.preferences.quick_composer = false;
    // Paseo quota defaults to remaining %, once; later explicit choices are kept.
    if !w.preferences.extra.contains_key("paseo_usage_v1") {
        w.preferences.show_remaining_usage = true;
        w.preferences
            .extra
            .insert("paseo_usage_v1".into(), json!(true));
    }
    w.preferences.idle_agents = 0;
    w.title_order.retain(|tab| {
        matches!(
            tab,
            crate::workspace::TitleTab::Session(_)
                | crate::workspace::TitleTab::Search
                | crate::workspace::TitleTab::App(crate::workspace::AppView::Notes)
        )
    });
    w.scope_focus.retain(|_, tab| {
        matches!(
            tab,
            crate::workspace::TitleTab::Session(_)
                | crate::workspace::TitleTab::Search
                | crate::workspace::TitleTab::App(crate::workspace::AppView::Notes)
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{App, AppContext, TestAppContext, px};
    fn configure(cx: &mut App) {
        let preferences = crate::settings::Preferences {
            paced_reveal: false,
            ..Default::default()
        };
        crate::theme::configure(&preferences, false);
        cx.set_global(preferences);
        crate::input::bind(cx);
        crate::popover::bind(cx);
        crate::shell::bind_keys(cx);
        cx.set_reduce_motion(true);
    }
    #[test]
    fn local_terminal_authority_and_status_order_are_explicit() {
        let dir = std::env::temp_dir().join(format!(
            "paseo-terminal-test-{}",
            monocode_core::mono::new_id()
        ));
        std::fs::create_dir(&dir).unwrap();
        let cwd = dir.as_path().to_str().unwrap();
        let project = format!("remote://fixture{cwd}");
        assert_eq!(
            terminal_path(true, "fixture", &project, "http://127.0.0.1:1234", cwd).unwrap(),
            dir.as_path().canonicalize().unwrap().to_str().unwrap()
        );
        assert!(
            terminal_path(false, "fixture", &project, "http://127.0.0.1:1234", cwd)
                .unwrap_err()
                .contains("remote projects")
        );
        assert!(terminal_path(true, "other", &project, "http://127.0.0.1:1234", cwd).is_err());
        assert!(
            terminal_path(
                true,
                "fixture",
                &project,
                "http://example.invalid:1234",
                cwd
            )
            .is_err()
        );
        assert_eq!(
            terminal_path(
                true,
                "fixture",
                &project,
                "http://127.0.0.1:1234",
                "/synthetic/missing-folder"
            )
            .unwrap_err(),
            "Project folder not found on this Mac"
        );
        let link = format!("remote://fixture{cwd}/src/main.rs:12:1");
        assert_eq!(
            local_file(true, "fixture", "http://127.0.0.1:1234", &link).unwrap(),
            format!("{cwd}/src/main.rs:12:1")
        );
        assert!(local_file(false, "fixture", "http://127.0.0.1:1234", &link).is_none());
        assert!(local_file(true, "other", "http://127.0.0.1:1234", &link).is_none());
        assert!(local_file(true, "fixture", "http://example.invalid:1234", &link).is_none());
        assert!(local_file(true, "fixture", "http://127.0.0.1:1234", "/plain/path").is_none());
        assert_eq!(
            StatusBucket::ALL.map(StatusBucket::label),
            ["Needs input", "Failed", "Attention", "Working", "Done"]
        );
        for (value, wanted) in [
            ("needs_input", StatusBucket::NeedsInput),
            ("failed", StatusBucket::Failed),
            ("attention", StatusBucket::Attention),
            ("running", StatusBucket::Working),
            ("done", StatusBucket::Done),
        ] {
            assert_eq!(
                StatusBucket::from_details(&json!({"bucket":value}), false, false),
                wanted
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn project_selection_defaults_and_add_back_are_persistent_opt_in() {
        let config = ProjectSelection {
            default_names: vec!["Workspace".into(), "Helpers".into()],
            added: vec![],
        };
        assert!(config.includes("daily", "Workspace", true));
        assert!(config.includes("bots", "Helpers", true));
        assert!(!config.includes("fixture", "fixture", true));
        assert!(!config.includes("daily", "Workspace", false));
        let mut config = config;
        config.added.push("fixture".into());
        let value = serde_json::to_value(config).unwrap();
        let restored = ProjectSelection::read(Some(&value));
        assert!(restored.includes("fixture", "fixture", false));
        assert!(!project_ui(&crate::settings::Preferences::default()));
    }
    #[gpui::test]
    fn paseo_default_shell_exposes_new_chat_and_dispatches_selected_project_draft(
        cx: &mut TestAppContext,
    ) {
        cx.update(configure);
        let window = cx.add_window(|_, cx| crate::shell::paseo_shell_fixture(cx));
        let cx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("new-tab").is_some(),
            "original titlebar + exists"
        );
        // Switch by the real original project tab, then click the new affordance.
        let project = cx.debug_bounds("project-space-1").expect("Helpers tab");
        cx.simulate_click(project.center(), Default::default());
        cx.run_until_parked();
        let button = cx
            .debug_bounds("sidebar-new-chat")
            .expect("New chat visible");
        cx.simulate_click(button.center(), Default::default());
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                assert_eq!(shell.workspace.project, "remote://paseo-fixture/second");
                shell.paseo_fixture_check_draft_and_add(cx);
            })
            .unwrap();
    }
    #[gpui::test]
    fn original_shell_mounts_remote_identity_and_real_workspace_layout(cx: &mut TestAppContext) {
        cx.update(configure);
        let window = cx.add_window(|_, cx| crate::shell::paseo_shell_fixture(cx));
        cx.refresh().unwrap();
        window
            .update(cx, |shell, _, _| {
                assert!(
                    shell
                        .workspace
                        .project
                        .starts_with("remote://paseo-fixture/")
                );
                assert!(!shell.workspace.preferences.monos);
                assert!(!shell.workspace.sidebar_automations_visible);
                assert_eq!(shell.workspace.tabs.len(), 1);
            })
            .unwrap();
    }
    #[gpui::test]
    fn adapter_blocks_replay_into_original_transcript(cx: &mut TestAppContext) {
        cx.update(configure);
        let Some(directory) = std::env::var_os("PASEO_R8_BLOCKS") else {
            return;
        };
        let window = cx.add_window(|_, _| crate::transcript::TranscriptView::new());
        let files = std::fs::read_dir(directory)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(files.len(), 16);
        let mut count = 0;
        for file in files {
            let snapshots: Vec<Value> =
                serde_json::from_slice(&std::fs::read(file.path()).unwrap()).unwrap();
            for snapshot in snapshots {
                count += 1;
                let (history, turns) = Store::project_remote_history(&snapshot).unwrap();
                window
                    .update(cx, |view, _, cx| {
                        view.paseo_mode();
                        view.set_turns(turns);
                        view.restore_history(history.rows, cx);
                        let _ = view.test_work_rows(false, cx);
                    })
                    .unwrap();
                cx.refresh().unwrap();
            }
        }
        assert_eq!(count, 37);
    }
    #[gpui::test]
    fn authoritative_paseo_history_replacement_drops_removed_tail(cx: &mut TestAppContext) {
        cx.update(configure);
        let window = cx.add_window(|_, _| crate::transcript::TranscriptView::new());
        window
            .update(cx, |view, _, cx| {
                view.paseo_mode();
                for texts in [
                    vec!["old question", "old answer", "old tail"],
                    vec!["new answer"],
                    vec![],
                ] {
                    let rows = texts
                        .iter()
                        .map(|text| monocode_core::transcript::Row {
                            role: "assistant".into(),
                            text: (*text).into(),
                            first: true,
                            sent_at: None,
                        })
                        .collect();
                    view.install_prepared_history(
                        crate::transcript::PreparedHistory::new(rows, false),
                        cx,
                    );
                    assert_eq!(
                        view.source_rows()
                            .iter()
                            .map(|r| r.text.as_str())
                            .collect::<Vec<_>>(),
                        texts
                    );
                }
            })
            .unwrap();
    }
    #[test]
    #[cfg(target_os = "macos")]
    #[ignore = "Requires Metal; actual original Shell, synthetic data, no OS window/daemon"]
    fn render_paseo_full_shell_fixture() {
        let mut cx = gpui::HeadlessAppContext::with_platform(
            Arc::new(gpui_apple::AppleTextSystem::new()),
            Arc::new(crate::assets::Assets),
            || {
                Ok(Some(Box::new(
                    gpui_apple::metal_renderer::MetalHeadlessRenderer::new(),
                )))
            },
        );
        cx.update(configure);
        let window = cx
            .open_window(gpui::size(px(1180.), px(800.)), |_, cx| {
                cx.new(crate::shell::paseo_shell_fixture)
            })
            .unwrap();
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        })
        .unwrap();
        cx.run_until_parked();
        let image = cx.capture_screenshot(window.into()).unwrap();
        let expected = cx
            .update_window(window.into(), |_, window, _| {
                window.bounds().size.to_device_pixels(window.scale_factor())
            })
            .unwrap();
        assert_eq!(
            (image.width(), image.height()),
            (expected.width.0 as u32, expected.height.0 as u32)
        );
        let output = PathBuf::from(
            std::env::var_os("PASEO_R11B_RENDER_DIR")
                .or_else(|| std::env::var_os("PASEO_R11_RENDER_DIR"))
                .or_else(|| std::env::var_os("PASEO_R9_RENDER_DIR"))
                .or_else(|| std::env::var_os("PASEO_R8_RENDER_DIR"))
                .expect("Set isolated render directory"),
        );
        std::fs::create_dir_all(&output).unwrap();
        image.save(output.join("r11-default-window.png")).unwrap();

        cx.update_window(window.into(), |shell, window, cx| {
            shell
                .downcast::<crate::shell::Shell>()
                .unwrap()
                .update(cx, |shell, cx| {
                    shell.paseo_fixture_show_switcher(window, cx);
                });
            window.refresh();
            window.draw(cx).clear(cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.capture_screenshot(window.into())
            .unwrap()
            .save(output.join("r11-project-switcher.png"))
            .unwrap();
        cx.update_window(window.into(), |shell, window, cx| {
            shell
                .downcast::<crate::shell::Shell>()
                .unwrap()
                .update(cx, |shell, cx| {
                    shell.paseo_fixture_show_favorites(window, cx)
                });
            window.refresh();
            window.draw(cx).clear(cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.capture_screenshot(window.into())
            .unwrap()
            .save(output.join("r11-favorites.png"))
            .unwrap();
        for lang in ["en", "zh-CN"] {
            cx.update_window(window.into(), |shell, window, cx| {
                shell
                    .downcast::<crate::shell::Shell>()
                    .unwrap()
                    .update(cx, |shell, cx| {
                        shell.paseo_fixture_r11b(lang, true, true, window, cx)
                    });
                window.refresh();
                window.draw(cx).clear(cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.capture_screenshot(window.into())
                .unwrap()
                .save(output.join(format!("r11b-{lang}-terminal-status.png")))
                .unwrap();
            cx.update_window(window.into(), |shell, window, cx| {
                shell
                    .downcast::<crate::shell::Shell>()
                    .unwrap()
                    .update(cx, |shell, cx| {
                        shell.paseo_fixture_show_favorites(window, cx)
                    });
                window.refresh();
                window.draw(cx).clear(cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.capture_screenshot(window.into())
                .unwrap()
                .save(output.join(format!("r11b-{lang}-favorites.png")))
                .unwrap();
        }
        cx.update_window(window.into(), |shell, window, cx| {
            shell
                .downcast::<crate::shell::Shell>()
                .unwrap()
                .update(cx, |shell, cx| shell.paseo_fixture_show_queue(window, cx));
            window.refresh();
            window.draw(cx).clear(cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.capture_screenshot(window.into())
            .unwrap()
            .save(output.join("r13-native-queue.png"))
            .unwrap();
    }

    struct MockLoopbackHost {
        endpoint: String,
        stopped: Arc<std::sync::atomic::AtomicBool>,
        join: Option<std::thread::JoinHandle<()>>,
    }
    impl MockLoopbackHost {
        fn spawn(
            env_id: &'static str,
            expected_token: &'static str,
            capabilities: Vec<&'static str>,
            mut handler: impl FnMut(&str, &Value) -> Result<Value, String> + Send + 'static,
        ) -> Self {
            use std::io::{Read, Write};
            use std::sync::atomic::{AtomicBool, Ordering};
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            listener.set_nonblocking(true).unwrap();
            let stopped = Arc::new(AtomicBool::new(false));
            let done = stopped.clone();
            let join = std::thread::spawn(move || {
                while !done.load(Ordering::Relaxed) {
                    let (mut stream, _) = match listener.accept() {
                        Ok(pair) => pair,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(_) => break,
                    };
                    stream.set_nonblocking(false).unwrap();
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut size = 0usize;
                    let mut authorized = false;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                            break;
                        }
                        if let Some((k, v)) = line.split_once(':') {
                            if k.eq_ignore_ascii_case("content-length") {
                                size = v.trim().parse().unwrap_or(0);
                            } else if k.eq_ignore_ascii_case("authorization") {
                                authorized = v.trim() == format!("Bearer {expected_token}");
                            }
                        }
                    }
                    let mut buf = vec![0u8; size];
                    let _ = reader.read_exact(&mut buf);
                    if !authorized {
                        let body = r#"{"error":"Device credential is invalid"}"#;
                        let _ = write!(
                            stream,
                            "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        continue;
                    }
                    let req: Value = serde_json::from_slice(&buf).unwrap_or_else(|_| json!({}));
                    let method = req["method"].as_str().unwrap_or("");
                    let reply = if method == "environment.describe" {
                        Ok(json!({
                            "protocolVersion": 1,
                            "environmentId": env_id,
                            "name": format!("Paseo {env_id}"),
                            "providers": ["codex"],
                            "capabilities": capabilities,
                        }))
                    } else if req["environmentId"].as_str() != Some(env_id) {
                        Err("Host identity changed".into())
                    } else {
                        handler(method, &req["params"])
                    };
                    let (status, payload) = match reply {
                        Ok(result) => ("200 OK", json!({ "result": result }).to_string()),
                        Err(error) => ("400 Bad Request", json!({ "error": error }).to_string()),
                    };
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                }
            });
            Self {
                endpoint,
                stopped,
                join: Some(join),
            }
        }
    }
    impl Drop for MockLoopbackHost {
        fn drop(&mut self) {
            self.stopped
                .store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
    }

    #[test]
    fn multihost_terminal_and_local_file_isolation_and_cross_host_session_scoping() {
        // Remote Devbox paths must never pass terminal_path or local_file against local-mac-id.
        assert_eq!(
            terminal_path(
                true,
                "local-mac-id",
                "remote://devbox-boe-id/home/user/repo",
                "http://127.0.0.1:1234",
                "/home/user/repo",
            ),
            Err("Terminals are not available on remote projects.")
        );
        assert_eq!(
            local_file(
                true,
                "local-mac-id",
                "http://127.0.0.1:1234",
                "remote://devbox-boe-id/home/user/repo/src/lib.rs:10:1",
            ),
            None
        );
        assert_eq!(
            local_file(
                true,
                "local-mac-id",
                "http://127.0.0.1:1234",
                "remote://local-mac-id/Users/user/repo/src/lib.rs:10:1",
            ),
            Some("/Users/user/repo/src/lib.rs:10:1".into())
        );

        // Identical repo cwds and identical agent IDs on Local Mac vs devbox-boe vs devbox-ai
        // never collide on project keys, session handles, or parent/child links.
        let store = Store::open(Path::new(":memory:")).unwrap();
        let proj_json =
            json!({"id": "paseo:/work/ai_scope", "cwd": "/work/ai_scope", "name": "ai_scope"});
        let mac_proj = store
            .remember_remote_project("local-mac-id", &proj_json)
            .unwrap();
        let boe_proj = store
            .remember_remote_project("devbox-boe-id", &proj_json)
            .unwrap();
        let ai_proj = store
            .remember_remote_project("devbox-ai-id", &proj_json)
            .unwrap();
        assert_ne!(mac_proj.key, boe_proj.key);
        assert_ne!(boe_proj.key, ai_proj.key);
        assert_ne!(mac_proj.key, ai_proj.key);

        let mac_sessions = vec![
            json!({"id": "shared-root", "title": "Mac Root", "harness": "codex", "model": "codex:gpt-5.4", "status": "idle", "updatedAt": 1000}),
        ];
        let boe_sessions = vec![
            json!({"id": "shared-root", "title": "BOE Root", "harness": "codex", "model": "codex:gpt-5.4", "status": "idle", "updatedAt": 2000}),
            json!({"id": "boe-child-only", "title": "BOE Child", "harness": "codex", "model": "codex:gpt-5.4", "status": "running", "updatedAt": 2100, "paseoParentAgentId": "shared-root"}),
            json!({"id": "orphan-foreign-child", "title": "Orphan Child", "harness": "codex", "model": "codex:gpt-5.4", "status": "idle", "updatedAt": 2200, "paseoParentAgentId": "mac-only-parent"}),
        ];
        let mac_only = vec![
            json!({"id": "shared-root", "title": "Mac Root", "harness": "codex", "model": "codex:gpt-5.4", "status": "idle", "updatedAt": 1000}),
            json!({"id": "mac-only-parent", "title": "Mac Only Parent", "harness": "codex", "model": "codex:gpt-5.4", "status": "idle", "updatedAt": 1500}),
        ];
        let _ = mac_sessions;
        let mac_reg = store.sync_remote_summaries(&mac_proj, &mac_only).unwrap();
        let boe_reg = store
            .sync_remote_summaries(&boe_proj, &boe_sessions)
            .unwrap();
        assert_ne!(mac_reg[0].summary.id, boe_reg[0].summary.id);
        let links = store.started_links().unwrap();
        // boe-child-only links ONLY to BOE's shared-root, NEVER Mac's shared-root.
        assert_eq!(
            links.get(&boe_reg[1].summary.id),
            Some(&boe_reg[0].summary.id)
        );
        // orphan-foreign-child does NOT cross-link to mac-only-parent on local-mac-id!
        assert_eq!(links.get(&boe_reg[2].summary.id), None);
    }

    #[test]
    fn multihost_discovery_and_connect_remote_host_roundtrip() {
        const TOKEN: &str = "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";
        let dir = std::env::temp_dir().join(format!(
            "paseo-multihost-rust-test-{}",
            monocode_core::mono::new_id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("paseo-host-token"), TOKEN).unwrap();

        let boe_server = MockLoopbackHost::spawn(
            "srv-devbox-boe",
            TOKEN,
            vec!["sessions", "paseo.workspaces", "paseo.usage"],
            |method, _params| match method {
                "projects.list" => Ok(json!([
                    {"id": "paseo:/home/user/ai_scope", "cwd": "/home/user/ai_scope", "name": "ai_scope", "exists": true, "active": true}
                ])),
                "sessions.list" => Ok(json!([
                    {"id": "boe-root", "title": "Devbox BOE Task", "harness": "codex", "model": "codex:gpt-5.4", "status": "idle", "updatedAt": 5000, "paseoBucket": "done", "paseoWorkspaceAnchor": true},
                    {"id": "boe-sub", "title": "Devbox Subagent", "harness": "codex", "model": "codex:gpt-5.4", "status": "running", "updatedAt": 5100, "paseoBucket": "running", "paseoParentAgentId": "boe-root"}
                ])),
                _ => Err(format!("unexpected remote method {method}")),
            },
        );
        let boe_endpoint = boe_server.endpoint.clone();

        let local_server = MockLoopbackHost::spawn(
            "local-mac-id",
            TOKEN,
            vec!["sessions", "paseo.hosts", "files.read"],
            move |method, params| match method {
                "paseo.hosts.list" => Ok(json!({
                    "supported": true,
                    "self": {"serverId": "local-mac-id", "label": "Local Mac"},
                    "hosts": [
                        {"registrationId": "r1", "serverId": "srv-devbox-boe", "label": "devbox-boe", "enabled": true, "status": "online", "targetDescription": "ssh://devbox-boe", "connected": false},
                        {"registrationId": "r2", "serverId": "srv-devbox-ai", "label": "devbox-ai", "enabled": true, "status": "offline", "targetDescription": "ssh://devbox-ai", "connected": false}
                    ]
                })),
                "paseo.hosts.connect" => match params["serverId"].as_str() {
                    Some("srv-devbox-boe") => Ok(json!({
                        "serverId": "srv-devbox-boe",
                        "label": "devbox-boe",
                        "status": "online",
                        "endpoint": boe_endpoint,
                        "isRemote": true,
                    })),
                    Some("srv-non-loopback") => Ok(json!({
                        "serverId": "srv-non-loopback",
                        "label": "evil",
                        "status": "online",
                        "endpoint": "http://192.0.2.10:9999",
                        "isRemote": true,
                    })),
                    _ => Err("Host offline".into()),
                },
                _ => Err(format!("unexpected local method {method}")),
            },
        );

        let connections = Connections::new(&dir);
        connections
            .connect("Paseo".into(), local_server.endpoint.clone(), TOKEN.into())
            .unwrap();
        let store = Arc::new(Mutex::new(Store::open(&dir.join("monocode.db")).unwrap()));

        let hosts = list_hosts(&connections, "local-mac-id").unwrap();
        assert_eq!(hosts.len(), 3);
        assert_eq!(hosts[0].label, "Local Mac");
        assert!(!hosts[0].is_remote);
        assert_eq!(hosts[1].label, "devbox-boe");
        assert_eq!(hosts[1].status, "online");
        assert_eq!(hosts[2].label, "devbox-ai");
        assert_eq!(hosts[2].status, "offline");

        assert!(
            connect_remote_host(
                &dir,
                &store,
                &connections,
                "local-mac-id",
                "local-mac-id",
                &ProjectSelection::default(),
            )
            .is_err()
        );
        assert!(
            connect_remote_host(
                &dir,
                &store,
                &connections,
                "local-mac-id",
                "srv-non-loopback",
                &ProjectSelection::default(),
            )
            .is_err()
        );

        let connected = connect_remote_host(
            &dir,
            &store,
            &connections,
            "local-mac-id",
            "srv-devbox-boe",
            &ProjectSelection::default(),
        )
        .unwrap();
        assert_eq!(connected.server_id, "srv-devbox-boe");
        assert_eq!(
            connected.projects,
            vec!["remote://srv-devbox-boe/home/user/ai_scope"]
        );
        assert_eq!(
            connected
                .labels
                .get("remote://srv-devbox-boe/home/user/ai_scope"),
            Some(&"ai_scope · devbox-boe".to_owned())
        );
        assert_eq!(connected.summaries.len(), 2);
        let root_id = connected.summaries[0].0.id;
        let sub_id = connected.summaries[1].0.id;
        assert_eq!(connected.started_by.get(&sub_id), Some(&root_id));
        assert!(
            connections
                .machine_for_environment("srv-devbox-boe")
                .is_ok()
        );

        drop(boe_server);
        drop(local_server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui::test]
    fn paseo_shell_host_switcher_connects_devbox_and_keeps_local_authority_pinned(
        cx: &mut TestAppContext,
    ) {
        const TOKEN: &str = "DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD";
        cx.update(configure);
        let dir = std::env::temp_dir().join(format!(
            "paseo-shell-multihost-test-{}",
            monocode_core::mono::new_id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("paseo-host-token"), TOKEN).unwrap();

        let boe_server = MockLoopbackHost::spawn(
            "srv-devbox-boe",
            TOKEN,
            vec!["sessions", "paseo.workspaces"],
            |method, _params| match method {
                "projects.list" => Ok(json!([
                    {"id": "paseo:/home/bianrenping.juns/ai_scope", "cwd": "/home/bianrenping.juns/ai_scope", "name": "ai_scope", "exists": true, "active": true}
                ])),
                "sessions.list" => Ok(json!([
                    {"id": "boe-1", "title": "BOE Remote Session", "harness": "codex", "model": "codex:gpt-5.4", "status": "idle", "updatedAt": 9000, "paseoBucket": "done", "paseoWorkspaceAnchor": true}
                ])),
                _ => Err(format!("unexpected remote method {method}")),
            },
        );
        let boe_endpoint = boe_server.endpoint.clone();
        let initial_hosts = vec![
            RemoteHostInfo {
                registration_id: "self".into(),
                server_id: Some("paseo-fixture".into()),
                label: "Local Mac".into(),
                enabled: true,
                status: "online".into(),
                target_description: "127.0.0.1".into(),
                is_remote: false,
                connected: true,
            },
            RemoteHostInfo {
                registration_id: "r-boe".into(),
                server_id: Some("srv-devbox-boe".into()),
                label: "devbox-boe".into(),
                enabled: true,
                status: "online".into(),
                target_description: "ssh://devbox-boe".into(),
                is_remote: true,
                connected: false,
            },
            RemoteHostInfo {
                registration_id: "r-ai".into(),
                server_id: Some("srv-devbox-ai".into()),
                label: "devbox-ai".into(),
                enabled: true,
                status: "offline".into(),
                target_description: "ssh://devbox-ai".into(),
                is_remote: true,
                connected: false,
            },
        ];

        let local_server = MockLoopbackHost::spawn(
            "paseo-fixture",
            TOKEN,
            vec!["sessions", "paseo.hosts", "files.read"],
            move |method, params| match method {
                "paseo.hosts.list" => Ok(json!({
                    "supported": true,
                    "self": {"serverId": "paseo-fixture", "label": "Local Mac"},
                    "hosts": [
                        {"registrationId": "r-boe", "serverId": "srv-devbox-boe", "label": "devbox-boe", "enabled": true, "status": "online", "targetDescription": "ssh://devbox-boe", "connected": false},
                        {"registrationId": "r-ai", "serverId": "srv-devbox-ai", "label": "devbox-ai", "enabled": true, "status": "offline", "targetDescription": "ssh://devbox-ai", "connected": false}
                    ]
                })),
                "paseo.hosts.connect" if params["serverId"] == "srv-devbox-boe" => Ok(json!({
                    "serverId": "srv-devbox-boe",
                    "label": "devbox-boe",
                    "status": "online",
                    "endpoint": boe_endpoint,
                    "isRemote": true,
                })),
                "sessions.list" => Ok(json!([])),
                _ => Err(format!("unexpected local method {method}")),
            },
        );

        let connections = Arc::new(Connections::new(&dir));
        connections
            .connect("Paseo".into(), local_server.endpoint.clone(), TOKEN.into())
            .unwrap();

        let window = cx.add_window(|_, cx| crate::shell::paseo_shell_fixture(cx));
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();

        let local_endpoint = local_server.endpoint.clone();
        let dir_for_shell = dir.clone();
        window
            .update(visual, |shell, w, cx| {
                shell.paseo_fixture_wire_multihost(
                    dir_for_shell,
                    connections.clone(),
                    "paseo-fixture",
                    &local_endpoint,
                    initial_hosts,
                );
                // Offline devbox-ai is disabled in the Header menu and must not switch project.
                shell.paseo_fixture_pick_header_action("paseo-host:srv-devbox-ai", w, cx);
                assert_eq!(shell.workspace.project, "remote://paseo-fixture/fixture");

                // Online devbox-boe connects on demand in background and switches project.
                shell.paseo_fixture_pick_header_action("paseo-host:srv-devbox-boe", w, cx);
            })
            .unwrap();
        visual.run_until_parked();

        window
            .update(visual, |shell, w, cx| {
                assert_eq!(
                    shell.workspace.project,
                    "remote://srv-devbox-boe/home/bianrenping.juns/ai_scope"
                );
                assert_eq!(
                    shell.paseo_fixture_current_project_label(),
                    "ai_scope · devbox-boe"
                );
                assert_eq!(
                    shell.paseo_fixture_session_titles_for_project(
                        "remote://srv-devbox-boe/home/bianrenping.juns/ai_scope"
                    ),
                    vec!["BOE Remote Session".to_owned()]
                );
                shell.paseo_fixture_assert_remote_host_isolated(
                    "paseo-fixture",
                    "remote://srv-devbox-boe/home/bianrenping.juns/ai_scope/README.md:1:1",
                    w,
                    cx,
                );

                // Switching back to Local Mac restores the local Paseo project immediately.
                shell.paseo_fixture_pick_header_action("paseo-host:paseo-fixture", w, cx);
            })
            .unwrap();
        visual.run_until_parked();

        window
            .update(visual, |shell, _, _| {
                assert_eq!(shell.workspace.project, "remote://paseo-fixture/fixture");
            })
            .unwrap();

        drop(boe_server);
        drop(local_server);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
