use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use thiserror::Error;

use crate::cache::{hash_parts, CacheError, CacheStore};
use crate::index::SourceLanguage;
use crate::query::{QueryCandidate, QueryResults};

/// Name of the single environment variable that configures the LLM.
pub const LLM_ENV_VAR: &str = "CODIE_LLM";

const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_OLLAMA_BASE_URL: &str = "http://localhost:11434";
const DEFAULT_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com/v1";

pub mod auth;
pub mod policy;
pub mod registry;

pub use auth::AuthStyle;
pub use policy::Policy;
pub use registry::Tier;

/// Auth style override, honoured when the policy allows a custom endpoint.
const AUTH_VAR: &str = "CODIE_LLM_AUTH";
/// Extra query string appended to the endpoint, for Azure's `api-version`.
const QUERY_VAR: &str = "CODIE_LLM_QUERY";
const DEFAULT_MAX_TOKENS: usize = 700;

#[derive(Debug, Error)]
pub enum LlmError {
    #[error(transparent)]
    Cache(#[from] CacheError),
    #[error("failed to read configuration file {path}: {source}")]
    ConfigFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("no LLM configured; set {LLM_ENV_VAR} (e.g. {LLM_ENV_VAR}=ollama,qwen2.5-coder)")]
    MissingConfig,
    #[error("invalid {LLM_ENV_VAR} value: {0}")]
    InvalidConfig(String),
    #[error("unknown LLM provider '{0}' in {LLM_ENV_VAR}")]
    UnknownProvider(String),
    #[error(
        "model '{model}' is not approved for {provider}.\n\
         Audit output quality depends on the model, so only tested models are allowed.\n\
         A custom base URL is fine — run an approved model on your own endpoint.\n\n\
         Approved models:\n{catalogue}"
    )]
    UnapprovedModel {
        provider: String,
        model: String,
        catalogue: String,
    },
    #[error("{provider} requires an API key; set it as the third field of {LLM_ENV_VAR}")]
    MissingCredential { provider: &'static str },
    #[error(
        "auth scheme '{scheme}' needs request signing, which is not implemented.\n\
         Reach AWS Bedrock through an OpenAI-compatible gateway (LiteLLM or the Bedrock \
         Access Gateway) and point {LLM_ENV_VAR} at that instead."
    )]
    SigningUnsupported { scheme: String },
    #[error(
        "{setting} was supplied, but the policy is '{policy}'.\n\
         Under this policy the model and endpoint are chosen by whoever runs the service, \
         not by the repository being audited."
    )]
    NotPermittedByPolicy {
        setting: &'static str,
        policy: &'static str,
    },
    #[error("no previous find results are available; run codereader find first")]
    MissingFindResults,
    #[error("result index {requested} is out of range; last find has {available} candidate(s)")]
    ResultIndexOutOfRange { requested: usize, available: usize },
    #[error("provider request failed with HTTP {status}: {body}")]
    HttpStatus { status: u16, body: String },
    #[error("provider transport error: {0}")]
    Transport(String),
    #[error("provider returned a response without any text content")]
    EmptyResponse,
    #[error("provider returned invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("failed to stream provider response: {0}")]
    StreamIo(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmProviderKind {
    OpenAi,
    Anthropic,
    Ollama,
}

impl LlmProviderKind {
    fn parse(value: &str) -> Result<Self, LlmError> {
        match normalize_token(value).as_str() {
            // `openai-compatible` is just OpenAI's chat API at a custom base URL.
            // DeepSeek speaks OpenAI's chat API; only the base URL differs.
            "openai" | "openai-compatible" | "compatible" | "local" | "gpt" | "chatgpt"
            | "deepseek" => Ok(Self::OpenAi),
            "anthropic" | "claude" => Ok(Self::Anthropic),
            "ollama" => Ok(Self::Ollama),
            _ => Err(LlmError::UnknownProvider(value.trim().to_owned())),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Ollama => "ollama",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmConfig {
    kind: LlmProviderKind,
    model: String,
    base_url: String,
    api_key: Option<String>,
    max_tokens: usize,
    anthropic_version: String,
    /// How credentials are attached. Defaults per provider; overridable when
    /// the policy permits a custom endpoint.
    auth: AuthStyle,
    /// Query string appended to the endpoint, for clouds that require one.
    extra_query: Option<String>,
}

/// The auth header a provider uses when nobody has said otherwise.
fn default_auth_for(kind: LlmProviderKind) -> AuthStyle {
    match kind {
        LlmProviderKind::Anthropic => AuthStyle::XApiKey,
        LlmProviderKind::OpenAi | LlmProviderKind::Ollama => AuthStyle::Bearer,
    }
}

impl LlmConfig {
    /// Build the config from the single `CODIE_LLM` variable (or an explicit override
    /// with the same `provider,model,key,base_url` shape).
    ///
    /// The policy decides whose configuration counts. Locally the caller's does.
    /// Once we are the ones running the audit, the repository under audit gets
    /// no say — its `.env` belongs to the customer, and honouring it would let
    /// them choose which model we pay for.
    pub fn from_env(root: &Path, override_spec: Option<&str>) -> Result<Self, LlmError> {
        Self::from_env_with_policy(root, override_spec, Policy::from_env())
    }

    /// As [`from_env`], with the policy supplied rather than read from the
    /// environment. Separated so policy behaviour is testable without mutating
    /// process-global state from inside parallel tests.
    pub fn from_env_with_policy(
        root: &Path,
        override_spec: Option<&str>,
        policy: Policy,
    ) -> Result<Self, LlmError> {
        if let Some(value) = override_spec.filter(|value| !value.trim().is_empty()) {
            if !policy.allows_model_choice() {
                return Err(LlmError::NotPermittedByPolicy {
                    setting: "--llm",
                    policy: policy.label(),
                });
            }
            return Self::parse(value);
        }

        let spec = if policy.reads_repository_config() {
            load_llm_spec(root)?.ok_or(LlmError::MissingConfig)?
        } else {
            // Operator configuration only: the process environment, never a
            // file inside the repository being audited.
            operator_spec()?.ok_or(LlmError::MissingConfig)?
        };
        Self::parse(&spec)
    }

    /// Parse a `provider,model,key,base_url` spec. Trailing fields are optional.
    pub fn parse(spec: &str) -> Result<Self, LlmError> {
        let mut fields = spec.splitn(4, ',').map(str::trim);
        let provider = fields.next().unwrap_or("");
        if provider.is_empty() {
            return Err(LlmError::InvalidConfig(
                "expected provider,model[,key[,base_url]]".to_owned(),
            ));
        }
        let kind = LlmProviderKind::parse(provider)?;
        let is_deepseek = normalize_token(provider) == "deepseek";
        let provider_token = normalize_token(provider).to_string();
        let model = fields.next().unwrap_or("").to_owned();
        // An omitted model resolves from the registry rather than a constant.
        let model = if model.is_empty() {
            registry::resolve_id(&provider_token, registry::Tier::Fast).unwrap_or_default()
        } else {
            model
        };
        if model.is_empty() {
            return Err(LlmError::InvalidConfig(format!(
                "missing model; use {LLM_ENV_VAR}={provider},<model>"
            )));
        }
        // The registry decides which models are allowed. A custom base URL is
        // fine — run an approved model wherever your contract says — but an
        // unrecognised model is not, because its output quality is our
        // reputation and the prompts are tuned against the approved set.
        if !registry::is_approved(&provider_token, &model) {
            return Err(LlmError::UnapprovedModel {
                provider: provider_token,
                model,
                catalogue: registry::describe_catalogue(),
            });
        }
        let api_key = fields
            .next()
            .map(str::to_owned)
            .filter(|value| !value.is_empty());
        let base_url = fields
            .next()
            .map(str::to_owned)
            .filter(|value| !value.is_empty());

        // Auth style and query fragment only matter when someone has pointed us
        // at their own endpoint; the defaults are right for every direct cloud.
        let custom_endpoint = base_url.is_some();
        let auth = match env::var(AUTH_VAR) {
            Ok(value) if !value.trim().is_empty() => {
                let style = AuthStyle::parse(&value).map_err(LlmError::InvalidConfig)?;
                if style.requires_signing() {
                    return Err(LlmError::SigningUnsupported {
                        scheme: style.label(),
                    });
                }
                style
            }
            _ => default_auth_for(kind),
        };
        let extra_query = env::var(QUERY_VAR)
            .ok()
            .map(|value| value.trim().trim_start_matches('?').to_owned())
            .filter(|value| !value.is_empty() && custom_endpoint);

        let config = match kind {
            LlmProviderKind::OpenAi => {
                let base_url = base_url.unwrap_or_else(|| {
                    if is_deepseek {
                        DEFAULT_DEEPSEEK_BASE_URL.to_owned()
                    } else {
                        DEFAULT_OPENAI_BASE_URL.to_owned()
                    }
                });
                // A custom base URL means a local/self-hosted OpenAI-compatible server,
                // which usually needs no key. Only the real OpenAI cloud requires one.
                if api_key.is_none() && is_openai_cloud(&base_url) {
                    return Err(LlmError::MissingCredential { provider: "openai" });
                }
                Self {
                    kind,
                    model,
                    base_url,
                    api_key,
                    max_tokens: DEFAULT_MAX_TOKENS,
                    anthropic_version: DEFAULT_ANTHROPIC_VERSION.to_owned(),
                    auth: auth.clone(),
                    extra_query: extra_query.clone(),
                }
            }
            LlmProviderKind::Anthropic => Self {
                kind,
                model,
                base_url: base_url.unwrap_or_else(|| DEFAULT_ANTHROPIC_BASE_URL.to_owned()),
                api_key: Some(api_key.ok_or(LlmError::MissingCredential {
                    provider: "anthropic",
                })?),
                max_tokens: DEFAULT_MAX_TOKENS,
                anthropic_version: DEFAULT_ANTHROPIC_VERSION.to_owned(),
                auth: auth.clone(),
                extra_query: extra_query.clone(),
            },
            LlmProviderKind::Ollama => Self {
                kind,
                model,
                base_url: base_url.unwrap_or_else(|| DEFAULT_OLLAMA_BASE_URL.to_owned()),
                api_key,
                max_tokens: DEFAULT_MAX_TOKENS,
                anthropic_version: DEFAULT_ANTHROPIC_VERSION.to_owned(),
                auth: auth.clone(),
                extra_query: extra_query.clone(),
            },
        };

        Ok(config)
    }


    /// The full request URL, including any query fragment the endpoint needs.
    ///
    /// A base URL that already names a deployment path is left alone apart from
    /// the suffix, which is what Azure's
    /// `…/openai/deployments/<name>/chat/completions?api-version=…` shape wants.
    pub fn endpoint_url(&self, suffix: &str) -> String {
        let base = join_url(&self.base_url, suffix);
        match &self.extra_query {
            Some(query) => {
                let separator = if base.contains('?') { '&' } else { '?' };
                format!("{base}{separator}{query}")
            }
            None => base,
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplanationRequest {
    candidate: QueryCandidate,
    objective_flags: Vec<ObjectiveFlag>,
}

impl ExplanationRequest {
    pub fn new(candidate: QueryCandidate) -> Self {
        Self {
            candidate,
            objective_flags: Vec::new(),
        }
    }

    pub fn candidate(&self) -> &QueryCandidate {
        &self.candidate
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectiveFlag {
    pub kind: ObjectiveFlagKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectiveFlagKind {
    CompileError,
    SyntaxError,
    UnresolvedImport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedPrompt {
    pub system: String,
    pub user: String,
}

impl RenderedPrompt {
    pub fn prompt_hash(&self) -> String {
        hash_parts(&[self.system.as_bytes(), self.user.as_bytes()])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplanationOutcome {
    pub cache_hit: bool,
    pub prompt_hash: String,
    pub model_key: String,
}

/// Provider-agnostic interface. `complete` is a single non-streaming call used for
/// ranking; `stream_explanation` streams an opt-in explanation token by token.
pub trait LlmProvider {
    fn provider_name(&self) -> &str;
    fn model(&self) -> &str;

    fn complete(&mut self, prompt: &RenderedPrompt) -> Result<String, LlmError>;

    fn stream_explanation(
        &mut self,
        prompt: &RenderedPrompt,
        sink: &mut dyn FnMut(&str) -> Result<(), LlmError>,
    ) -> Result<(), LlmError>;

    fn cache_model_key(&self) -> String {
        format!("{}:{}", self.provider_name(), self.model())
    }
}

/// Build a boxed provider from the single env-var config, for callers (like the
/// semantic `find` path) that need an LLM but do not have one in hand.
pub fn provider_from_env(
    root: &Path,
    override_spec: Option<&str>,
) -> Result<Box<dyn LlmProvider>, LlmError> {
    let config = LlmConfig::from_env(root, override_spec)?;
    Ok(provider_from_config(config))
}

/// Returns true when an LLM is configured for this repository, without building it.
pub fn llm_is_configured(root: &Path) -> bool {
    matches!(load_llm_spec(root), Ok(Some(_)))
}

pub fn explain_last_find_result<W: Write>(
    root: &Path,
    result_number: usize,
    llm_override: Option<&str>,
    writer: &mut W,
) -> Result<ExplanationOutcome, LlmError> {
    let cache = CacheStore::open_for_root(root)?;
    let results = cache
        .last_find_results::<QueryResults>()?
        .ok_or(LlmError::MissingFindResults)?;
    let candidate = results
        .candidates()
        .get(result_number.saturating_sub(1))
        .cloned()
        .ok_or_else(|| LlmError::ResultIndexOutOfRange {
            requested: result_number,
            available: results.candidates().len(),
        })?;
    let mut provider = provider_from_env(root, llm_override)?;

    explain_candidate_with_cache(
        &cache,
        &ExplanationRequest::new(candidate),
        provider.as_mut(),
        writer,
    )
}

pub fn explain_candidate_with_cache<P, W>(
    cache: &CacheStore,
    request: &ExplanationRequest,
    provider: &mut P,
    writer: &mut W,
) -> Result<ExplanationOutcome, LlmError>
where
    P: LlmProvider + ?Sized,
    W: Write,
{
    let prompt = render_explanation_prompt(request);
    let prompt_hash = prompt.prompt_hash();
    let model_key = provider.cache_model_key();

    if let Some(response) = cache.get_llm_response(&prompt_hash, &model_key)? {
        writer.write_all(response.as_bytes())?;
        writer.flush()?;
        return Ok(ExplanationOutcome {
            cache_hit: true,
            prompt_hash,
            model_key,
        });
    }

    let mut response = String::new();
    provider.stream_explanation(&prompt, &mut |chunk| {
        response.push_str(chunk);
        writer.write_all(chunk.as_bytes())?;
        writer.flush()?;
        Ok(())
    })?;
    cache.put_llm_response(&prompt_hash, &model_key, &response)?;

    Ok(ExplanationOutcome {
        cache_hit: false,
        prompt_hash,
        model_key,
    })
}

pub fn render_explanation_prompt(request: &ExplanationRequest) -> RenderedPrompt {
    let candidate = request.candidate();
    let language = SourceLanguage::from_path(&candidate.file)
        .map(|language| language.display_name().to_owned())
        .unwrap_or_else(|| "Unknown".to_owned());
    let flags = if request.objective_flags.is_empty() {
        "none".to_owned()
    } else {
        request
            .objective_flags
            .iter()
            .map(|flag| format!("- {:?}: {}", flag.kind, flag.message))
            .collect::<Vec<_>>()
            .join("\n")
    };

    RenderedPrompt {
        system: [
            "You explain retrieved code snippets for Code Reader.",
            "Explain only what the provided snippet does and why it matched the user's find result.",
            "Do not suggest fixes, patches, improvements, refactors, alternatives, style changes, or performance changes.",
            "Do not invent repository context outside the snippet and metadata.",
            "If objective flags are present, mention them as facts only.",
            "Return concise plain text.",
        ]
        .join(" "),
        user: format!(
            "Find result metadata:\nkind: {}\nlabel: {}\nfile: {}\nlines: {}\nlanguage: {}\nobjective_flags:\n{}\n\nCode snippet:\n```{}\n{}\n```",
            candidate.kind,
            candidate.label,
            candidate.file.display(),
            format_line_range(&candidate.line_range),
            language,
            flags,
            language_fence(&language),
            candidate.snippet
        ),
    }
}

fn provider_from_config(config: LlmConfig) -> Box<dyn LlmProvider> {
    match config.kind {
        LlmProviderKind::OpenAi => Box::new(OpenAiProvider { config }),
        LlmProviderKind::Anthropic => Box::new(AnthropicProvider { config }),
        LlmProviderKind::Ollama => Box::new(OllamaProvider { config }),
    }
}

#[derive(Debug)]
struct OpenAiProvider {
    config: LlmConfig,
}

impl OpenAiProvider {
    fn request(&self) -> Result<ureq::Request, LlmError> {
        // Azure puts the deployment in the path and the API version in the
        // query string, so both the URL suffix and the auth header have to be
        // configurable rather than assumed.
        let url = self.config.endpoint_url("chat/completions");
        let mut request = ureq::post(&url).set("Content-Type", "application/json");
        if let Some(api_key) = &self.config.api_key {
            if let Some((name, value)) = self.config.auth.header(api_key) {
                request = request.set(&name, &value);
            }
        }
        Ok(request)
    }

    fn body(&self, prompt: &RenderedPrompt, stream: bool) -> Value {
        json!({
            "model": self.config.model.as_str(),
            "messages": [
                {"role": "system", "content": prompt.system.as_str()},
                {"role": "user", "content": prompt.user.as_str()}
            ],
            "max_tokens": self.config.max_tokens,
            "stream": stream
        })
    }
}

impl LlmProvider for OpenAiProvider {
    fn provider_name(&self) -> &str {
        self.config.kind.label()
    }

    fn model(&self) -> &str {
        &self.config.model
    }

    fn complete(&mut self, prompt: &RenderedPrompt) -> Result<String, LlmError> {
        let response = self
            .request()?
            .send_json(self.body(prompt, false))
            .map_err(http_error)?;
        let value = response_json(response)?;
        let text = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("message"))
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        non_empty(text)
    }

    fn stream_explanation(
        &mut self,
        prompt: &RenderedPrompt,
        sink: &mut dyn FnMut(&str) -> Result<(), LlmError>,
    ) -> Result<(), LlmError> {
        let response = self
            .request()?
            .send_json(self.body(prompt, true))
            .map_err(http_error)?;

        stream_sse_response(response, |event| {
            if let Some(text) = event
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first())
                .and_then(|choice| choice.get("delta"))
                .and_then(|delta| delta.get("content"))
                .and_then(Value::as_str)
            {
                sink(text)?;
            }
            Ok(false)
        })
    }
}

#[derive(Debug)]
struct AnthropicProvider {
    config: LlmConfig,
}

impl AnthropicProvider {
    fn request(&self) -> Result<ureq::Request, LlmError> {
        let url = join_url(&self.config.base_url, "v1/messages");
        let api_key = self
            .config
            .api_key
            .as_ref()
            .ok_or(LlmError::MissingCredential {
                provider: "anthropic",
            })?;
        Ok(ureq::post(&url)
            .set("x-api-key", api_key)
            .set("anthropic-version", &self.config.anthropic_version)
            .set("Content-Type", "application/json"))
    }

    fn body(&self, prompt: &RenderedPrompt, stream: bool) -> Value {
        json!({
            "model": self.config.model.as_str(),
            "system": prompt.system.as_str(),
            "messages": [
                {"role": "user", "content": prompt.user.as_str()}
            ],
            "max_tokens": self.config.max_tokens,
            "stream": stream
        })
    }
}

impl LlmProvider for AnthropicProvider {
    fn provider_name(&self) -> &str {
        self.config.kind.label()
    }

    fn model(&self) -> &str {
        &self.config.model
    }

    fn complete(&mut self, prompt: &RenderedPrompt) -> Result<String, LlmError> {
        let response = self
            .request()?
            .send_json(self.body(prompt, false))
            .map_err(http_error)?;
        let value = response_json(response)?;
        let text = value
            .get("content")
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|block| block.get("text").and_then(Value::as_str))
                    .collect::<String>()
            })
            .unwrap_or_default();
        non_empty(text)
    }

    fn stream_explanation(
        &mut self,
        prompt: &RenderedPrompt,
        sink: &mut dyn FnMut(&str) -> Result<(), LlmError>,
    ) -> Result<(), LlmError> {
        let response = self
            .request()?
            .send_json(self.body(prompt, true))
            .map_err(http_error)?;

        stream_sse_response(response, |event| {
            match event.get("type").and_then(Value::as_str) {
                Some("content_block_delta") => {
                    if let Some(text) = event
                        .get("delta")
                        .and_then(|delta| delta.get("text"))
                        .and_then(Value::as_str)
                    {
                        sink(text)?;
                    }
                }
                Some("message_stop") => return Ok(true),
                Some("error") => return Err(provider_json_error(event)),
                _ => {}
            }
            Ok(false)
        })
    }
}

#[derive(Debug)]
struct OllamaProvider {
    config: LlmConfig,
}

impl OllamaProvider {
    fn request(&self) -> Result<ureq::Request, LlmError> {
        let url = ollama_chat_url(&self.config.base_url);
        let mut request = ureq::post(&url).set("Content-Type", "application/json");
        if let Some(api_key) = &self.config.api_key {
            request = request.set("Authorization", &format!("Bearer {api_key}"));
        }
        Ok(request)
    }

    fn body(&self, prompt: &RenderedPrompt, stream: bool) -> Value {
        json!({
            "model": self.config.model.as_str(),
            "messages": [
                {"role": "system", "content": prompt.system.as_str()},
                {"role": "user", "content": prompt.user.as_str()}
            ],
            "stream": stream,
            "options": {"num_predict": self.config.max_tokens}
        })
    }
}

impl LlmProvider for OllamaProvider {
    fn provider_name(&self) -> &str {
        self.config.kind.label()
    }

    fn model(&self) -> &str {
        &self.config.model
    }

    fn complete(&mut self, prompt: &RenderedPrompt) -> Result<String, LlmError> {
        let response = self
            .request()?
            .send_json(self.body(prompt, false))
            .map_err(http_error)?;
        let value = response_json(response)?;
        if let Some(error) = value.get("error").and_then(Value::as_str) {
            return Err(LlmError::Transport(error.to_owned()));
        }
        let text = value
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        non_empty(text)
    }

    fn stream_explanation(
        &mut self,
        prompt: &RenderedPrompt,
        sink: &mut dyn FnMut(&str) -> Result<(), LlmError>,
    ) -> Result<(), LlmError> {
        let response = self
            .request()?
            .send_json(self.body(prompt, true))
            .map_err(http_error)?;

        stream_ndjson_response(response, |event| {
            if let Some(error) = event.get("error").and_then(Value::as_str) {
                return Err(LlmError::Transport(error.to_owned()));
            }
            if let Some(text) = event
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(Value::as_str)
            {
                sink(text)?;
            }
            Ok(event.get("done").and_then(Value::as_bool).unwrap_or(false))
        })
    }
}

fn load_llm_spec(root: &Path) -> Result<Option<String>, LlmError> {
    if let Ok(value) = env::var(LLM_ENV_VAR) {
        if !value.trim().is_empty() {
            return Ok(Some(value));
        }
    }

    // A provider key already present in the environment is enough on its own.
    // There is no default vendor — the search order only makes the result
    // deterministic, and whichever provider wins is announced to stderr so
    // nobody is surprised by which account gets billed.
    for provider in registry::PROVIDER_SEARCH_ORDER {
        if let Some(spec) = spec_from_key(provider, |variable| env::var(variable).ok()) {
            announce_provider(provider, "environment");
            return Ok(Some(spec));
        }
    }

    // Search the audited repository first, then the directory the operator ran
    // from. Auditing someone else's code must not require putting your API key
    // in their repository.
    let mut candidates = vec![root.to_path_buf()];
    if let Ok(cwd) = env::current_dir() {
        if cwd != root {
            candidates.push(cwd);
        }
    }

    for directory in candidates {
        let dotenv_path = directory.join(".env");
        let contents = match fs::read_to_string(&dotenv_path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(LlmError::ConfigFile {
                    path: dotenv_path,
                    source,
                })
            }
        };

        let values = parse_dotenv(&contents);
        if let Some(spec) = values
            .get(LLM_ENV_VAR)
            .map(|value| value.to_owned())
            .filter(|value| !value.trim().is_empty())
        {
            return Ok(Some(spec));
        }

        for provider in registry::PROVIDER_SEARCH_ORDER {
            if let Some(spec) =
                spec_from_key(provider, |variable| values.get(variable).cloned())
            {
                announce_provider(provider, &dotenv_path.display().to_string());
                return Ok(Some(spec));
            }
        }
    }

    Ok(None)
}

/// Configuration supplied by whoever operates the service.
///
/// Only the process environment is consulted. Under a managed policy this is
/// the one place a key may come from, so a `.env` sitting in a customer's
/// repository cannot redirect our spending.
fn operator_spec() -> Result<Option<String>, LlmError> {
    if let Ok(value) = env::var(LLM_ENV_VAR) {
        if !value.trim().is_empty() {
            return Ok(Some(value));
        }
    }
    for provider in registry::PROVIDER_SEARCH_ORDER {
        if let Some(spec) = spec_from_key(provider, |variable| env::var(variable).ok()) {
            announce_provider(provider, "service environment");
            return Ok(Some(spec));
        }
    }
    Ok(None)
}

/// Build a spec from a provider's API key, using the registry for the model id.
///
/// The model is never written in this function. It comes from the registry,
/// which an operator can override without a rebuild.
fn spec_from_key(
    provider: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let fast = registry::model_for(provider, registry::Tier::Fast)?;
    let key = lookup(fast.key_var)?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    let model = registry::resolve_id(provider, registry::Tier::Fast)?;
    Some(format!("{provider},{model},{key}"))
}

/// Say which provider was picked and where the key came from. Never the key.
fn announce_provider(provider: &str, source: &str) {
    eprintln!("codereader: using {provider} (key from {source})");
}

fn parse_dotenv(contents: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let assignment = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let Some((key, value)) = assignment.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        values.insert(key.to_owned(), unquote_env_value(value.trim()));
    }

    values
}

fn unquote_env_value(value: &str) -> String {
    let single_quoted = value.starts_with('\'') && value.ends_with('\'');
    let double_quoted = value.starts_with('"') && value.ends_with('"');
    if (single_quoted || double_quoted) && value.len() >= 2 {
        value[1..value.len() - 1].to_owned()
    } else {
        value.to_owned()
    }
}

fn normalize_token(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace(' ', "-")
}

fn is_openai_cloud(base_url: &str) -> bool {
    base_url.contains("api.openai.com")
}

fn join_url(base_url: &str, suffix: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        suffix.trim_start_matches('/')
    )
}

fn ollama_chat_url(base_url: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    if base_url.ends_with("/api") {
        format!("{base_url}/chat")
    } else {
        format!("{base_url}/api/chat")
    }
}

fn response_json(response: ureq::Response) -> Result<Value, LlmError> {
    response
        .into_string()
        .map_err(|error| LlmError::Transport(error.to_string()))
        .and_then(|body| serde_json::from_str::<Value>(&body).map_err(LlmError::Json))
}

fn non_empty(text: String) -> Result<String, LlmError> {
    if text.trim().is_empty() {
        Err(LlmError::EmptyResponse)
    } else {
        Ok(text)
    }
}

fn stream_sse_response<F>(response: ureq::Response, mut on_event: F) -> Result<(), LlmError>
where
    F: FnMut(&Value) -> Result<bool, LlmError>,
{
    let reader = BufReader::new(response.into_reader());
    for line in reader.lines() {
        let line = line?;
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            break;
        }
        let value = serde_json::from_str::<Value>(data)?;
        if on_event(&value)? {
            break;
        }
    }
    Ok(())
}

fn stream_ndjson_response<F>(response: ureq::Response, mut on_event: F) -> Result<(), LlmError>
where
    F: FnMut(&Value) -> Result<bool, LlmError>,
{
    let reader = BufReader::new(response.into_reader());
    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value = serde_json::from_str::<Value>(trimmed)?;
        if on_event(&value)? {
            break;
        }
    }
    Ok(())
}

fn http_error(error: ureq::Error) -> LlmError {
    match error {
        ureq::Error::Status(status, response) => {
            let body = response.into_string().unwrap_or_default();
            LlmError::HttpStatus {
                status,
                body: sanitize_provider_body(&body),
            }
        }
        ureq::Error::Transport(error) => LlmError::Transport(error.to_string()),
    }
}

fn provider_json_error(event: &Value) -> LlmError {
    let message = event
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .or_else(|| event.get("message").and_then(Value::as_str))
        .unwrap_or("provider returned an error event");
    LlmError::Transport(message.to_owned())
}

fn sanitize_provider_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.len() <= 700 {
        trimmed.to_owned()
    } else {
        format!("{}...", &trimmed[..700])
    }
}

fn format_line_range(line_range: &crate::index::LineRange) -> String {
    if line_range.start == line_range.end {
        line_range.start.to_string()
    } else {
        format!("{}-{}", line_range.start, line_range.end)
    }
}

fn language_fence(language: &str) -> &str {
    match language {
        "C++" => "cpp",
        "C#" => "csharp",
        "F#" => "fsharp",
        "Objective-C" => "objective-c",
        "Protocol Buffers" => "protobuf",
        "Visual Basic" => "vb",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use tempfile::tempdir;

    use crate::index::{ByteRange, LineRange};
    use crate::query::{CandidateKind, QueryCandidate};

    use super::{
        explain_candidate_with_cache, parse_dotenv, render_explanation_prompt, AuthStyle,
        ExplanationRequest, LlmConfig, LlmError, LlmProvider, LlmProviderKind, RenderedPrompt,
        DEFAULT_ANTHROPIC_VERSION, DEFAULT_MAX_TOKENS,
    };
    use crate::cache::CacheStore;

    #[test]
    fn parses_local_ollama_spec_without_key() {
        let config = LlmConfig::parse("ollama,qwen2.5-coder").unwrap();
        assert_eq!(config.kind, LlmProviderKind::Ollama);
        assert_eq!(config.model, "qwen2.5-coder");
        assert_eq!(config.base_url, "http://localhost:11434");
        assert!(config.api_key.is_none());
    }

    #[test]
    fn parses_openai_spec_with_key() {
        let config = LlmConfig::parse("openai,gpt-4o-mini,sk-test").unwrap();
        assert_eq!(config.kind, LlmProviderKind::OpenAi);
        assert_eq!(config.model, "gpt-4o-mini");
        assert_eq!(config.api_key.as_deref(), Some("sk-test"));
        assert_eq!(config.base_url, "https://api.openai.com/v1");
    }

    #[test]
    fn an_approved_model_may_run_on_a_custom_endpoint() {
        // The enterprise case: our approved model, their infrastructure. A
        // custom base URL is always allowed; the model still has to be approved.
        let config =
            LlmConfig::parse("openai,gpt-4o-mini,,https://acme.openai.azure.com/v1").unwrap();
        assert_eq!(config.kind, LlmProviderKind::OpenAi);
        assert_eq!(config.base_url, "https://acme.openai.azure.com/v1");
        assert!(config.api_key.is_none());
    }

    /// A repository carrying its own key, as a customer's would.
    fn repository_with_key(key: &str) -> tempfile::TempDir {
        let directory = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            directory.path().join(".env"),
            format!("CODIE_LLM=deepseek,deepseek-chat,{key}\n"),
        )
        .expect("write .env");
        directory
    }

    #[test]
    fn locally_a_repository_env_file_is_honoured() {
        let directory = repository_with_key("sk-from-the-repo");
        let config =
            LlmConfig::from_env_with_policy(directory.path(), None, super::Policy::Local)
                .expect("local config");
        assert_eq!(config.api_key.as_deref(), Some("sk-from-the-repo"));
    }

    #[test]
    fn managed_ignores_a_key_sitting_in_the_audited_repository() {
        // That file belongs to the customer. Honouring it would let them choose
        // which model the operator pays for.
        let directory = repository_with_key("sk-from-the-repo");
        let outcome =
            LlmConfig::from_env_with_policy(directory.path(), None, super::Policy::Managed);

        match outcome {
            // No operator key configured: it refused rather than falling back.
            Err(LlmError::MissingConfig) => {}
            // An operator key exists in this environment — it must not be the
            // repository's.
            Ok(config) => assert_ne!(
                config.api_key.as_deref(),
                Some("sk-from-the-repo"),
                "the repository's key must never be used under a managed policy"
            ),
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn enterprise_also_ignores_the_audited_repository() {
        let directory = repository_with_key("sk-from-the-repo");
        let outcome =
            LlmConfig::from_env_with_policy(directory.path(), None, super::Policy::Enterprise);

        match outcome {
            Err(LlmError::MissingConfig) => {}
            Ok(config) => assert_ne!(config.api_key.as_deref(), Some("sk-from-the-repo")),
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn a_caller_cannot_pick_the_model_once_we_are_running_it() {
        let directory = tempfile::tempdir().expect("temp dir");
        for policy in [super::Policy::Managed, super::Policy::Enterprise] {
            let error = LlmConfig::from_env_with_policy(
                directory.path(),
                Some("deepseek,deepseek-chat,sk-theirs"),
                policy,
            )
            .unwrap_err();
            assert!(
                matches!(error, LlmError::NotPermittedByPolicy { .. }),
                "{policy} accepted --llm: {error:?}"
            );
            assert!(error.to_string().contains(policy.label()), "{error}");
        }
    }

    #[test]
    fn locally_an_explicit_override_still_wins() {
        let directory = repository_with_key("sk-from-the-repo");
        let config = LlmConfig::from_env_with_policy(
            directory.path(),
            Some("deepseek,deepseek-chat,sk-explicit"),
            super::Policy::Local,
        )
        .expect("override accepted");
        assert_eq!(config.api_key.as_deref(), Some("sk-explicit"));
    }

    #[test]
    fn an_azure_endpoint_builds_the_url_and_header_azure_expects() {
        // Azure names the deployment in the path, versions via the query
        // string, and authenticates with `api-key` rather than a bearer token.
        // Getting any one of the three wrong looks like a credential problem.
        let config = LlmConfig {
            kind: LlmProviderKind::OpenAi,
            model: "gpt-4o-mini".to_owned(),
            base_url: "https://acme.openai.azure.com/openai/deployments/gpt4o".to_owned(),
            api_key: Some("secret".to_owned()),
            max_tokens: DEFAULT_MAX_TOKENS,
            anthropic_version: DEFAULT_ANTHROPIC_VERSION.to_owned(),
            auth: AuthStyle::ApiKey,
            extra_query: Some("api-version=2024-06-01".to_owned()),
        };

        assert_eq!(
            config.endpoint_url("chat/completions"),
            "https://acme.openai.azure.com/openai/deployments/gpt4o/chat/completions?api-version=2024-06-01"
        );
        assert_eq!(
            config.auth.header("secret"),
            Some(("api-key".to_owned(), "secret".to_owned()))
        );
    }

    #[test]
    fn a_plain_endpoint_is_unchanged_by_the_new_fields() {
        let config = LlmConfig::parse("deepseek,deepseek-chat,sk-test").unwrap();
        assert_eq!(
            config.endpoint_url("chat/completions"),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(config.auth, AuthStyle::Bearer);
    }

    #[test]
    fn bedrock_is_refused_with_the_gateway_it_should_use_instead() {
        // Request signing is not a header, and a half-implemented SigV4 fails
        // in ways that look like a typo'd key. Say so plainly.
        let error = LlmError::SigningUnsupported {
            scheme: "aws-sigv4".to_owned(),
        };
        let message = error.to_string();
        assert!(message.contains("gateway"), "{message}");
        assert!(message.contains("Bedrock"), "{message}");
    }

    #[test]
    fn an_unapproved_model_is_rejected_even_on_a_custom_endpoint() {
        // Pointing at an arbitrary local model is exactly what the registry
        // exists to prevent: output quality is our reputation.
        let error =
            LlmConfig::parse("openai,mistral-7b,,http://localhost:1234/v1").unwrap_err();
        assert!(
            matches!(error, LlmError::UnapprovedModel { .. }),
            "{error:?}"
        );
        // The error has to tell the operator what they *can* use.
        assert!(error.to_string().contains("gpt-4o-mini"), "{error}");
    }

    #[test]
    fn omitting_the_model_resolves_it_from_the_registry() {
        let config = LlmConfig::parse("ollama").unwrap();
        assert_eq!(config.model, "qwen2.5-coder");
    }

    #[test]
    fn openai_cloud_requires_a_key() {
        let error = LlmConfig::parse("openai,gpt-4o-mini").unwrap_err();
        assert!(
            matches!(error, LlmError::MissingCredential { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn rejects_unknown_provider() {
        assert!(matches!(
            LlmConfig::parse("rancorporated,model").unwrap_err(),
            LlmError::UnknownProvider(_)
        ));
        assert!(matches!(
            LlmConfig::parse("").unwrap_err(),
            LlmError::InvalidConfig(_)
        ));
    }

    #[test]
    fn dotenv_parser_handles_export_and_quotes() {
        let parsed = parse_dotenv(
            r#"
            # ignored
            export CODIE_LLM=ollama,qwen2.5-coder
            OTHER="quoted value"
            "#,
        );

        assert_eq!(
            parsed.get("CODIE_LLM").map(String::as_str),
            Some("ollama,qwen2.5-coder")
        );
        assert_eq!(
            parsed.get("OTHER").map(String::as_str),
            Some("quoted value")
        );
    }

    #[test]
    fn explanation_prompt_forbids_advice_and_uses_only_candidate_snippet() {
        let request = ExplanationRequest::new(sample_candidate());
        let prompt = render_explanation_prompt(&request);

        assert!(prompt.system.contains("Do not suggest fixes"));
        assert!(prompt.user.contains("def verify_token"));
        assert!(!prompt.user.contains("whole repository"));
    }

    #[test]
    fn same_prompt_and_model_hits_llm_response_cache() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::open_for_root(directory.path()).unwrap();
        let calls = Rc::new(Cell::new(0));
        let mut provider = MockProvider {
            calls: Rc::clone(&calls),
            response: "This checks whether a token is present.",
            model: "mock-model".to_owned(),
        };
        let request = ExplanationRequest::new(sample_candidate());
        let mut first = Vec::new();
        let mut second = Vec::new();

        let first_outcome =
            explain_candidate_with_cache(&cache, &request, &mut provider, &mut first).unwrap();
        let second_outcome =
            explain_candidate_with_cache(&cache, &request, &mut provider, &mut second).unwrap();

        assert!(!first_outcome.cache_hit);
        assert!(second_outcome.cache_hit);
        assert_eq!(calls.get(), 1);
        assert_eq!(first, second);
    }

    fn sample_candidate() -> QueryCandidate {
        QueryCandidate {
            kind: CandidateKind::FunctionDefinition,
            label: "verify_token".to_owned(),
            file: "auth.py".into(),
            line_range: LineRange { start: 1, end: 2 },
            byte_range: ByteRange { start: 0, end: 42 },
            snippet: "def verify_token(token):\n    return bool(token)".to_owned(),
            score: None,
            context: "module".to_owned(),
        }
    }

    struct MockProvider {
        calls: Rc<Cell<usize>>,
        response: &'static str,
        model: String,
    }

    impl LlmProvider for MockProvider {
        fn provider_name(&self) -> &str {
            "mock"
        }

        fn model(&self) -> &str {
            &self.model
        }

        fn complete(&mut self, _prompt: &RenderedPrompt) -> Result<String, LlmError> {
            self.calls.set(self.calls.get() + 1);
            Ok(self.response.to_owned())
        }

        fn stream_explanation(
            &mut self,
            _prompt: &RenderedPrompt,
            sink: &mut dyn FnMut(&str) -> Result<(), LlmError>,
        ) -> Result<(), LlmError> {
            self.calls.set(self.calls.get() + 1);
            sink(self.response)
        }
    }
}
