//! Configuration module for Synapse.
//!
//! Loads configuration from TOML files with environment variable substitution.
//!
//! # Example
//!
//! ```toml
//! [server]
//! port = 3000
//!
//! [routes]
//! "game.*" = ["log:game", "webhook:discord"]
//!
//! [effects.webhook.discord]
//! url = "${DISCORD_WEBHOOK_URL}"
//! ```

use regex::Regex;
use serde::Deserialize;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::Path;
use thiserror::Error;
use tracing::{debug, info, warn};

/// Configuration errors
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Failed to read config file: {0}")]
    ReadError(#[from] std::io::Error),

    #[error("Failed to parse TOML: {0}")]
    ParseError(#[from] toml::de::Error),

    #[error("Validation error: {0}")]
    ValidationError(String),

    #[error("Missing required field: {0}")]
    MissingField(String),
}

/// Root configuration structure
#[derive(Debug, Deserialize, Clone, Default)]
pub struct SynapseConfig {
    #[serde(default)]
    pub server: ServerConfig,

    #[serde(default)]
    pub redis: RedisConfig,

    #[serde(default)]
    pub worker: WorkerConfig,

    /// Event routing rules: pattern -> [effect_names]
    #[serde(default)]
    pub routes: HashMap<String, Vec<String>>,

    /// Effect configurations
    #[serde(default)]
    pub effects: EffectsConfig,
}

/// Server configuration
#[derive(Debug, Deserialize, Clone)]
pub struct ServerConfig {
    #[serde(default = "default_port")]
    pub port: u16,

    #[serde(default)]
    pub api_key: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: default_port(),
            api_key: None,
        }
    }
}

fn default_port() -> u16 {
    3000
}

/// Redis configuration
#[derive(Debug, Deserialize, Clone)]
pub struct RedisConfig {
    #[serde(default = "default_redis_url")]
    pub url: String,
}

impl Default for RedisConfig {
    fn default() -> Self {
        Self {
            url: default_redis_url(),
        }
    }
}

fn default_redis_url() -> String {
    "redis://localhost:6379".to_string()
}

/// Worker configuration
#[derive(Debug, Deserialize, Clone)]
pub struct WorkerConfig {
    #[serde(default)]
    pub name: Option<String>,

    #[serde(default = "default_consumer_group")]
    pub consumer_group: String,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            name: None,
            consumer_group: default_consumer_group(),
        }
    }
}

fn default_consumer_group() -> String {
    "synapse_workers".to_string()
}

/// Effects configuration container
#[derive(Debug, Deserialize, Clone, Default)]
pub struct EffectsConfig {
    #[serde(default)]
    pub log: HashMap<String, LogEffectConfig>,

    #[serde(default)]
    pub webhook: HashMap<String, WebhookEffectConfig>,
}

/// Log effect configuration
#[derive(Debug, Deserialize, Clone)]
pub struct LogEffectConfig {
    #[serde(default = "default_log_prefix")]
    pub prefix: String,
}

fn default_log_prefix() -> String {
    "synapse".to_string()
}

/// Webhook effect configuration
#[derive(Debug, Deserialize, Clone)]
pub struct WebhookEffectConfig {
    pub url: String,

    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,

    #[serde(default = "default_retries")]
    pub retries: u32,

    /// Format: "json" (default) or "discord"
    #[serde(default = "default_format")]
    pub format: String,
}

fn default_timeout_ms() -> u64 {
    10000
}

fn default_retries() -> u32 {
    2
}

fn default_format() -> String {
    "json".to_string()
}

