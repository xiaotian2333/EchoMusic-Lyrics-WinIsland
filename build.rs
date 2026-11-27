fn main() {
    println!("cargo::rustc-check-cfg=cfg(has_builtin_font)");
    let font_path = std::path::PathBuf::from("resources/font.otf");
    if font_path.exists() {
        println!("cargo::rustc-cfg=has_builtin_font");
        println!("cargo::rerun-if-changed=resources/font.otf");
    }
    // fresh clone 尚未构建前端时 frontendDist 不存在会直接构建失败，
    // 这里补一个占位页面，保证纯 Rust 的 check/build 可独立完成
    let dist = std::path::PathBuf::from("settings-ui/dist");
    if !dist.exists() {
        let _ = std::fs::create_dir_all(&dist);
        let _ = std::fs::write(
            dist.join("index.html"),
            "<!doctype html><html><body>settings-ui not built</body></html>",
        );
    }
    tauri_build::build();
}
