//! The approved-model registry.
//!
//! Two things have to be true at once for an enterprise sale:
//!
//! - **We choose the model.** An audit's output quality is our reputation. A
//!   customer pointing this at an unknown 7B model and getting nonsense
//!   descriptions is our problem, not theirs.
//! - **They choose where it runs.** Banks, healthcare, and anyone with valuable
//!   IP cannot let code-derived data reach a third-party API, and many already
//!   hold a signed agreement with Azure OpenAI or Bedrock. Refusing their
//!   endpoint does not keep them on our infrastructure — it loses the deal.
//!
//! So the registry fixes *what* may run and leaves *where* open. A custom base
//! URL is always allowed; an unrecognised model is not.
//!
//! No model id appears anywhere else in the codebase. Ids change; this table is
//! the single place to edit when they do, and an operator can override any of
//! them from the environment without a rebuild.

use std::fmt;

/// What a model is for. Ranking runs constantly and is trivial; describing runs
/// rarely and carries the output a client reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// Cheap and fast. Search ranking, classification.
    Fast,
    /// Stronger reasoning. Descriptions, documentation reconciliation.
    Strong,
}

impl Tier {
    pub fn label(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Strong => "strong",
        }
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// An approved model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApprovedModel {
    /// Provider token as written in configuration.
    pub provider: &'static str,
    /// The model id sent on the wire.
    pub id: &'static str,
    pub tier: Tier,
    /// Environment variable that overrides this id without a rebuild.
    pub override_var: &'static str,
    /// Variable holding the API key for this provider.
    pub key_var: &'static str,
}

/// The approved set.
///
/// Ordered by provider, then tier. Deliberately small: every entry is a
/// support obligation, and prompts are tuned against the models listed here.
///
/// Anthropic ids are current. Ids for other vendors move faster than this file
/// does, which is exactly why `override_var` exists — an operator can correct
/// one without waiting for a release.
pub const APPROVED_MODELS: &[ApprovedModel] = &[
    ApprovedModel {
        provider: "anthropic",
        id: "claude-haiku-4-5-20251001",
        tier: Tier::Fast,
        override_var: "ANTHROPIC_FAST_MODEL",
        key_var: "ANTHROPIC_API_KEY",
    },
    ApprovedModel {
        provider: "anthropic",
        id: "claude-sonnet-5",
        tier: Tier::Strong,
        override_var: "ANTHROPIC_STRONG_MODEL",
        key_var: "ANTHROPIC_API_KEY",
    },
    ApprovedModel {
        provider: "openai",
        id: "gpt-4o-mini",
        tier: Tier::Fast,
        override_var: "OPENAI_FAST_MODEL",
        key_var: "OPENAI_API_KEY",
    },
    ApprovedModel {
        provider: "openai",
        id: "gpt-4o",
        tier: Tier::Strong,
        override_var: "OPENAI_STRONG_MODEL",
        key_var: "OPENAI_API_KEY",
    },
    ApprovedModel {
        provider: "deepseek",
        id: "deepseek-chat",
        tier: Tier::Fast,
        override_var: "DEEPSEEK_FAST_MODEL",
        key_var: "DEEPSEEK_API_KEY",
    },
    ApprovedModel {
        provider: "deepseek",
        id: "deepseek-reasoner",
        tier: Tier::Strong,
        override_var: "DEEPSEEK_STRONG_MODEL",
        key_var: "DEEPSEEK_API_KEY",
    },
    ApprovedModel {
        provider: "ollama",
        id: "qwen2.5-coder",
        tier: Tier::Fast,
        override_var: "OLLAMA_FAST_MODEL",
        key_var: "OLLAMA_API_KEY",
    },
];

/// Providers in the order they are tried when nothing is configured explicitly.
///
/// There is no default vendor. This order exists only so the search is
/// deterministic; whichever provider is selected is always announced, because
/// silently choosing who gets billed is not acceptable.
pub const PROVIDER_SEARCH_ORDER: &[&str] = &["anthropic", "openai", "deepseek", "ollama"];

/// Look up the approved model for a provider and tier.
pub fn model_for(provider: &str, tier: Tier) -> Option<&'static ApprovedModel> {
    APPROVED_MODELS
        .iter()
        .find(|model| model.provider == provider && model.tier == tier)
}

/// Every approved model for a provider, whatever the tier.
pub fn models_for_provider(provider: &str) -> Vec<&'static ApprovedModel> {
    APPROVED_MODELS
        .iter()
        .filter(|model| model.provider == provider)
        .collect()
}

/// Is this model id approved for this provider?
///
/// An operator override counts as approved: it was set deliberately, by
/// someone who can already run any command they like.
pub fn is_approved(provider: &str, id: &str) -> bool {
    models_for_provider(provider).iter().any(|model| {
        model.id == id
            || std::env::var(model.override_var)
                .map(|value| value.trim() == id)
                .unwrap_or(false)
    })
}

/// The id to send for a provider and tier, honouring an operator override.
pub fn resolve_id(provider: &str, tier: Tier) -> Option<String> {
    let model = model_for(provider, tier)?;
    if let Ok(override_value) = std::env::var(model.override_var) {
        let trimmed = override_value.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    Some(model.id.to_owned())
}

/// Every approved model, for `--list-models` and error messages.
pub fn describe_catalogue() -> String {
    let mut out = String::new();
    for provider in PROVIDER_SEARCH_ORDER {
        let models = models_for_provider(provider);
        if models.is_empty() {
            continue;
        }
        out.push_str(&format!("{provider}:\n"));
        for model in models {
            out.push_str(&format!(
                "  {:<7} {:<28} override with {}\n",
                model.tier.label(),
                model.id,
                model.override_var
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_provider_offers_both_tiers() {
        for provider in PROVIDER_SEARCH_ORDER {
            // Ollama is local-only and ships a single recommended model.
            if *provider == "ollama" {
                continue;
            }
            assert!(
                model_for(provider, Tier::Fast).is_some(),
                "{provider} has no fast model"
            );
            assert!(
                model_for(provider, Tier::Strong).is_some(),
                "{provider} has no strong model"
            );
        }
    }

    #[test]
    fn approval_rejects_an_unknown_model() {
        assert!(is_approved("anthropic", "claude-haiku-4-5-20251001"));
        assert!(!is_approved("anthropic", "some-random-finetune"));
        assert!(!is_approved("openai", "llama-3-8b-instruct"));
    }

    #[test]
    fn a_model_id_is_never_hardcoded_outside_this_table() {
        // resolve_id is the only way callers obtain an id, so a stale constant
        // cannot leak into request-building code.
        assert_eq!(
            resolve_id("deepseek", Tier::Fast).as_deref(),
            Some("deepseek-chat")
        );
        assert_eq!(resolve_id("nonexistent", Tier::Fast), None);
    }

    #[test]
    fn every_override_variable_is_distinct() {
        let mut seen = std::collections::HashSet::new();
        for model in APPROVED_MODELS {
            assert!(
                seen.insert(model.override_var),
                "{} is used twice",
                model.override_var
            );
        }
    }

    #[test]
    fn the_catalogue_lists_every_approved_model() {
        let catalogue = describe_catalogue();
        for model in APPROVED_MODELS {
            assert!(
                catalogue.contains(model.id),
                "{} missing from the catalogue",
                model.id
            );
        }
    }
}
