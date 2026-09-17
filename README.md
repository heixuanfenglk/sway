# Sway · 全局代理

基于 SSH 动态转发的系统级全局代理工具：支持多主机切换，提供 SOCKS5 / HTTP 本地代理，并可一键写入系统代理。

## 平台支持

| 平台 | UI | SSH 隧道 | 系统代理 |
|------|----|----------|----------|
| Windows | ✅ | ✅ | ✅ |
| macOS | ✅ | ✅ | ✅（networksetup） |
| Linux | ✅ | ✅ | 需手动配置 |

> **说明**：无法在 Windows 上直接交叉编译出可运行的 macOS 程序。请在 Mac 本机编译，或使用 GitHub Actions。

## 在 Mac 上编译

```bash
# 安装 Rust：https://rustup.rs
git clone <your-repo-url> sway
cd sway
cargo build --release

# 产物
./target/release/sway
```

可选：打成简易 `.app` 再运行：

```bash
mkdir -p Sway.app/Contents/MacOS
cp target/release/sway Sway.app/Contents/MacOS/Sway
chmod +x Sway.app/Contents/MacOS/Sway
open Sway.app
```

首次运行若提示无法打开，可在「系统设置 → 隐私与安全性」允许，或执行：

```bash
xattr -cr Sway.app
```

## 用 GitHub Actions 出包

仓库已包含 `.github/workflows/build.yml`：

1. 把代码推到 GitHub
2. 在 Actions 里手动运行 **build**，或打 tag：`git tag v0.1.0 && git push --tags`
3. 下载产物：`Sway-macos-arm64`（Apple Silicon）或 `Sway-macos-x64`（Intel）

## Windows 运行

```bash
cargo run --release
```

产物：`target/release/sway.exe`

> **Windows Server / 远程桌面**：界面使用 DirectX 12（wgpu），不再依赖 OpenGL，可在 Server 与 RDP 会话中正常显示。

## 功能

- 多主机档案：新建 / 复制 / 删除 / 保存，连接页快速切换
- SSH 密码 / 私钥认证
- 本地 SOCKS5（默认 `1080`）与 HTTP（默认 `7890`）
- 连接路径与实时流量
- 可选自动设置系统代理
- 界面分栏：连接 / 主机 / 设置

配置目录：

- Windows: `%APPDATA%\sway\config.toml`
- macOS: `~/Library/Application Support/sway/config.toml`

（若存在旧版 `sslink` 或 `sshtools` 配置，会自动迁移到 `sway`。）  
