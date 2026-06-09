#[cfg(windows)]
#[path = "updater_windows.rs"]
mod imp;

#[cfg(not(windows))]
mod imp {
    use crate::core::config::APP_HOMEPAGE;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};

    static MANUAL_CHECK_RUNNING: AtomicBool = AtomicBool::new(false);

    pub fn get_app_dir() -> PathBuf {
        let mut path = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        path.push(".echomusic-lyrics-winisland");
        if !path.exists() {
            let _ = fs::create_dir_all(&path);
        }
        path
    }

    pub fn start_update_checker() {
        // TODO: macOS/Linux 打包产物和安装方式确定后，再实现跨平台更新检查与提示。
        let _ = get_app_dir();
    }

    pub fn check_for_updates_now() {
        if MANUAL_CHECK_RUNNING
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        let _ = open::that(APP_HOMEPAGE);
        MANUAL_CHECK_RUNNING.store(false, Ordering::Release);
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
            _ => None,
        }
    }

    fn expected_asset_name_for_arch(arch: &str) -> Option<String> {
        arch_name_for(arch).map(|arch| format!("EchoMusic-Lyrics-WinIsland-{arch}.exe"))
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
        }
    }
}

pub use self::imp::*;
