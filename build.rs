fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap() == "windows" {
        let mut res = winres::WindowsResource::new();

        let icon_path = "resources/icon-dark.ico";
        if std::path::Path::new(icon_path).exists() {
            res.set_icon(icon_path);
        } else {
            println!(
                "cargo:warning=Icon file not found: {}, executable will use default icon",
                icon_path
            );
        }

        let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "1.0.0".to_string());
        res.set("CompanyName", "xiaotian2333");
        res.set("FileDescription", "EchoMusic-Lyrics-WinIsland");
        res.set("ProductName", "EchoMusic-Lyrics-WinIsland");
        res.set("FileVersion", &version);
        res.set("ProductVersion", &version);
        res.set("OriginalFilename", "EchoMusic-Lyrics-WinIsland.exe");
        res.set("InternalName", "EchoMusic-Lyrics-WinIsland");
        res.set("LegalCopyright", "Copyright (c) xiaotian2333");

        let manifest = r#"
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>
"#;
        res.set_manifest(manifest);
        res.compile().unwrap();
    }
}
