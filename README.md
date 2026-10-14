# EchoMusic-Lyrics-WinIsland

> [!WARNING]
> 该项目仍在开发中，可能会出现错误。

这是一个可以在 Windows 上以灵动岛形式显示歌词的项目。  
需要配合 [EchoMusic-Lyrics-bridge](https://github.com/xiaotian2333/Lyrics-bridge) 使用。

## 下载

你可以在 [Release](https://github.com/xiaotian2333/EchoMusic-Lyrics-WinIsland/releases) 下载 EchoMusic-Lyrics-WinIsland 的最新版本。

## 构建项目

### 环境要求

- **Rust** 环境
- **Cargo**
- **Node.js** (构建设置页前端)

### 构建步骤

```cmd
git clone https://github.com/xiaotian2333/EchoMusic-Lyrics-WinIsland.git

cd EchoMusic-Lyrics-WinIsland
```

**1. 编译设置页前端**

```cmd
cd settings-ui
npm install
npm run build
cd ..
```

**2. 编译 Rust 后端**

```cmd
cargo build --release
```

## 内置字体

EchoMusic-Lyrics-WinIsland 支持将自定义 TTF 字体编译时嵌入为默认字体，替换系统字体（Microsoft YaHei / Segoe UI）。

### 本地构建

将 TTF 字体文件放入 `resources/font.ttf`，然后正常构建即可自动嵌入。

> 字体文件已加入 `.gitignore`，不会被 git 追踪。未放置字体文件时，程序使用系统默认字体作为后备。

### CI 发布构建

在 GitHub 仓库 → Settings → Secrets and variables → Actions → Variables 中设置 `BUILTIN_FONT_URL` 为字体文件的直链下载地址，CI 在发布构建时会自动下载并嵌入产物中。

## 贡献

我们欢迎任何形式的贡献！

如果你有精力或兴趣，欢迎提交 PR。

> [!IMPORTANT]
> 所有未遵守[贡献指南](CONTRIBUTING.md)的PR将会被close

## 许可证

本项目以 [GNU General Public License v3.0](LICENSE) 发布。

Copyright (C) 2026 EchoMusic-Lyrics-WinIsland contributors

# 致谢

EchoMusic-Lyrics-WinIsland 是基于 [WinIsland](https://github.com/Eatgrapes/WinIsland/tree/ab7254285b2532441b0f69a2a050fcce478bead7) 的硬 fork 。感谢原项目作者的贡献！
