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

## 贡献

我们欢迎任何形式的贡献！

如果你有精力或兴趣，欢迎提交 PR。

> [!IMPORTANT]
> 所有未遵守[贡献指南](CONTRIBUTING.md)的PR将会被close

## 许可证

[MIT](LICENSE)

# 致谢

EchoMusic-Lyrics-WinIsland 是基于 [WinIsland](https://github.com/Eatgrapes/WinIsland/tree/ab7254285b2532441b0f69a2a050fcce478bead7) 的硬 fork 。感谢原项目作者的贡献！
