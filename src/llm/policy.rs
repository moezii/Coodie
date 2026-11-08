//! Who is allowed to choose the model, and where it runs.
//!
//! Three contexts, and they are genuinely different:
//!
//! - **Local** — someone running the CLI on their own machine with their own
//!   key. There is no "us" to protect; they configure whatever they like.
//! - **Managed** — we run the audit as a service. The customer never sees a
//!   provider, a key, or a model name, because we buy tokens and sell audits.
//!   Configuration found in the repository under audit is ignored: it belongs
//!   to the customer, and honouring it would let them bill us or point us at
//!   a model whose output we would have to stand behind.
//! - **Enterprise** — managed, except the customer may direct calls at their
//!   own endpoint. This exists for procurement, not privacy: an audit sends
//!   function and route names, never source, so the usual "our code cannot
//!   leave" objection does not apply. What does happen is that a bank's
//!   approved-vendor list excludes whoever we buy tokens from, and no
//!   architecture argument fixes that.
//!
//! The model stays ours in every mode. Enterprise moves *where* it runs, never
//! *what* runs — output quality is our name on the report.

use std::env;
use std::fmt;

/// Environment variable selecting the policy. Set by whoever operates the
/// service, never by the repository being audited.
pub const POLICY_VAR: &str = "CODIE_LLM_POLICY";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Policy {
    /// The CLI on someone's own machine. They configure everything.
    #[default]
    Local,
    /// We run it. The customer configures nothing.
    Managed,
    /// We run it, but the customer may supply their own endpoint.
    Enterprise,
}

impl Policy {
    pub fn from_env() -> Self {
        match env::var(POLICY_VAR)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "managed" => Self::Managed,
            "enterprise" => Self::Enterprise,
            _ => Self::Local,
        }
    }

    /// May configuration inside the audited repository be honoured?
    ///
    /// Never once we are the ones running it. A `.env` in a customer's repo is
    /// their file, and it is not a channel for telling our service which model
    /// to bill us for.
    pub fn reads_repository_config(self) -> bool {
        matches!(self, Self::Local)
    }

    /// May the caller point us at a different endpoint?
    pub fn allows_custom_endpoint(self) -> bool {
        matches!(self, Self::Local | Self::Enterprise)
    }

    /// May the caller choose which model runs?
    ///
    /// Only locally. Under managed and enterprise the model is ours: it is what
    /// the prompts are tuned against and what the report's quality rests on.
    pub fn allows_model_choice(self) -> bool {
        matches!(self, Self::Local)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Managed => "managed",
            Self::Enterprise => "enterprise",
        }
    }
}

impl fmt::Display for Policy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_is_the_default_so_the_cli_keeps_working() {
        assert_eq!(Policy::default(), Policy::Local);
        assert!(Policy::Local.reads_repository_config());
        assert!(Policy::Local.allows_model_choice());
        assert!(Policy::Local.allows_custom_endpoint());
    }

    #[test]
    fn managed_ignores_everything_the_customer_supplies() {
        let policy = Policy::Managed;
        // A .env in the audited repository must not steer our spending.
        assert!(!policy.reads_repository_config());
        assert!(!policy.allows_model_choice());
        assert!(!policy.allows_custom_endpoint());
    }

    #[test]
    fn enterprise_moves_the_endpoint_but_never_the_model() {
        let policy = Policy::Enterprise;
        assert!(policy.allows_custom_endpoint());
        // The model is what the report's quality rests on. It stays ours.
        assert!(!policy.allows_model_choice());
        assert!(!policy.reads_repository_config());
    }
}
