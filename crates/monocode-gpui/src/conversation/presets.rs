use super::*;
use crate::settings::AgentPreset;

impl Conversation {
    pub(super) fn favorite_preset_selected(&mut self, id: &str, cx: &mut Context<Self>) {
        self.menu_choice(&format!("agent-preset:{id}"), cx);
    }

    pub(super) fn agent_preset_items(&self, cx: &gpui::App) -> Vec<MenuItem> {
        if self.settings.mono.is_some() {
            return Vec::new();
        }
        let Some(prefs) = cx.try_global::<crate::settings::Preferences>() else {
            return Vec::new();
        };
        let hidden = prefs.hidden_for(&self.summary.cwd);
        let mut items = Vec::new();
        for preset in &prefs.agent_presets {
            if hidden.contains(&preset.provider) && preset.provider != self.summary.provider {
                continue;
            }
            let checked = if self.paseo_controls() {
                preset.paseo_settings().is_ok_and(|(_, bundle)| {
                    self.summary.provider == preset.provider
                        && self.settings.access == AccessMode::FullAccess
                        && self.model.as_deref()
                            == Some(format!("{}:{}", preset.provider, preset.model).as_str())
                        && self.settings.effort == bundle.effort
                        && self
                            .settings
                            .model_settings
                            .iter()
                            .filter(|(key, _)| key.starts_with("paseo."))
                            .all(|(key, value)| bundle.model_settings.get(key) == Some(value))
                        && bundle.model_settings.iter().all(|(key, value)| {
                            self.settings.model_settings.get(key) == Some(value)
                        })
                })
            } else {
                preset.settings().is_ok_and(|(_, bundle)| {
                    self.summary.provider == preset.provider
                        && self.model == bundle.model
                        && self.settings.access == bundle.access
                        && self.settings.effort == bundle.effort
                        && self.settings.service_tier == bundle.service_tier
                        && preset
                            .validate_effort(self.catalog.iter().find(|m| m.id == preset.model))
                            .is_ok()
                        && !self.settings.fast
                        && !self.settings.plan
                        && self.settings.thinking.is_none()
                        && self.settings.model_settings.iter().all(|(key, value)| {
                            match key.as_str() {
                                "fast" => value == "false",
                                "serviceTier" => Some(value) == bundle.service_tier.as_ref(),
                                "effort" | "reasoning" | "reasoningEffort" | "reasoning_effort"
                                | "variant" => Some(value) == bundle.effort.as_ref(),
                                _ => false,
                            }
                        })
                })
            };
            items.push(
                MenuAction::new(format!("agent-preset:{}", preset.id), preset.name.clone())
                    .image(Kind::from_id(&preset.provider).id())
                    .description(preset.summary())
                    .checked(checked)
                    .disabled(
                        self.busy
                            || self.stopping
                            || self.account_switching
                            || (self.is_remote() && !self.paseo_controls()),
                    )
                    .into(),
            );
        }
        if self.paseo_controls() {
            items.push(
                MenuAction::new("save-agent-preset", "Save current model as preset")
                    .icon("plus")
                    .disabled(self.busy || self.model.is_none())
                    .into(),
            );
        }
        if !items.is_empty() {
            items.insert(0, MenuItem::Header("Presets".into()));
        }
        items
    }

