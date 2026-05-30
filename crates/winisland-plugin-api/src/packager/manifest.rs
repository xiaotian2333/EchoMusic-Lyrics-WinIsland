use serde::{Deserialize, Serialize};
use std::path::Path;

/// Represents a `plugin.yml` manifest for a WinIsland plugin.
///
/// This struct is serialised to YAML when packaging a plugin,
/// and deserialised by the WinIsland host when loading a plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub author: String,
    pub version: String,
    pub description: String,
    #[serde(rename = "github-link")]
    pub github_link: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dll_hashes: Option<Vec<String>>,
}

impl PluginManifest {
    /// Build the signing payload: canonical JSON of all fields except `signature`.
    pub fn signing_payload(&self) -> String {
        let payload = serde_json::json!({
            "name": self.name,
            "author": self.author,
            "version": self.version,
            "description": self.description,
            "github-link": self.github_link,
            "dll_hashes": self.dll_hashes,
        });
        serde_json::to_string(&payload).unwrap_or_default()
    }

    /// Write the manifest to a `plugin.yml` file.
    pub fn write_to_yaml(&self, path: &Path) -> Result<(), String> {
        let yaml = serde_yaml::to_string(self)
            .map_err(|e| format!("Failed to serialise manifest: {}", e))?;
        std::fs::write(path, &yaml).map_err(|e| format!("Failed to write manifest: {}", e))
    }

    /// Compute a safe directory name from the plugin name.
    pub fn safe_dir_name(&self) -> String {
        self.name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }

    /// Validate required fields are non-empty.
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("'name' is empty".into());
        }
        if self.author.trim().is_empty() {
            return Err("'author' is empty".into());
        }
        if self.version.trim().is_empty() {
            return Err("'version' is empty".into());
        }
        if self.description.trim().is_empty() {
            return Err("'description' is empty".into());
        }
        if self.github_link.trim().is_empty() {
            return Err("'github-link' is empty".into());
        }
        Ok(())
    }
}
