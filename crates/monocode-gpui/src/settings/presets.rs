//! Configured names and bundles overlay discovery without rewriting provider IDs.
use monocode_core::{
    agent::{AccessMode, Settings},
    catalog::Model,
    provider::Kind,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentPreset {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub model: String,
    pub mode_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_option_id: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub feature_values: HashMap<String, String>,
    #[serde(default)]
    pub notes: String,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl AgentPreset {
    pub(crate) fn summary(&self) -> String {
        format!(
            "{} · {}{}{}",
            Kind::from_id(&self.provider).label(),
            self.model,
            self.thinking_option_id
                .as_ref()
                .map(|v| format!(" · {v}"))
                .unwrap_or_default(),
            if self.notes.is_empty() {
                String::new()
            } else {
                format!(" — {}", self.notes)
            }
        )
    }

    pub(crate) fn settings(&self) -> Result<(Kind, Settings), String> {
        let kind = Kind::parse(&self.provider).ok_or("Unknown preset agent")?;
        let full = matches!(
            (kind, self.mode_id.as_str()),
            (Kind::Hermes, "dont_ask")
                | (Kind::Claude, "bypassPermissions")
                | (Kind::Codex, "full-access")
                | (Kind::Omp, "full")
        );
        if !full || self.model.is_empty() {
            return Err("Unsupported preset permission mode or empty model".into());
        }
        if self
            .feature_values
            .iter()
            .any(|(key, value)| kind != Kind::Codex || key != "service_tier" || value != "default")
        {
            return Err("Unsupported preset feature; no settings changed".into());
        }
        Ok((
            kind,
            Settings {
                model: Some(self.model.clone()),
                access: AccessMode::FullAccess,
                effort: self.thinking_option_id.clone(),
                service_tier: self.feature_values.get("service_tier").cloned(),
                fast: false,
                ..Default::default()
            },
        ))
    }

    /// Only Paseo's advertised feature controls use these daemon-owned IDs.
    pub(crate) fn paseo_settings(&self) -> Result<(Kind, Settings), String> {
        let mut basic = self.clone();
        basic.feature_values.clear();
        let (kind, mut settings) = basic.settings()?;
        settings.model_settings = self
            .feature_values
            .iter()
            .map(|(key, value)| {
                (
                    if key.starts_with("paseo.") {
                        key.clone()
                    } else {
                        format!("paseo.{key}")
                    },
                    value.clone(),
                )
            })
            .collect();
        Ok((kind, settings))
    }
    pub(crate) fn apply_paseo(&self, target: &mut Settings) -> Result<(), String> {
        let (_, bundle) = self.paseo_settings()?;
        target.model = bundle.model;
        target.effort = bundle.effort;
        target.fast = false;
        target.thinking = None;
        target.service_tier = None;
        target.model_settings = bundle.model_settings;
        target.plan = false;
        // Existing agents retain their actual permission policy.
        Ok(())
    }
    pub(crate) fn validate_effort(&self, model: Option<&Model>) -> Result<(), String> {
        if let Some(effort) = &self.thinking_option_id
            && !model.is_some_and(|model| model.efforts.contains(effort))
        {
            return Err(format!(
                "{}: exact model/effort ({effort}) is not advertised by this agent. No settings changed.",
                self.name
            ));
        }
        Ok(())
    }

    pub(crate) fn apply(&self, target: &mut Settings) -> Result<(), String> {
        let (_, bundle) = self.settings()?;
        target.model = bundle.model;
        target.access = bundle.access;
        target.effort = bundle.effort;
        target.fast = false;
        target.service_tier = bundle.service_tier;
        target.thinking = None;
        target.model_settings.clear();
        target.plan = false;
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ConfiguredAgent {
    pub label: String,
    pub models: Vec<ConfiguredModel>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ConfiguredModel {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "isDefault", default)]
    pub is_default: bool,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::settings::Preferences;
    use serde_json::{Value, json};

    pub(crate) fn source_preferences() -> Preferences {
        let source: Value =
            serde_json::from_str(include_str!("fixtures/paseo-presets.json")).unwrap();
        serde_json::from_value(json!({"agent_presets": source["profiles"],
            "configured_models": source["providers"], "future_preference": {"keep": 7}}))
        .unwrap()
    }

    #[test]
    fn exact_presets_round_trip_and_map_full_permissions_without_fast_or_inherited_effort() {
        let prefs = source_preferences();
        let source: Value =
            serde_json::from_str(include_str!("fixtures/paseo-presets.json")).unwrap();
        assert_eq!(prefs.agent_presets.len(), 7);
        for (preset, source) in prefs
            .agent_presets
            .iter()
            .zip(source["profiles"].as_array().unwrap())
        {
            assert_eq!(serde_json::to_value(preset).unwrap(), *source);
            let (kind, bundle) = preset.settings().unwrap();
            assert_eq!(kind.id(), source["provider"].as_str().unwrap());
            assert_eq!(bundle.access, AccessMode::FullAccess);
            assert_eq!(bundle.model.as_deref(), source["model"].as_str());
            assert_eq!(
                bundle.effort.as_deref(),
                source["thinkingOptionId"].as_str()
            );
            let mut target = Settings {
                effort: Some("max".into()),
                fast: true,
                service_tier: Some("fast".into()),
                plan: true,
                model_settings: HashMap::from([("thinking".into(), "max".into())]),
                ..Default::default()
            };
            preset.apply(&mut target).unwrap();
            assert!(!target.fast && !target.plan && target.model_settings.is_empty());
            assert_eq!(target.effort, bundle.effort);
            if kind == Kind::Codex {
                assert_eq!(target.service_tier.as_deref(), Some("default"));
            }
        }
        let restored: Preferences =
            serde_json::from_value(serde_json::to_value(&prefs).unwrap()).unwrap();
        assert_eq!(restored, prefs);
        assert_eq!(restored.extra["future_preference"], json!({"keep": 7}));
        assert!(
            serde_json::from_value::<Preferences>(json!({}))
                .unwrap()
                .agent_presets
                .is_empty()
        );
    }

    #[test]
    fn paseo_presets_keep_native_feature_keys_and_existing_access_policy() {
        let preset = source_preferences().agent_presets[5].clone();
        let mut settings = Settings {
            access: AccessMode::Supervised,
            ..Default::default()
        };
        preset.apply_paseo(&mut settings).unwrap();
        assert_eq!(settings.access, AccessMode::Supervised);
        assert_eq!(
            settings
                .model_settings
                .get("paseo.service_tier")
                .map(String::as_str),
            Some("default")
        );
        assert_eq!(
            preset.settings().unwrap().1.service_tier.as_deref(),
            Some("default")
        );
    }
    #[test]
    fn main_default_preset_is_scoped_to_its_exact_agent_and_model() {
        let mut prefs = source_preferences();
        prefs.default_agent_preset = Some("hermes-gpt-6".into());
        let preset = prefs
            .new_chat_preset("hermes", Some("openai-codex:gpt-6.1-sol"))
            .unwrap();
        let mut settings = Settings::default();
        preset.apply(&mut settings).unwrap();
        assert_eq!(settings.effort.as_deref(), Some("high"));
        assert_eq!(settings.access, AccessMode::FullAccess);
        assert!(
            prefs
                .new_chat_preset("hermes", Some("custom:sub2tok:claude-opus-5-5"))
                .is_none()
        );
        assert!(
            prefs
                .new_chat_preset("codex", Some("gpt-6.1-sol"))
                .is_none()
        );
    }

    #[test]
    fn labels_overlay_exact_ids_without_inventing_or_replacing_capabilities() {
        let prefs = source_preferences();
        let source: Value =
            serde_json::from_str(include_str!("fixtures/paseo-presets.json")).unwrap();
        for (provider, agent) in &prefs.configured_models {
            for (entry, configured) in agent
                .models
                .iter()
                .zip(source["providers"][provider]["models"].as_array().unwrap())
            {
                assert_eq!(entry.id, configured["id"].as_str().unwrap());
                assert_eq!(entry.label, configured["label"].as_str().unwrap());
                assert_eq!(
                    entry.description,
                    configured["description"].as_str().unwrap_or_default()
                );
            }
        }
        let models = prefs.named_catalog(
            "codex",
            vec![Model {
                id: "gpt-6.1-sol".into(),
                efforts: vec!["high".into()],
                default_effort: Some("high".into()),
                ..Default::default()
            }],
        );
        assert_eq!(models[0].name, "GPT-6.1 Sol");
        assert_eq!(models[0].efforts, ["high"]);
        let preset = &prefs.agent_presets[5];
        assert!(preset.validate_effort(models.first()).is_ok());
        assert!(preset.validate_effort(models.get(1)).is_err());
        assert!(prefs.agent_presets[1].validate_effort(None).is_err());
    }
}