    pub(super) fn save_paseo_preset(&mut self, cx: &mut Context<Self>) {
        if !self.paseo_controls() || self.busy {
            return;
        }
        let Some(model) = self.model.clone() else {
            return;
        };
        let provider = self.summary.provider.clone();
        let mode_id = match provider.as_str() {
            "hermes" => "dont_ask",
            "claude" => "bypassPermissions",
            "codex" => "full-access",
            "omp" => "full",
            _ => {
                self.status = "此 provider 暂不支持原生预设".into();
                cx.notify();
                return;
            }
        };
        let raw = model
            .strip_prefix(&format!("{provider}:"))
            .unwrap_or(&model)
            .to_owned();
        let features = self
            .settings
            .model_settings
            .iter()
            .filter(|(k, _)| k.starts_with("paseo."))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let name = format!(
            "{}{}",
            self.model_label(),
            self.settings
                .effort
                .as_ref()
                .map(|e| format!(" · {e}"))
                .unwrap_or_default()
        );
        let preset = AgentPreset {
            id: monocode_core::mono::new_id(),
            name,
            provider,
            model: raw,
            mode_id: mode_id.into(),
            thinking_option_id: self.settings.effort.clone(),
            feature_values: features,
            notes: String::new(),
            extra: Default::default(),
        };
        cx.update_global::<crate::settings::Preferences, _>(|prefs, _| {
            prefs.agent_presets.push(preset);
            prefs.model_picker_tab = Some("favorites".into());
        });
        self.status = "已保存到 Favorites".into();
        cx.notify();
    }
    pub(super) fn select_agent_preset(&mut self, preset: AgentPreset, cx: &mut Context<Self>) {
        self.preset_epoch += 1;
        let epoch = self.preset_epoch;
        if self.busy
            || self.stopping
            || self.account_switching
            || (self.is_remote() && !self.paseo_controls())
        {
            self.status = "Wait for an idle local conversation before applying a preset.".into();
            cx.notify();
            return;
        }
        let kind = match if self.paseo_controls() {
            preset.paseo_settings()
        } else {
            preset.settings()
        } {
            Ok((kind, _)) => kind,
            Err(error) => {
                self.status = error;
                cx.notify();
                return;
            }
        };
        let remote_project = self.is_remote().then(|| self.summary.cwd.clone());
        let models = if kind.id() == self.summary.provider {
            Some(self.catalog.clone())
        } else {
            let account = crate::accounts::get(cx)
                .map(|center| center.read(cx).selected(kind, &self.summary.cwd))
                .unwrap_or_default();
            if remote_project.is_some() {
                None
            } else {
                catalog::cached_models_for(kind, &account)
            }
        };
        if preset.thinking_option_id.is_none() || models.is_some() {
            self.apply_agent_preset(&preset, models.as_deref().unwrap_or_default(), cx);
            return;
        }
        if cfg!(test) {
            self.status = "Preset needs live model capabilities".into();
            cx.notify();
            return;
        }
        let account = crate::accounts::get(cx)
            .map(|center| center.read(cx).selected(kind, &self.summary.cwd))
            .unwrap_or_default();
        let revision = self.settings.revision;
        let provider = self.summary.provider.clone();
        self.status = "Checking preset capabilities…".into();
        let work = cx.background_executor().spawn(async move {
            if let Some(project) = remote_project {
                catalog::remote_models(&project, kind.id())
            } else {
                catalog::models_for(kind, &account)
            }
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.settings.revision != revision
                    || this.summary.provider != provider
                    || this.preset_epoch != epoch
                {
                    return;
                }
                match result {
                    Ok(models) => this.apply_agent_preset(&preset, &models, cx),
                    Err(error) => {
                        this.status = error;
                        cx.notify();
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn apply_agent_preset(
        &mut self,
        preset: &AgentPreset,
        models: &[Model],
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.stopping || self.account_switching {
            return;
        }
        if let Err(error) = preset.validate_effort(models.iter().find(|model| {
            model.id == preset.model
                || (self.paseo_controls()
                    && model.id == format!("{}:{}", preset.provider, preset.model))
        })) {
            self.status = error;
            cx.notify();
            return;
        }
        let kind = Kind::parse(&preset.provider).expect("validated preset agent");
        let picked = ModelPicked(
            kind,
            Some(if self.paseo_controls() {
                format!("{}:{}", kind.id(), preset.model)
            } else {
                preset.model.clone()
            }),
        );
        if self.apply_model_bundle(&picked, Some(preset), cx) {
            remember_model(kind, picked.1, cx);
            self.status = "Ready".into();
            cx.notify();
        }
    }
}
