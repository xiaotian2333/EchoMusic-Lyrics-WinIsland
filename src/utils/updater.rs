use crate::core::config::APP_VERSION;
use crate::core::i18n::tr;
use once_cell::sync::Lazy;
use reqwest::header::{ACCEPT, USER_AGENT};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use windows::Win32::UI::WindowsAndMessaging::{
    IDOK, IDYES, MB_ICONINFORMATION, MB_OKCANCEL, MB_SETFOREGROUND, MB_TOPMOST, MessageBoxW,
};
use windows::core::PCWSTR;

static HTTP_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap()
});

const GITHUB_RELEASES_API: &str =
    "https://api.github.com/repos/xiaotian2333/EchoMusic-Lyrics-WinIsland/releases?per_page=10";
const GITHUB_API_VERSION: &str = "2022-11-28";
const ASSET_PREFIX: &str = "EchoMusic-Lyrics-WinIsland";

#[derive(Deserialize, Debug, Clone)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize, Debug, Clone)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    size: Option<u64>,
}

#[derive(Debug, Clone)]
struct UpdateCandidate {
    tag_name: String,
    published_at: Option<String>,
    asset_name: String,
    download_url: String,
    asset_size: Option<u64>,
    version: [u64; 3],
}

pub fn get_app_dir() -> PathBuf {
    let mut path = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push(".echomusic-lyrics-winisland");
    if !path.exists() {
        let _ = fs::create_dir_all(&path);
    }
    path
}

pub fn start_update_checker() {
    tokio::spawn(async move {
        let app_dir = get_app_dir();
        let mut last_check = tokio::time::Instant::now();

        // 启动后先检查一次更新。
        if crate::core::persistence::load_config().check_for_updates {
            do_check(&app_dir).await;
            last_check = tokio::time::Instant::now();
        }

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let config = crate::core::persistence::load_config();
            if !config.check_for_updates {
                continue;
            }

            let interval_secs = config.update_check_interval * 3600.0;
            if last_check.elapsed().as_secs_f32() >= interval_secs {
                do_check(&app_dir).await;
                last_check = tokio::time::Instant::now();
            }
        }
    });
}

async fn do_check(app_dir: &Path) {
    let Some(candidate) = fetch_latest_update().await else {
        return;
    };

    let title_w: Vec<u16> = format!("{}\0", tr("update_available_title"))
        .encode_utf16()
        .collect();
    let date = candidate.published_at.as_deref().unwrap_or("-");
    let text_w: Vec<u16> = tr("update_available_desc")
        .replace("{version}", &candidate.tag_name)
        .replace("{date}", date)
        .add_null()
        .encode_utf16()
        .collect();

    // SAFETY: MessageBoxW displays a modal dialog. The text and title are
    // null-terminated UTF-16 strings allocated in the outer scope and moved
    // into the closure. None hWnd makes it a top-level message box.
    let result = tokio::task::spawn_blocking(move || unsafe {
        MessageBoxW(
            None,
            PCWSTR(text_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            MB_OKCANCEL | MB_ICONINFORMATION | MB_TOPMOST | MB_SETFOREGROUND,
        )
    })
    .await;

    if let Ok(r) = result
        && (r == IDOK || r == IDYES)
    {
        perform_update(candidate, app_dir.to_path_buf()).await;
    }
}

async fn fetch_latest_update() -> Option<UpdateCandidate> {
    let asset_name = expected_asset_name()?;
    let local_version = parse_version(APP_VERSION)?;
    let resp = HTTP_CLIENT
        .get(GITHUB_RELEASES_API)
        .header(ACCEPT, "application/vnd.github+json")
        .header(USER_AGENT, user_agent())
        .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }

    let releases: Vec<GitHubRelease> = resp.json().await.ok()?;
    let mut best: Option<UpdateCandidate> = None;

    for release in releases {
        if release.draft || release.prerelease {
            continue;
        }
        let version = match parse_version(&release.tag_name) {
            Some(version) if version > local_version => version,
            _ => continue,
        };
        let Some(asset) = release.assets.iter().find(|asset| asset.name == asset_name) else {
            continue;
        };
        if asset.browser_download_url.trim().is_empty() {
            continue;
        }

        let candidate = UpdateCandidate {
            tag_name: release.tag_name,
            published_at: release.published_at,
            asset_name: asset.name.clone(),
            download_url: asset.browser_download_url.clone(),
            asset_size: asset.size,
            version,
        };
        let should_replace = match &best {
            Some(current) => candidate.version > current.version,
            None => true,
        };
        if should_replace {
            best = Some(candidate);
        }
    }

    best
}