impl SynapseConfig {
    /// Build a Router from the configuration.
    ///
    /// Creates effects based on the effects config and registers them
    /// for the patterns defined in routes.
    pub fn build_router(&self) -> crate::Router {
        use crate::effects::{LogEffect, WebhookEffect};
        use std::sync::Arc;
        use std::time::Duration;
        use tracing::warn;

        let mut router = crate::Router::new();

        // Build effect instances from config
        let mut log_effects: std::collections::HashMap<String, Arc<dyn crate::Effect>> =
            std::collections::HashMap::new();
        let mut webhook_effects: std::collections::HashMap<String, Arc<dyn crate::Effect>> =
            std::collections::HashMap::new();

        // Create log effects
        for (name, config) in &self.effects.log {
            log_effects.insert(
                name.clone(),
                Arc::new(LogEffect::with_prefix(&config.prefix)),
            );
        }

        // Create webhook effects
        for (name, config) in &self.effects.webhook {
            // Skip webhooks with unsubstituted env vars
            if config.url.contains("${") {
                warn!(
                    webhook = %name,
                    "Skipping webhook with unsubstituted URL: {}",
                    config.url
                );
                continue;
            }

            let mut effect = WebhookEffect::new(&config.url)
                .with_timeout(Duration::from_millis(config.timeout_ms))
                .with_retries(config.retries);

            // Store format for later use in Discord formatting
            if config.format == "discord" {
                effect = effect.with_discord_format();
            }

            webhook_effects.insert(name.clone(), Arc::new(effect));
        }

        // Register routes
        for (pattern, effect_refs) in &self.routes {
            for effect_ref in effect_refs {
                if let Some((effect_type, name)) = effect_ref.split_once(':') {
                    match effect_type {
                        "log" => {
                            if let Some(effect) = log_effects.get(name) {
                                router.on(pattern, effect.clone());
                            } else {
                                warn!(
                                    pattern = %pattern,
                                    effect = %effect_ref,
                                    "Log effect '{}' not found in config",
                                    name
                                );
                            }
                        }
                        "webhook" => {
                            if let Some(effect) = webhook_effects.get(name) {
                                router.on(pattern, effect.clone());
                            } else {
                                warn!(
                                    pattern = %pattern,
                                    effect = %effect_ref,
                                    "Webhook effect '{}' not found in config",
                                    name
                                );
                            }
                        }
                        _ => {
                            warn!(
                                pattern = %pattern,
                                effect = %effect_ref,
                                "Unknown effect type: {}",
                                effect_type
                            );
                        }
                    }
                } else if effect_ref == "log" {
                    // Bare "log" means default log effect
                    router.on(pattern, Arc::new(LogEffect::new()));
                } else {
                    warn!(
                        pattern = %pattern,
                        effect = %effect_ref,
                        "Invalid effect reference format"
                    );
                }
            }
        }

        // Set default handler if catch-all isn't registered
        if !router.has_handlers("__any_random_event__") {
            router.set_default(Arc::new(LogEffect::with_prefix("unhandled")));
        }

        router
    }

    /// Load configuration from the default path or SYNAPSE_CONFIG env var.
    pub fn load() -> Result<Self, ConfigError> {
        let config_path =
            env::var("SYNAPSE_CONFIG").unwrap_or_else(|_| "config/synapse.toml".to_string());

        Self::load_from(&config_path)
    }

    /// Load configuration from a specific path.
    pub fn load_from<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        let path = path.as_ref();

        if !path.exists() {
            info!(
                path = %path.display(),
                "Config file not found, using defaults"
            );
            return Ok(Self::default());
        }

        info!(path = %path.display(), "Loading configuration");

        let content = fs::read_to_string(path)?;
        let content = substitute_env_vars(&content);

        debug!("Parsing TOML configuration");
        let config: SynapseConfig = toml::from_str(&content)?;

        config.validate()?;

        info!(
            routes = config.routes.len(),
            webhook_effects = config.effects.webhook.len(),
            log_effects = config.effects.log.len(),
            "Configuration loaded"
        );

        Ok(config)
    }

    /// Validate the configuration
    fn validate(&self) -> Result<(), ConfigError> {
        // Validate webhook URLs
        for (name, webhook) in &self.effects.webhook {
            if webhook.url.is_empty() {
                return Err(ConfigError::ValidationError(format!(
                    "Webhook '{}' has empty URL",
                    name
                )));
            }

            // Check for unsubstituted env vars
            if webhook.url.contains("${") {
                warn!(
                    webhook = %name,
                    url = %webhook.url,
                    "Webhook URL contains unsubstituted environment variable"
                );
            }

            // Validate URL format
            if !webhook.url.starts_with("http://") && !webhook.url.starts_with("https://") {
                return Err(ConfigError::ValidationError(format!(
                    "Webhook '{}' URL must start with http:// or https://",
                    name
                )));
            }

            // Validate format
            if webhook.format != "json" && webhook.format != "discord" {
                return Err(ConfigError::ValidationError(format!(
                    "Webhook '{}' format must be 'json' or 'discord'",
                    name
                )));
            }
        }

        // Validate route effect references
        for (pattern, effects) in &self.routes {
            for effect_ref in effects {
                if !self.effect_exists(effect_ref) {
                    warn!(
                        pattern = %pattern,
                        effect = %effect_ref,
                        "Route references undefined effect (will use default)"
                    );
                }
            }
        }

        Ok(())
    }

    /// Check if an effect reference exists in configuration
    fn effect_exists(&self, effect_ref: &str) -> bool {
        if let Some((effect_type, name)) = effect_ref.split_once(':') {
            match effect_type {
                "log" => self.effects.log.contains_key(name),
                "webhook" => self.effects.webhook.contains_key(name),
                _ => false,
            }
        } else {
            // Bare "log" means default log effect
            effect_ref == "log"
        }
    }

    /// Get all route patterns
    pub fn route_patterns(&self) -> Vec<&str> {
        self.routes.keys().map(|s| s.as_str()).collect()
    }
}

