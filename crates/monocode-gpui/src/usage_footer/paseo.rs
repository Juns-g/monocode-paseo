//! Read-only Codex + Antigravity quota relayed by the Paseo daemon (`paseo.usage`).
//! Credentials and provider CLIs stay inside the daemon; nothing here reads them.
use super::*;
use monocode_core::usage::duration;
use serde::Deserialize;
use serde_json::{Value, json};

/// The only sources shown, in display order.
const ALLOWED: [&str; 2] = ["codex", "antigravity"];
const PASSIVE_INTERVAL: u64 = 180;
const RESET_INTERVAL: u64 = 60;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct PaseoUsageWindow {
    pub(super) id: String,
    pub(super) label: String,
    pub(super) short_label: Option<String>,
    pub(super) summary: bool,
    pub(super) used_pct: Option<f64>,
    pub(super) remaining_pct: Option<f64>,
    pub(super) resets_at: Option<i64>,
    pub(super) duration_seconds: Option<f64>,
    pub(super) runs_out_at: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct PaseoUsageBalance {
    pub(super) label: String,
    pub(super) used: Option<f64>,
    pub(super) remaining: Option<f64>,
    pub(super) limit: Option<f64>,
    pub(super) unit: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct PaseoUsageSource {
    pub(super) id: String,
    pub(super) source_id: String,
    pub(super) source_label: String,
    pub(super) account_label: Option<String>,
    pub(super) plan_label: Option<String>,
    pub(super) status: String,
    pub(super) retained: bool,
    pub(super) error: Option<String>,
    pub(super) observed_at: Option<i64>,
    pub(super) windows: Vec<PaseoUsageWindow>,
    pub(super) balances: Vec<PaseoUsageBalance>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct PaseoUsageResponse {
    pub(super) sources: Vec<PaseoUsageSource>,
}

impl PaseoUsageResponse {
    /// Defensive second filter: only Codex and Antigravity ever leave this function.
    pub(super) fn parse(value: Value) -> Result<Vec<PaseoUsageSource>, String> {
        let mut sources = serde_json::from_value::<Self>(value)
            .map_err(|_| "Unexpected usage response from Paseo".to_owned())?
            .sources;
        sources.retain(|s| ALLOWED.contains(&s.source_id.as_str()));
        sources.sort_by(|a, b| {
            rank(&a.source_id)
                .cmp(&rank(&b.source_id))
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(sources)
    }
}

fn rank(source_id: &str) -> usize {
    ALLOWED
        .iter()
        .position(|id| *id == source_id)
        .unwrap_or(ALLOWED.len())
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum UsagePace {
    Unknown,
    Reached,
    RunsOut(i64),
    Tight,
    OnTrack,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum PaceTone {
    Ok,
    Warning,
    Danger,
}

/// Port of Paseo's `packages/app/src/usage/pace.ts`: the window's average pace as of the reading.
pub(super) fn pace(w: &PaseoUsageWindow, observed_at: Option<i64>, now: i64) -> UsagePace {
    let Some(observed) = observed_at else {
        return UsagePace::Unknown;
    };
    if w.resets_at.is_some_and(|reset| reset <= now) {
        return UsagePace::Unknown;
    }
    let used = w.used_pct.or(w.remaining_pct.map(|r| 100. - r));
    if used.is_some_and(|u| u >= 100.) {
        return UsagePace::Reached;
    }
    if let Some(at) = w.runs_out_at
        && w.resets_at.is_none_or(|reset| at < reset)
    {
        return UsagePace::RunsOut(at);
    }
    let (Some(reset), Some(used)) = (w.resets_at, used) else {
        return UsagePace::Unknown;
    };
    let duration_ms = w.duration_seconds.unwrap_or(0.) * 1_000.;
    let elapsed = observed as f64 - (reset as f64 - duration_ms);
    let minimum = (duration_ms * 0.01).min(3_600_000.).max(60_000.);
    if !used.is_finite()
        || used < 0.
        || !elapsed.is_finite()
        || observed >= reset
        || elapsed < minimum
    {
        return UsagePace::Unknown;
    }
    let projected = used / elapsed * duration_ms;
    if projected > 100. {
        UsagePace::RunsOut((observed as f64 + (100. - used) / used * elapsed) as i64)
    } else if projected >= 90. {
        UsagePace::Tight
    } else {
        UsagePace::OnTrack
    }
}

pub(super) fn pace_label(pace: UsagePace, now: i64) -> Option<(String, PaceTone)> {
    match pace {
        UsagePace::Unknown => None,
        UsagePace::Reached => Some(("Limit reached".into(), PaceTone::Danger)),
        UsagePace::RunsOut(at) if at <= now => Some(("Limit reached".into(), PaceTone::Danger)),
        UsagePace::RunsOut(at) => Some((
            format!("Runs out in {}", duration(at - now)),
            if at - now < 3_600_000 {
                PaceTone::Danger
            } else {
                PaceTone::Warning
            },
        )),
        UsagePace::Tight => Some(("Tight pace".into(), PaceTone::Warning)),
        UsagePace::OnTrack => Some(("On track".into(), PaceTone::Ok)),
    }
}

fn tone_color(tone: PaceTone) -> gpui::Hsla {
    match tone {
        PaceTone::Ok => rgb(0x4ade80).into(),
        PaceTone::Warning => rgb(0xfbbf24).into(),
        PaceTone::Danger => rgb(0xf87171).into(),
    }
}

/// Up to two windows for the compact chip: the daemon's summary picks, else the first live ones.
pub(super) fn summary_windows(source: &PaseoUsageSource) -> Vec<&PaseoUsageWindow> {
    let live = |w: &&PaseoUsageWindow| w.used_pct.is_some();
    let picked: Vec<_> = source
        .windows
        .iter()
        .filter(|w| w.summary)
        .filter(live)
        .take(2)
        .collect();
    if picked.is_empty() {
        source.windows.iter().filter(live).take(2).collect()
    } else {
        picked
    }
}

/// "5h", "wk", "30d": names the window the countdown already conveys.
fn is_duration_token(label: &str) -> bool {
    let digits = label.trim_start_matches(|c: char| c.is_ascii_digit());
    matches!(digits, "h" | "d" | "m" | "w" | "wk" | "mo")
}

fn compact(text: String) -> String {
    text.replace(' ', "")
}

#[derive(Clone, Default)]
struct PaseoHostUsageCache {
    sources: Vec<PaseoUsageSource>,
    updated_at: Option<i64>,
    attempted: Option<Instant>,
    error: Option<String>,
}

fn paseo_host_key(project: &str) -> String {
    monocode_core::remote::parse_remote_path(project)
        .map(|p| p.environment_id)
        .unwrap_or_default()
}

#[derive(Default)]
pub(super) struct PaseoUsageState {
    pub(super) sources: Vec<PaseoUsageSource>,
    pub(super) updated_at: Option<i64>,
    pub(super) fetching: bool,
    pub(super) attempted: Option<Instant>,
    pub(super) error: Option<String>,
    active_host: Option<String>,
    by_host: std::collections::HashMap<String, PaseoHostUsageCache>,
    request_gen: u64,
    #[cfg(test)]
    pub(super) test_client: Option<(std::sync::Arc<monocode_core::remote::Connections>, String)>,
}

fn number(value: f64) -> String {
    if value.fract() == 0. {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
    }
}

impl PaseoUsageBalance {
    fn value(&self) -> String {
        let amount = |v: f64| match self.unit.as_str() {
            "usd" => format!("${v:.2}"),
            unit => format!("{} {unit}", number(v)),
        };
        match (self.remaining, self.used, self.limit) {
            (Some(remaining), _, _) => amount(remaining),
            (None, Some(used), Some(limit)) => format!("{} / {}", amount(used), amount(limit)),
            (None, Some(used), None) => format!("{} used", amount(used)),
            _ => "—".into(),
        }
    }
}

impl UsageFooter {
    /// Percent to show for a window: remaining by default, used on request.
    fn paseo_percent(&self, w: &PaseoUsageWindow) -> Option<f64> {
        let used = w
            .used_pct
            .or(w.remaining_pct.map(|r| 100. - r))?
            .clamp(0., 100.);
        let remaining = w.remaining_pct.map_or(100. - used, |r| r.clamp(0., 100.));
        Some(if self.show_remaining { remaining } else { used })
    }

    fn paseo_used(w: &PaseoUsageWindow) -> Option<f64> {
        w.used_pct
            .or(w.remaining_pct.map(|r| 100. - r))
            .map(|u| u.clamp(0., 100.))
    }

    /// Swap cached Paseo usage state when switching between Paseo hosts (`serverId`).
    /// Bumps `request_gen` so any slow in-flight fetch from the previous host is discarded.
    pub(super) fn switch_paseo_host(&mut self, project: &str, cx: &mut Context<Self>) {
        let next = paseo_host_key(project);
        let state = &mut self.paseo;
        if state.active_host.as_deref() == Some(next.as_str()) {
            return;
        }
        if let Some(prev) = state.active_host.take() {
            // If a fetch was in flight when switching away, its response will be
            // discarded by `request_gen`, so clear `attempted` so returning to this
            // host re-fetches immediately instead of waiting out the 180 s throttle.
            let prev_attempted = if state.fetching {
                state.attempted.take();
                None
            } else {
                state.attempted.take()
            };
            state.by_host.insert(
                prev,
                PaseoHostUsageCache {
                    sources: std::mem::take(&mut state.sources),
                    updated_at: state.updated_at.take(),
                    attempted: prev_attempted,
                    error: state.error.take(),
                },
            );
        } else {
            state.sources.clear();
            state.updated_at = None;
            state.attempted = None;
            state.error = None;
        }
        if let Some(cached) = state.by_host.remove(&next) {
            state.sources = cached.sources;
            state.updated_at = cached.updated_at;
            state.attempted = cached.attempted;
            state.error = cached.error;
        }
        state.active_host = Some(next);
        state.fetching = false;
        state.request_gen = state.request_gen.wrapping_add(1);
        cx.notify();
    }

    pub(super) fn refresh_paseo(&mut self, force: bool, cx: &mut Context<Self>) {
        #[cfg(test)]
        let injected = self.paseo.test_client.clone();
        #[cfg(not(test))]
        let injected: Option<(std::sync::Arc<monocode_core::remote::Connections>, String)> = None;
        if cfg!(test) && injected.is_none() {
            return;
        }
        let host_key = paseo_host_key(&self.project);
        if self.paseo.active_host.as_deref() != Some(host_key.as_str()) {
            self.switch_paseo_host(&self.project.clone(), cx);
        }
        let now = usage::now_ms();
        let state = &mut self.paseo;
        let reset = state.sources.iter().flat_map(|s| &s.windows).any(|w| {
            w.resets_at
                .is_some_and(|at| at > state.updated_at.unwrap_or(0) && at <= now)
        });
        let interval = if reset {
            RESET_INTERVAL
        } else {
            PASSIVE_INTERVAL
        };
        if state.fetching
            || (!force
                && state
                    .attempted
                    .is_some_and(|at| at.elapsed() < Duration::from_secs(interval)))
        {
            return;
        }
        state.attempted = Some(Instant::now());
        let resolved = injected.map(Ok).unwrap_or_else(|| resolve(&self.project));
        let (client, machine) = match resolved {
            Ok(value) => value,
            Err(error) => {
                state.sources.clear();
                state.updated_at = None;
                state.error = Some(error);
                cx.notify();
                return;
            }
        };
        state.fetching = true;
        state.request_gen = state.request_gen.wrapping_add(1);
        let expected_gen = state.request_gen;
        let expected_host = host_key;
        let work = cx.background_executor().spawn(async move {
            client
                .request(&machine, "paseo.usage", json!({"forceRefresh": force}))
                .and_then(PaseoUsageResponse::parse)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.paseo;
                if state.request_gen != expected_gen
                    || state.active_host.as_deref() != Some(expected_host.as_str())
                {
                    return;
                }
                state.fetching = false;
                match result {
                    Ok(sources) => {
                        state.sources = sources;
                        state.updated_at = Some(usage::now_ms());
                        state.error = None;
                    }
                    // A transient failure keeps the last snapshot on screen.
                    Err(error) => state.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn enable_paseo_for_test(
        &mut self,
        client: std::sync::Arc<monocode_core::remote::Connections>,
        machine: String,
        cx: &mut Context<Self>,
    ) {
        self.paseo_mode = true;
        self.paseo.test_client = Some((client, machine));
        self.refresh(false, cx);
        cx.notify();
    }

    fn source_chip(&self, source: &PaseoUsageSource, now: i64) -> gpui::Stateful<gpui::Div> {
        let windows = summary_windows(source);
        let worst = windows
            .iter()
            .filter_map(|w| Self::paseo_used(w))
            .reduce(f64::max);
        let multiple = windows.len() > 1;
        let text = if windows.is_empty() {
            match (&source.error, self.paseo.fetching) {
                (_, true) => "···".to_owned(),
                (Some(_), _) => "unavailable".into(),
                _ => "no data".into(),
            }
        } else {
            windows
                .iter()
                .filter_map(|w| {
                    let percent = self.paseo_percent(w)?;
                    let short = w
                        .short_label
                        .as_deref()
                        .filter(|s| !s.is_empty() && !is_duration_token(s));
                    let countdown = w
                        .resets_at
                        .filter(|at| *at > now)
                        .map(|at| compact(duration(at - now)));
                    let mut parts = Vec::new();
                    parts.extend(short.map(str::to_owned));
                    parts.push(format!("{percent:.0}%"));
                    // Windows without a reset time still need a name to tell them apart.
                    match (countdown, multiple, &w.short_label) {
                        (Some(c), _, _) => parts.push(c),
                        (None, true, Some(s)) if short.is_none() && !s.is_empty() => {
                            parts.insert(0, s.clone())
                        }
                        _ => {}
                    }
                    Some(parts.join(" "))
                })
                .collect::<Vec<_>>()
                .join(" · ")
        };
        div()
            .id(gpui::SharedString::from(format!(
                "usage-source-{}",
                source.id
            )))
            .h(px(20.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(provider_icon(&source.source_id, 12.))
            .when_some(worst, |d, percent| {
                d.child(
                    div()
                        .w(px(32.))
                        .h(px(4.))
                        .rounded_full()
                        .bg(content(0.1))
                        .overflow_hidden()
                        .child(
                            div()
                                .h_full()
                                .w(gpui::relative(percent as f32 / 100.))
                                .rounded_full()
                                .bg(usage_color(percent)),
                        ),
                )
            })
            .child(div().truncate().child(text))
    }

    pub(super) fn render_paseo(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let now = usage::now_ms();
        let classic = self.classic;
        let fetching = self.paseo.fetching;
        let description = if self.paseo.sources.is_empty() {
            self.paseo
                .error
                .clone()
                .unwrap_or_else(|| "Loading usage".into())
        } else {
            self.paseo
                .sources
                .iter()
                .map(|s| {
                    let windows = summary_windows(s)
                        .iter()
                        .filter_map(|w| {
                            let percent = self.paseo_percent(w)?;
                            Some(format!(
                                "{:.0}% {}",
                                percent,
                                if self.show_remaining {
                                    "remaining"
                                } else {
                                    "used"
                                }
                            ))
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{}: {windows}", s.source_label)
                })
                .collect::<Vec<_>>()
                .join(" · ")
        };
        let chips: Vec<gpui::AnyElement> = self
            .paseo
            .sources
            .iter()
            .map(|s| self.source_chip(s, now).into_any_element())
            .collect();
        let placeholder = if chips.is_empty() {
            Some(if fetching || self.paseo.error.is_none() {
                "···"
            } else {
                "usage unavailable"
            })
        } else {
            None
        };
        let weak = cx.weak_entity();
        div()
            .id("usage-footer")
            .debug_selector(|| "usage-footer".into())
            .role(Role::Group)
            .aria_label("Provider usage")
            .h(px(if classic { 28. } else { 30. }))
            .w_full()
            .flex_shrink_0()
            .when(!classic, |d| d.pb(px(2.)))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(11.))
            .text_color(content(0.55))
            .child(
                div()
                    .id("usage-provider-chip")
                    .role(Role::Button)
                    .aria_label("Codex and Antigravity usage details")
                    .aria_description(description)
                    .track_focus(&self.trigger)
                    .key_context("UsageChip")
                    .tab_index(0)
                    .relative()
                    .h(px(20.))
                    .min_w_0()
                    .map(|d| {
                        if classic {
                            d.mx(px(-4.)).px(px(4.)).rounded_md()
                        } else {
                            d.px(px(8.)).rounded_full()
                        }
                    })
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .overflow_hidden()
                    .cursor_pointer()
                    .hover(|s| s.bg(content(0.1)).text_color(content(1.)))
                    .child(self.anchor.probe())
                    .children(chips)
                    .children(placeholder.map(|text| div().child(text)))
                    .on_click(cx.listener(|this, _, window, cx| this.toggle(window, cx)))
                    .on_action(
                        cx.listener(|this, _: &ToggleUsage, window, cx| this.toggle(window, cx)),
                    ),
            )
            .child(
                div()
                    .id("refresh-usage")
                    .role(Role::Button)
                    .aria_label("Refresh usage")
                    .track_focus(&self.refresh_focus)
                    .tab_index(0)
                    .key_context("UsageRefresh")
                    .map(|d| {
                        if classic {
                            d.size(px(18.)).rounded_md()
                        } else {
                            d.size(px(20.)).ml(px(-4.)).rounded_full()
                        }
                    })
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .opacity(if fetching { 0.5 } else { 1. })
                    .when(!fetching, |d| d.hover(|s| s.bg(content(0.1))))
                    .child(icon("refresh", 12.))
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(true, cx)))
                    .on_action(cx.listener(|this, _: &RefreshUsage, _, cx| this.refresh(true, cx))),
            )
            .when(self.open, |d| {
                d.child(
                    popover("usage-details", Anchor::Slot(self.anchor.clone()))
                        .side(Side::Top)
                        .gap(7.)
                        .width(320.)
                        .max_height(460.)
                        .padding(10.)
                        .on_dismiss(move |window, cx| {
                            let _ = weak.update(cx, |this, cx| this.dismiss(window, cx));
                        })
                        .child(self.paseo_details(now, cx)),
                )
            })
            .into_any_element()
    }

    fn notice(text: String) -> gpui::Div {
        div()
            .mb_2()
            .rounded_lg()
            .px(px(10.))
            .py_2()
            .bg(gpui::rgba(0xfbbf241a))
            .text_size(px(10.))
            .line_height(px(16.))
            .text_color(rgb(0xfcd34d))
            .child(text)
    }

    pub(super) fn paseo_details(&self, now: i64, cx: &mut Context<Self>) -> gpui::AnyElement {
        let state = &self.paseo;
        let updated = match state.updated_at {
            Some(at) => match (now - at).max(0) / 60_000 {
                0 => "Updated just now".to_owned(),
                minutes @ 1..60 => format!("Updated {minutes}m ago"),
                minutes => format!("Updated {}h ago", minutes / 60),
            },
            None => "Rate-limit details".into(),
        };
        let loading = state.fetching || (state.sources.is_empty() && state.error.is_none());
        let empty_text = if loading {
            Some("Loading usage…")
        } else if state.sources.is_empty() && state.error.is_some() {
            Some("Usage unavailable")
        } else if state.sources.is_empty() {
            Some("No Codex or Antigravity usage reported by Paseo.")
        } else {
            None
        };
        div()
            .id("usage-details-content")
            .role(Role::Dialog)
            .aria_label("Paseo usage details")
            .track_focus(&self.details)
            .key_context("UsageDetails")
            .on_action(cx.listener(|this, _: &DismissUsage, window, cx| {
                this.dismiss(window, cx);
            }))
            .flex()
            .flex_col()
            .text_size(px(11.))
            .text_color(content(1.))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .px_1()
                    .pt(px(2.))
                    .pb(px(10.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .line_height(px(16.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("Paseo usage"),
                            )
                            .child(
                                div()
                                    .mt(px(2.))
                                    .text_size(px(10.))
                                    .line_height(px(16.))
                                    .text_color(content(0.4))
                                    .child(updated),
                            ),
                    )
                    .when(state.fetching, |d| {
                        d.child(
                            div()
                                .mt(px(2.))
                                .flex()
                                .items_center()
                                .gap_1()
                                .text_size(px(10.))
                                .text_color(content(0.4))
                                .child(icon("refresh", 10.).text_color(content(0.4)))
                                .child("Updating"),
                        )
                    }),
            )
            .when(state.error.is_some() && !state.sources.is_empty(), |d| {
                d.child(Self::notice(
                    "Couldn’t refresh. Showing the last available snapshot.".into(),
                ))
            })
            .children(empty_text.map(|text| {
                div()
                    .rounded_lg()
                    .bg(content(0.04))
                    .border_1()
                    .border_color(content(0.06))
                    .px_3()
                    .py_4()
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(content(0.65))
                            .child(text),
                    )
                    .children(state.error.clone().filter(|_| !loading).map(|error| {
                        div()
                            .mt_1()
                            .max_w(px(260.))
                            .text_center()
                            .text_size(px(10.))
                            .line_height(px(16.))
                            .text_color(content(0.4))
                            .child(error)
                    }))
            }))
            .children(
                state
                    .sources
                    .iter()
                    .map(|source| self.source_section(source, now)),
            )
            .into_any()
    }

    fn source_section(&self, source: &PaseoUsageSource, now: i64) -> gpui::Stateful<gpui::Div> {
        let title = match &source.account_label {
            Some(account) => format!("{} · {account}", source.source_label),
            None => source.source_label.clone(),
        };
        div()
            .id(gpui::SharedString::from(format!(
                "usage-section-{}",
                source.id
            )))
            .flex()
            .flex_col()
            .gap(px(6.))
            .mb_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px_1()
                    .child(provider_icon(&source.source_id, 14.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .child(title),
                    )
                    .children(source.plan_label.clone().map(|plan| {
                        div()
                            .flex_shrink_0()
                            .rounded_full()
                            .px(px(8.))
                            .bg(content(0.08))
                            .text_size(px(10.))
                            .text_color(content(0.55))
                            .child(plan)
                    })),
            )
            .when(source.retained, |d| {
                d.child(Self::notice(format!(
                    "Couldn’t refresh. Showing the last available snapshot.{}",
                    source
                        .error
                        .as_ref()
                        .map(|e| format!(" ({e})"))
                        .unwrap_or_default()
                )))
            })
            .when(source.windows.is_empty(), |d| {
                d.child(
                    div()
                        .rounded_lg()
                        .bg(content(0.045))
                        .border_1()
                        .border_color(content(0.06))
                        .px_3()
                        .py(px(10.))
                        .text_color(content(0.55))
                        .child(
                            source
                                .error
                                .clone()
                                .unwrap_or_else(|| "No usage windows reported.".into()),
                        ),
                )
            })
            .children(
                source
                    .windows
                    .iter()
                    .map(|w| self.paseo_window_card(source, w, now)),
            )
            .children(source.balances.iter().map(|b| {
                div()
                    .rounded_lg()
                    .bg(content(0.045))
                    .border_1()
                    .border_color(content(0.06))
                    .px_3()
                    .py(px(10.))
                    .flex()
                    .justify_between()
                    .items_baseline()
                    .gap_3()
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(content(0.65))
                            .child(b.label.clone()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .font_weight(FontWeight::MEDIUM)
                            .child(b.value()),
                    )
            }))
    }

    fn paseo_window_card(
        &self,
        source: &PaseoUsageSource,
        w: &PaseoUsageWindow,
        now: i64,
    ) -> gpui::Div {
        let shown = self.paseo_percent(w);
        let used = Self::paseo_used(w);
        let other = shown.map(|s| 100. - s);
        let reset = match w.resets_at {
            Some(at) if at <= now => Some("Resets now".to_owned()),
            Some(at) => Some(format!("Resets in {}", duration(at - now))),
            None => None,
        };
        // A retained reading is old; extrapolating its pace would mislead.
        let hint = if source.retained {
            None
        } else {
            pace_label(pace(w, source.observed_at, now), now)
        };
        div()
            .rounded_lg()
            .bg(content(0.045))
            .border_1()
            .border_color(content(0.06))
            .px_3()
            .py(px(10.))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_baseline()
                    .gap_3()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(content(0.65))
                            .child(w.label.clone()),
                    )
                    .child(div().flex_shrink_0().font_weight(FontWeight::MEDIUM).child(
                        match shown {
                            Some(p) => format!(
                                "{p:.0}% {}",
                                if self.show_remaining {
                                    "remaining"
                                } else {
                                    "used"
                                }
                            ),
                            None => "Disabled".into(),
                        },
                    )),
            )
            .child(
                div()
                    .mt_2()
                    .h(px(6.))
                    .w_full()
                    .rounded_full()
                    .bg(content(0.1))
                    .overflow_hidden()
                    .child(
                        div()
                            .h_full()
                            .w(gpui::relative(shown.unwrap_or(0.) as f32 / 100.))
                            .rounded_full()
                            .bg(usage_color(used.unwrap_or(0.))),
                    ),
            )
            .child(
                div()
                    .mt(px(6.))
                    .flex()
                    .justify_between()
                    .gap_3()
                    .text_size(px(10.))
                    .line_height(px(16.))
                    .text_color(content(0.4))
                    .child(
                        other
                            .map(|p| {
                                format!(
                                    "{p:.0}% {}",
                                    if self.show_remaining {
                                        "used"
                                    } else {
                                        "remaining"
                                    }
                                )
                            })
                            .unwrap_or_default(),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(6.))
                            .min_w_0()
                            .children(reset.map(|r| div().truncate().child(r)))
                            .children(hint.map(|(text, tone)| {
                                div()
                                    .flex_shrink_0()
                                    .text_color(tone_color(tone))
                                    .child(text)
                            })),
                    ),
            )
    }
}

fn resolve(
    project: &str,
) -> Result<(std::sync::Arc<monocode_core::remote::Connections>, String), String> {
    let connections = monocode_core::remote::default_connections()?;
    resolve_with(&connections, project).map(|machine| (connections, machine))
}

fn resolve_with(
    connections: &monocode_core::remote::Connections,
    project: &str,
) -> Result<String, String> {
    if let Some(remote) = monocode_core::remote::parse_remote_path(project) {
        return connections
            .machine_for_environment(&remote.environment_id)
            .map(|machine| machine.id)
            .map_err(|_| format!("Paseo host {} is not connected", remote.environment_id));
    }
    connections
        .list()
        .ok()
        .and_then(|m| m.into_iter().next())
        .map(|machine| machine.id)
        .ok_or_else(|| "No Paseo host is connected".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{runtime::remote::TestHost, usage_footer};
    use std::sync::{Arc, Mutex};

    const T0: i64 = 1_800_000_000_000;
    const HOUR: i64 = 3_600_000;

    fn window(used: f64, reset_in: i64, duration_hours: f64) -> PaseoUsageWindow {
        PaseoUsageWindow {
            id: "w".into(),
            label: "Session".into(),
            used_pct: Some(used),
            remaining_pct: Some(100. - used),
            resets_at: Some(T0 + reset_in),
            duration_seconds: Some(duration_hours * 3600.),
            ..Default::default()
        }
    }

    #[test]
    fn pace_ports_paseo_forecast() {
        // 5h window, 2h elapsed.
        let at = |used| pace(&window(used, 3 * HOUR, 5.), Some(T0), T0);
        assert_eq!(at(100.), UsagePace::Reached);
        assert_eq!(at(20.), UsagePace::OnTrack); // 20/2h*5h = 50
        assert_eq!(at(38.), UsagePace::Tight); // 95
        assert_eq!(at(40.), UsagePace::Tight); // 100 exactly is not > 100
        // 60% in 2h projects 150%; exhausted after 100/60*... = 2h * 40/60 = 80 min later.
        assert_eq!(at(60.), UsagePace::RunsOut(T0 + 80 * 60_000));
        // Daemon-supplied forecast wins while it lands before the reset.
        let mut w = window(10., 3 * HOUR, 5.);
        w.runs_out_at = Some(T0 + HOUR);
        assert_eq!(pace(&w, Some(T0), T0), UsagePace::RunsOut(T0 + HOUR));
        w.runs_out_at = Some(T0 + 4 * HOUR);
        assert_eq!(pace(&w, Some(T0), T0), UsagePace::OnTrack);
        // No reading, expired reset, or too little elapsed time is unknown.
        assert_eq!(
            pace(&window(20., 3 * HOUR, 5.), None, T0),
            UsagePace::Unknown
        );
        assert_eq!(pace(&window(20., -1, 5.), Some(T0), T0), UsagePace::Unknown);
        assert_eq!(
            pace(&window(20., 5 * HOUR - 30_000, 5.), Some(T0), T0),
            UsagePace::Unknown
        );
    }

    #[test]
    fn pace_labels_use_core_durations() {
        assert_eq!(pace_label(UsagePace::Unknown, T0), None);
        assert_eq!(
            pace_label(UsagePace::Reached, T0),
            Some(("Limit reached".into(), PaceTone::Danger))
        );
        assert_eq!(
            pace_label(UsagePace::RunsOut(T0 + 3 * HOUR + 12 * 60_000), T0),
            Some(("Runs out in 3h 12m".into(), PaceTone::Warning))
        );
        assert_eq!(
            pace_label(UsagePace::RunsOut(T0 + 45 * 60_000), T0),
            Some(("Runs out in 45m".into(), PaceTone::Danger))
        );
        assert_eq!(pace_label(UsagePace::Tight, T0).unwrap().0, "Tight pace");
        assert_eq!(pace_label(UsagePace::OnTrack, T0).unwrap().0, "On track");
    }

    #[test]
    fn parse_keeps_only_codex_and_antigravity_in_order() {
        let entry = |source: &str, id: &str| {
            json!({"id": id, "sourceId": source, "sourceLabel": source, "status": "available",
                   "windows": [{"id": "w", "label": "Session", "usedPct": 10.0, "summary": true}]})
        };
        let sources = PaseoUsageResponse::parse(json!({"fetchedAt": 1, "sources": [
            entry("grok", "g"), entry("antigravity", "b"), entry("claude", "c"),
            entry("codex", "z"), entry("codex", "a")]}))
        .unwrap();
        let ids: Vec<_> = sources.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["a", "z", "b"]);
        assert!(PaseoUsageResponse::parse(json!("nope")).is_err());
        // Summary windows prefer the daemon's picks, skip disabled buckets, cap at two.
        let mut source = sources[0].clone();
        source.windows = vec![
            PaseoUsageWindow {
                used_pct: None,
                summary: true,
                ..Default::default()
            },
            PaseoUsageWindow {
                id: "a".into(),
                used_pct: Some(1.),
                ..Default::default()
            },
            PaseoUsageWindow {
                id: "b".into(),
                used_pct: Some(2.),
                summary: true,
                ..Default::default()
            },
        ];
        let picked: Vec<_> = summary_windows(&source)
            .iter()
            .map(|w| w.id.as_str())
            .collect();
        assert_eq!(picked, ["b"]);
        source.windows[2].summary = false;
        let picked: Vec<_> = summary_windows(&source)
            .iter()
            .map(|w| w.id.as_str())
            .collect();
        assert_eq!(picked, ["a", "b"]);
    }

    #[test]
    fn duration_tokens_are_not_repeated_in_the_chip() {
        for token in ["5h", "wk", "30d", "mo", "d"] {
            assert!(is_duration_token(token), "{token}");
        }
        for label in ["Gemini wk", "Pro", ""] {
            assert!(!is_duration_token(label), "{label}");
        }
    }

    fn source(source: &str, id: &str, used: f64) -> Value {
        let reset = usage::now_ms() + 3 * HOUR;
        json!({"id": id, "sourceId": source, "sourceLabel": source, "status": "available",
               "observedAt": usage::now_ms(),
               "windows": [{"id": "five_hour", "label": "Session", "shortLabel": "5h", "summary": true,
                            "usedPct": used, "remainingPct": 100. - used, "resetsAt": reset,
                            "durationSeconds": 18000}]})
    }

    #[gpui::test]
    fn paseo_usage_footer_fetches_codex_and_antigravity_only_and_supports_manual_refresh(
        cx: &mut gpui::TestAppContext,
    ) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let host = TestHost::with_capabilities(vec!["paseo.usage"], move |request| {
            let mut seen = recorded.lock().unwrap();
            seen.push(request["params"].clone());
            assert_eq!(request["method"], "paseo.usage");
            match seen.len() {
                // The third request fails after the first two succeeded.
                3 => Some(json!({"error": "daemon busy"})),
                n => Some(json!({"fetchedAt": 1, "sources": [
                    source("grok", "grok", 5.),
                    source("antigravity", "ag", 30.),
                    source("codex", "cx", 10. * n as f64),
                ]})),
            }
        });
        let machine = host.client.list().unwrap().remove(0).id;
        cx.update(usage_footer::bind);
        let client = host.client.clone();
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| {
                    let mut footer = UsageFooter::new(Kind::Codex, cx);
                    footer.set_remaining(true, cx);
                    footer.enable_paseo_for_test(client, machine, cx);
                    footer
                })
            })
            .unwrap()
        });
        cx.run_until_parked();
        let wait = |cx: &mut gpui::TestAppContext| {
            for _ in 0..100 {
                cx.run_until_parked();
                if !window.read_with(cx, |f, _| f.paseo.fetching).unwrap() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        wait(cx);
        window
            .update(cx, |footer, w, cx| {
                let ids: Vec<_> = footer.paseo.sources.iter().map(|s| s.id.as_str()).collect();
                assert_eq!(ids, ["cx", "ag"], "grok is dropped");
                assert!(footer.paseo.error.is_none());
                assert_eq!(
                    footer.paseo_percent(&footer.paseo.sources[0].windows[0]),
                    Some(90.)
                );
                w.focus(&footer.trigger, cx);
            })
            .unwrap();
        assert_eq!(requests.lock().unwrap()[0], json!({"forceRefresh": false}));
        cx.refresh().unwrap();
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        assert!(visual.debug_bounds("usage-footer").is_some());
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        window
            .update(visual, |footer, w, _| {
                assert!(footer.open);
                assert!(footer.details.is_focused(w));
            })
            .unwrap();
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        window
            .update(visual, |footer, _, _| assert!(!footer.open))
            .unwrap();

        // Manual refresh bypasses the 180 s throttle and asks the daemon to refresh live.
        window
            .update(visual, |footer, _, cx| footer.refresh(false, cx))
            .unwrap();
        assert_eq!(
            requests.lock().unwrap().len(),
            1,
            "passive refresh is throttled"
        );
        window
            .update(visual, |footer, _, cx| footer.refresh(true, cx))
            .unwrap();
        wait(visual);
        assert_eq!(requests.lock().unwrap()[1], json!({"forceRefresh": true}));
        window
            .update(visual, |footer, _, _| {
                assert_eq!(
                    footer.paseo_percent(&footer.paseo.sources[0].windows[0]),
                    Some(80.)
                );
            })
            .unwrap();

        // A transient failure keeps both snapshots and flags the error.
        window
            .update(visual, |footer, _, cx| footer.refresh(true, cx))
            .unwrap();
        wait(visual);
        window
            .update(visual, |footer, _, _| {
                assert!(footer.paseo.error.is_some());
                let ids: Vec<_> = footer.paseo.sources.iter().map(|s| s.id.as_str()).collect();
                assert_eq!(ids, ["cx", "ag"]);
                assert!(!footer.paseo.fetching);
            })
            .unwrap();
        assert_eq!(requests.lock().unwrap().len(), 3);
    }

    #[test]
    fn resolve_with_never_falls_back_to_another_host_when_remote_env_is_missing() {
        let dir = std::env::temp_dir().join(format!(
            "paseo-usage-resolve-{}",
            monocode_core::mono::new_id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let connections = monocode_core::remote::Connections::new(&dir);
        let err = resolve_with(&connections, "remote://devbox-boe/home/me/app").unwrap_err();
        assert!(err.contains("devbox-boe"), "unexpected error: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui::test]
    fn switching_paseo_host_isolates_snapshots_and_throttle_state(cx: &mut gpui::TestAppContext) {
        let footer = cx.new(|cx| {
            let mut f = UsageFooter::new(Kind::Codex, cx);
            f.paseo_mode = true;
            f
        });
        footer.update(cx, |f, cx| {
            f.switch_paseo_host("remote://mac-local/Users/me/repo", cx);
            f.paseo.sources = vec![PaseoUsageSource {
                id: "mac-cx".into(),
                source_id: "codex".into(),
                source_label: "Codex".into(),
                status: "available".into(),
                ..Default::default()
            }];
            f.paseo.attempted = Some(Instant::now());
            f.paseo.updated_at = Some(T0);

            // Switch to devbox-boe: Mac's snapshot and throttle must NOT carry over.
            f.switch_paseo_host("remote://devbox-boe/home/me/repo", cx);
            assert!(f.paseo.sources.is_empty());
            assert!(f.paseo.attempted.is_none());
            assert!(f.paseo.updated_at.is_none());
            assert_eq!(f.paseo.active_host.as_deref(), Some("devbox-boe"));

            // Switch back to mac-local: Mac's cached snapshot returns intact.
            f.switch_paseo_host("remote://mac-local/Users/me/repo", cx);
            assert_eq!(f.paseo.sources.len(), 1);
            assert_eq!(f.paseo.sources[0].id, "mac-cx");

            // Switching away while a fetch is in flight (A -> B -> A) clears `attempted`
            // so returning to A immediately re-fetches instead of hitting the 180 s throttle.
            f.paseo.fetching = true;
            f.paseo.attempted = Some(Instant::now());
            f.switch_paseo_host("remote://devbox-boe/home/me/repo", cx);
            f.switch_paseo_host("remote://mac-local/Users/me/repo", cx);
            assert!(
                f.paseo.attempted.is_none(),
                "abandoned in-flight fetch must clear throttle timestamp"
            );
        });
    }
}
