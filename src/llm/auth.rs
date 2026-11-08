//! How a request proves who it is.
//!
//! Every managed cloud speaks the same OpenAI chat API and then disagrees about
//! one header. Azure wants `api-key`, Anthropic wants `x-api-key`, everyone
//! else wants `Authorization: Bearer`. Modelling that as a header choice, rather
//! than as a new provider per vendor, is the difference between a config field
//! and a rewrite each time a customer names a cloud.
//!
//! Azure also puts the deployment in the path and the API version in the query
//! string, so an extra query fragment is configurable too.

use std::fmt;

/// How to attach credentials to a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthStyle {
    /// `Authorization: Bearer <key>` — OpenAI, DeepSeek, most gateways.
    Bearer,
    /// `api-key: <key>` — Azure OpenAI.
    ApiKey,
    /// `x-api-key: <key>` — Anthropic.
    XApiKey,
    /// A header name the operator supplies, for gateways that invent their own.
    Custom(String),
    /// Signed requests, which a header cannot express.
    ///
    /// AWS SigV4 needs the request canonicalised and signed per call. Doing it
    /// by hand is a lot of code that fails in ways that look like an auth typo,
    /// so this is deliberately not implemented: Bedrock is reached through an
    /// OpenAI-compatible gateway, which is the normal deployment anyway.
    Signed(String),
}

impl AuthStyle {
    /// Parse an operator-supplied auth style.
    pub fn parse(value: &str) -> Result<Self, String> {
        let trimmed = value.trim();
        let lower = trimmed.to_ascii_lowercase();
        match lower.as_str() {
            "" | "bearer" | "authorization" => Ok(Self::Bearer),
            "api-key" | "apikey" | "azure" => Ok(Self::ApiKey),
            "x-api-key" | "anthropic" => Ok(Self::XApiKey),
            "sigv4" | "aws" | "bedrock" => Ok(Self::Signed("aws-sigv4".to_owned())),
            _ => match trimmed.strip_prefix("header:") {
                Some(name) if !name.trim().is_empty() => {
                    Ok(Self::Custom(name.trim().to_owned()))
                }
                _ => Err(format!(
                    "unknown auth style '{trimmed}'; use bearer, api-key, x-api-key, or header:<Name>"
                )),
            },
        }
    }

    /// The header name and value for this style, if it is header-based.
    pub fn header(&self, key: &str) -> Option<(String, String)> {
        match self {
            Self::Bearer => Some(("Authorization".to_owned(), format!("Bearer {key}"))),
            Self::ApiKey => Some(("api-key".to_owned(), key.to_owned())),
            Self::XApiKey => Some(("x-api-key".to_owned(), key.to_owned())),
            Self::Custom(name) => Some((name.clone(), key.to_owned())),
            Self::Signed(_) => None,
        }
    }

    /// Whether this style needs request signing rather than a header.
    pub fn requires_signing(&self) -> bool {
        matches!(self, Self::Signed(_))
    }

    pub fn label(&self) -> String {
        match self {
            Self::Bearer => "bearer".to_owned(),
            Self::ApiKey => "api-key".to_owned(),
            Self::XApiKey => "x-api-key".to_owned(),
            Self::Custom(name) => format!("header:{name}"),
            Self::Signed(scheme) => scheme.clone(),
        }
    }
}

impl fmt::Display for AuthStyle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn azure_sends_its_key_in_the_api_key_header() {
        let style = AuthStyle::parse("azure").expect("parsed");
        assert_eq!(style, AuthStyle::ApiKey);
        assert_eq!(
            style.header("secret"),
            Some(("api-key".to_owned(), "secret".to_owned()))
        );
    }

    #[test]
    fn the_default_is_bearer_so_existing_setups_are_unchanged() {
        assert_eq!(AuthStyle::parse("").expect("parsed"), AuthStyle::Bearer);
        assert_eq!(
            AuthStyle::Bearer.header("secret"),
            Some(("Authorization".to_owned(), "Bearer secret".to_owned()))
        );
    }

    #[test]
    fn a_gateway_may_name_its_own_header() {
        let style = AuthStyle::parse("header:X-Acme-Token").expect("parsed");
        assert_eq!(
            style.header("secret"),
            Some(("X-Acme-Token".to_owned(), "secret".to_owned()))
        );
    }

    #[test]
    fn signed_styles_are_recognised_but_carry_no_header() {
        let style = AuthStyle::parse("bedrock").expect("parsed");
        assert!(style.requires_signing());
        // Nothing to attach: signing is not a header, which is exactly why it
        // is refused rather than half-implemented.
        assert_eq!(style.header("secret"), None);
    }

    #[test]
    fn an_unknown_style_says_what_is_accepted() {
        let error = AuthStyle::parse("magic").unwrap_err();
        assert!(error.contains("bearer"), "{error}");
        assert!(error.contains("header:"), "{error}");
    }
}