/// Substitute environment variables in the format ${VAR_NAME}
fn substitute_env_vars(content: &str) -> String {
    let re = Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}").unwrap();

    re.replace_all(content, |caps: &regex::Captures| {
        let var_name = &caps[1];
        match env::var(var_name) {
            Ok(value) => value,
            Err(_) => {
                debug!(var = %var_name, "Environment variable not set, keeping placeholder");
                caps[0].to_string()
            }
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_env_var_substitution() {
        env::set_var("TEST_VAR", "substituted_value");
        let input = "url = \"${TEST_VAR}\"";
        let output = substitute_env_vars(input);
        assert_eq!(output, "url = \"substituted_value\"");
        env::remove_var("TEST_VAR");
    }

    #[test]
    fn test_env_var_not_set() {
        let input = "url = \"${NONEXISTENT_VAR}\"";
        let output = substitute_env_vars(input);
        assert_eq!(output, "url = \"${NONEXISTENT_VAR}\"");
    }

    #[test]
    fn test_parse_minimal_config() {
        let toml = r#"
            [server]
            port = 4000
        "#;

        let config: SynapseConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.server.port, 4000);
        assert_eq!(config.redis.url, "redis://localhost:6379");
    }

    #[test]
    fn test_parse_routes() {
        let toml = r#"
            [routes]
            "user.created" = ["log:user"]
            "game.*" = ["log:game", "webhook:discord"]
        "#;

        let config: SynapseConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.routes.len(), 2);
        assert_eq!(
            config.routes.get("user.created"),
            Some(&vec!["log:user".to_string()])
        );
        assert_eq!(
            config.routes.get("game.*"),
            Some(&vec!["log:game".to_string(), "webhook:discord".to_string()])
        );
    }

    #[test]
    fn test_parse_effects() {
        let toml = r#"
            [effects.log.game]
            prefix = "game"

            [effects.webhook.discord]
            url = "https://discord.com/api/webhooks/test"
            timeout_ms = 5000
            retries = 3
            format = "discord"
        "#;

        let config: SynapseConfig = toml::from_str(toml).unwrap();

        let log_config = config.effects.log.get("game").unwrap();
        assert_eq!(log_config.prefix, "game");

        let webhook_config = config.effects.webhook.get("discord").unwrap();
        assert_eq!(webhook_config.url, "https://discord.com/api/webhooks/test");
        assert_eq!(webhook_config.timeout_ms, 5000);
        assert_eq!(webhook_config.retries, 3);
        assert_eq!(webhook_config.format, "discord");
    }

    #[test]
    fn test_default_config() {
        let config = SynapseConfig::default();
        assert_eq!(config.server.port, 3000);
        assert_eq!(config.redis.url, "redis://localhost:6379");
        assert_eq!(config.worker.consumer_group, "synapse_workers");
    }

    #[test]
    fn test_validation_invalid_url() {
        let toml = r#"
            [effects.webhook.bad]
            url = "not-a-url"
        "#;

        let config: SynapseConfig = toml::from_str(toml).unwrap();
        let result = config.validate();
        assert!(result.is_err());
    }

    #[test]
    fn test_validation_invalid_format() {
        let toml = r#"
            [effects.webhook.bad]
            url = "https://example.com"
            format = "invalid"
        "#;

        let config: SynapseConfig = toml::from_str(toml).unwrap();
        let result = config.validate();
        assert!(result.is_err());
    }
}
