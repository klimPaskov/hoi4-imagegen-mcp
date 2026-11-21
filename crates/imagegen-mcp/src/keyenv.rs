//! API keys saved by HOI4 Mod Setup, read from the operating system credential store.
//!
//! An AI client starts `hoi4-imagegen-mcp` itself, so Setup cannot hand it a key through the environment without
//! writing the secret into a project file, which it never does. Setup keeps the keys in the user's credential store
//! (Windows Credential Manager, macOS Keychain, Linux Secret Service) under its own service name, and this module
//! reads them when the matching environment variable is not set. A variable that is set always wins. Nothing is
//! written, cached on disk or logged.
//!
//! On macOS this lookup is off by default. A Keychain item belongs to the program that made it, and this server is a
//! different, ad-hoc signed program, so reading Setup's items makes macOS ask for the login keychain password, which an
//! agent's MCP server cannot answer. Set [`SETUP_KEYCHAIN_ENV`] to `1` to read them anyway (macOS asks once per item and
//! per build; "Always Allow" ends it). Everywhere else the lookup is on. Each key is looked up once per process.

use std::collections::HashMap;
use std::sync::Mutex;

use imagegen_core::config::EnvSource;

/// Environment variable that turns the lookup of Setup's saved keys on (`1`) or off (`0`), overriding the platform default.
pub const SETUP_KEYCHAIN_ENV: &str = "HOI4_IMAGEGEN_SETUP_KEYCHAIN";

/// Whether Setup's credential store is read for provider keys: off on macOS unless asked for, on elsewhere unless turned off.
pub fn setup_store_enabled(env: &dyn EnvSource, macos: bool) -> bool {
    match env.get(SETUP_KEYCHAIN_ENV).as_deref().map(str::trim) {
        Some("1") => true,
        Some("0") => false,
        _ => !macos,
    }
}

/// Setup's credential-store service name.
pub const SETUP_SERVICE: &str = "com.klimpaskov.hoi4-mod-setup";

/// The Setup credential field that holds the key for an environment variable, if Setup manages one.
pub fn setup_field(var: &str) -> Option<&'static str> {
    match var {
        "OPENAI_API_KEY" => Some("openai_api_key"),
        "GEMINI_API_KEY" | "GOOGLE_API_KEY" => Some("gemini_api_key"),
        "FAL_KEY" => Some("fal_key"),
        "COMFYUI_API_KEY" => Some("comfyui_api_key"),
        "REPLICATE_API_TOKEN" => Some("replicate_api_token"),
        "BFL_API_KEY" => Some("bfl_api_key"),
        "STABILITY_API_KEY" => Some("stability_api_key"),
        "IDEOGRAM_API_KEY" => Some("ideogram_api_key"),
        "RECRAFT_API_KEY" => Some("recraft_api_key"),
        "LEONARDO_API_KEY" => Some("leonardo_api_key"),
        "XAI_API_KEY" => Some("xai_api_key"),
        "TOGETHER_API_KEY" => Some("together_api_key"),
        "OPENROUTER_API_KEY" => Some("openrouter_api_key"),
        "OPENAI_COMPATIBLE_API_KEY" => Some("openai_compatible_api_key"),
        "HF_TOKEN" => Some("hf_token"),
        _ => None,
    }
}

/// The credential-store account Setup writes a field under (its `image_generation_credential_reference`).
pub fn setup_account(field: &str) -> String {
    format!("credential://image_generation_api_key/hoi4_imagegen_{field}")
}

/// An environment that falls back to Setup's credential store for the provider keys.
pub struct SetupKeyEnv<E: EnvSource> {
    inner: E,
    enabled: bool,
    cache: Mutex<HashMap<String, Option<String>>>,
}

impl<E: EnvSource> SetupKeyEnv<E> {
    /// Wrap an environment.
    pub fn new(inner: E) -> Self {
        let enabled = setup_store_enabled(&inner, cfg!(target_os = "macos"));
        SetupKeyEnv { inner, enabled, cache: Mutex::new(HashMap::new()) }
    }
}

impl<E: EnvSource> EnvSource for SetupKeyEnv<E> {
    fn get(&self, key: &str) -> Option<String> {
        if let Some(v) = self.inner.get(key) {
            return Some(v);
        }
        let field = setup_field(key)?;
        if !self.enabled {
            return None;
        }
        let mut cache = self.cache.lock().ok()?;
        cache
            .entry(field.to_string())
            .or_insert_with(|| keyring::Entry::new(SETUP_SERVICE, &setup_account(field)).ok()?.get_password().ok().filter(|v| !v.is_empty()))
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use imagegen_core::config::MapEnv;

    #[test]
    fn only_provider_keys_map_to_setup_fields() {
        assert_eq!(setup_field("OPENAI_API_KEY"), Some("openai_api_key"));
        assert_eq!(setup_field("GOOGLE_API_KEY"), Some("gemini_api_key"));
        assert_eq!(setup_field("FAL_KEY"), Some("fal_key"));
        assert_eq!(setup_field("COMFYUI_API_KEY"), Some("comfyui_api_key"));
        assert_eq!(setup_field("REPLICATE_API_TOKEN"), Some("replicate_api_token"));
        assert_eq!(setup_field("AWS_SECRET_ACCESS_KEY"), None, "Bedrock is deferred");
        assert_eq!(setup_field("HF_TOKEN"), Some("hf_token"));
        assert_eq!(setup_field("HOME"), None);
        assert_eq!(setup_account("fal_key"), "credential://image_generation_api_key/hoi4_imagegen_fal_key");
    }

    #[test]
    fn the_setup_store_is_off_on_macos_unless_asked_for() {
        let none = MapEnv::from_pairs(&[]);
        assert!(!setup_store_enabled(&none, true), "macOS must not touch the Keychain by default");
        assert!(setup_store_enabled(&none, false));
        assert!(setup_store_enabled(&MapEnv::from_pairs(&[(SETUP_KEYCHAIN_ENV, "1")]), true));
        assert!(!setup_store_enabled(&MapEnv::from_pairs(&[(SETUP_KEYCHAIN_ENV, "0")]), false));
    }

    #[test]
    fn a_set_variable_wins_and_unrelated_names_never_touch_the_store() {
        let env = SetupKeyEnv::new(MapEnv::from_pairs(&[("OPENAI_API_KEY", "sk-synthetic"), ("HOME", "/h")]));
        assert_eq!(env.get("OPENAI_API_KEY").as_deref(), Some("sk-synthetic"));
        assert_eq!(env.get("HOME").as_deref(), Some("/h"));
        assert_eq!(env.get("PATH_NOT_SET"), None);
    }
}