async fn perform_update(candidate: UpdateCandidate, app_dir: PathBuf) {
    let response = match HTTP_CLIENT
        .get(&candidate.download_url)
        .header(USER_AGENT, user_agent())
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => resp,
        _ => {
            show_error_box(tr("update_failed_title"), tr("update_failed_dl")).await;
            return;
        }
    };

    let bytes = match response.bytes().await {
        Ok(b) => b.to_vec(),
        Err(_) => {
            show_error_box(tr("update_failed_title"), tr("update_failed_dl")).await;
            return;
        }
    };
    if let Some(expected_size) = candidate.asset_size
        && expected_size > 0
        && bytes.len() as u64 != expected_size
    {
        show_error_box(tr("update_failed_title"), tr("update_failed_dl")).await;
        return;
    }

    let current_exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(_) => {
            show_error_box(tr("update_failed_title"), tr("update_failed_save")).await;
            return;
        }
    };
    let new_exe_path = current_exe.with_extension("exe.new");

    if fs::write(&new_exe_path, &bytes).is_err() {
        show_error_box(tr("update_failed_title"), tr("update_failed_save")).await;
        return;
    }

    let local_json_path = app_dir.join("version_info.json");
    let local_state = serde_json::json!({
        "tag_name": candidate.tag_name,
        "published_at": candidate.published_at,
        "asset_name": candidate.asset_name,
    });
    if let Ok(json) = serde_json::to_string_pretty(&local_state) {
        let _ = fs::write(local_json_path, json);
    }

    let current_exe_str = current_exe.to_string_lossy().into_owned();
    let new_exe_str = new_exe_path.to_string_lossy().into_owned();

    // Escape single quotes for PowerShell: '' -> ''
    let ps_escape = |s: &str| s.replace('\'', "''");

    let pid = std::process::id();
    let script = format!(
        "Start-Sleep -Seconds 1; \
         while (Get-Process -Id {} -ErrorAction SilentlyContinue) {{ Start-Sleep -Milliseconds 100 }}; \
         Move-Item -Path '{}' -Destination '{}' -Force; \
         Start-Process -FilePath '{}'",
        pid,
        ps_escape(&new_exe_str),
        ps_escape(&current_exe_str),
        ps_escape(&current_exe_str)
    );

    let _ = Command::new("powershell")
        .args(["-WindowStyle", "Hidden", "-Command", &script])
        .spawn();

    std::process::exit(0);
}

fn parse_version(value: &str) -> Option<[u64; 3]> {
    let value = value
        .strip_prefix('v')
        .or_else(|| value.strip_prefix('V'))
        .unwrap_or(value);
    let mut parts = value.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some([major, minor, patch])
}

fn arch_name_for(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" => Some("AMD64"),
        "aarch64" => Some("ARM64"),
        "x86" => Some("X86"),
        _ => None,
    }
}

fn expected_asset_name() -> Option<String> {
    expected_asset_name_for_arch(std::env::consts::ARCH)
}

fn expected_asset_name_for_arch(arch: &str) -> Option<String> {
    arch_name_for(arch).map(|arch| format!("{ASSET_PREFIX}-{arch}.exe"))
}

fn user_agent() -> String {
    format!("EchoMusic-Lyrics-WinIsland/{APP_VERSION}")
}

async fn show_error_box(title: String, text: String) {
    let title_w: Vec<u16> = title.add_null().encode_utf16().collect();
    let text_w: Vec<u16> = text.add_null().encode_utf16().collect();
    // SAFETY: MessageBoxW displays a modal error dialog with the provided
    // null-terminated UTF-16 strings. All pointers are valid for the call duration.
    tokio::task::spawn_blocking(move || unsafe {
        MessageBoxW(
            None,
            PCWSTR(text_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            MB_ICONINFORMATION | MB_TOPMOST,
        );
    })
    .await
    .ok();
}

trait AddNull {
    fn add_null(&self) -> String;
}
impl AddNull for String {
    fn add_null(&self) -> String {
        format!("{}\0", self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_accepts_plain_and_v_tags() {
        assert_eq!(parse_version("1.2.3"), Some([1, 2, 3]));
        assert_eq!(parse_version("v1.2.3"), Some([1, 2, 3]));
        assert_eq!(parse_version("V1.2.3"), Some([1, 2, 3]));
    }

    #[test]
    fn parse_version_rejects_non_release_tags() {
        assert_eq!(parse_version("nightly"), None);
        assert_eq!(parse_version("v1.2.3-beta"), None);
        assert_eq!(parse_version("v1.2"), None);
    }

    #[test]
    fn version_tuple_orders_numerically() {
        assert!(parse_version("v1.2.10") > parse_version("1.2.9"));
        assert!(parse_version("v2.0.0") > parse_version("1.99.99"));
        assert_eq!(parse_version("v1.0.0"), parse_version("1.0.0"));
    }

    #[test]
    fn asset_name_uses_release_arch_names() {
        assert_eq!(
            expected_asset_name_for_arch("x86_64").as_deref(),
            Some("EchoMusic-Lyrics-WinIsland-AMD64.exe")
        );
        assert_eq!(
            expected_asset_name_for_arch("aarch64").as_deref(),
            Some("EchoMusic-Lyrics-WinIsland-ARM64.exe")
        );
        assert_eq!(
            expected_asset_name_for_arch("x86").as_deref(),
            Some("EchoMusic-Lyrics-WinIsland-X86.exe")
        );
    }
}
