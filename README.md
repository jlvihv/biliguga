# 哔哩咕嘎（biliguga）

一个使用 Rust + Rune + libmpv 实现的 B 站桌面客户端。

当前版本包含：

- 首页推荐视频和分页加载
- 视频搜索
- 登录、历史、收藏、稍后再看和动态
- libmpv 视频播放
- 播放进度上报和云端续播
- 评论、点赞、投币和收藏
- 窗口内全屏和屏幕全屏

## 界面

窗口分成两块：左边的图标导航栏、右边的浏览栏（推荐网格 / 搜索 / 各分栏列表）。没有播放视频时
浏览栏铺满窗口；点开一个视频后播放器独占导航栏右边的整块区域，不再有贴着播放器的列表栏。
网格按窗口宽度自己在 2–5 列之间取列数。推荐流在「离已加载内容的底部还有一屏」时预取下一页，
判断依据是滚动位置到底部的距离，并且**在应用本帧事件之前**判断：一页落地会让列表变长，但 `max`
还是上一帧布局的值，若在落地之后再判断，看起来就仍然在底部，于是不停地翻页。视口第一次布局完成
之前不做任何判断（那时 `max` 为 0，会被误读成"到底了"）。滚轮一格滚 100pt（约半张卡片），
而不是固定 40pt。

播放器独占窗口：16:9 的画面左右顶格铺满整栏（没有侧边距、没有圆角、没有描边），下面是进度条、播放控制、点赞/投币/收藏和视频信息，
再下面是评论，整列一起滚动。全屏交给系统，画面之外的控件浮在画面上下沿。

## 快捷键

| 按键 | 作用 |
| --- | --- |
| 空格 | 播放 / 暂停 |
| ← → | 后退 / 前进 10 秒 |
| ↑ ↓ | 音量 ±5 |
| F | 全屏 |
| Esc | 退出全屏；窗口内则关闭播放器回到浏览 |
| Ctrl+Enter | 搜索框内容重新搜索 |

## 平台支持

项目代码支持 Linux、Windows 和 macOS。播放器依赖系统的 libmpv，发布包中的程序需要对应平台的运行库。

| 平台 | 构建依赖 | 运行时依赖 |
| --- | --- | --- |
| Linux | Rust 1.99.1、`libmpv-dev`、`libxkbcommon-dev`、`libwayland-dev` | `libmpv.so`、Vulkan 或 OpenGL 驱动；支持 X11 和 Wayland 会话 |
| Windows | Rust MSVC toolchain、Visual Studio Build Tools、mpv-dev | `libmpv-2.dll` 及其依赖 |
| macOS | Rust 1.99.1、Homebrew `mpv` | Homebrew mpv 的动态库 |

## 本地构建

Linux：

```bash
sudo apt install libmpv-dev pkg-config
cargo run --release
```

macOS：

```bash
brew install mpv
LIBRARY_PATH="$(brew --prefix mpv)/lib" cargo run --release
```

Windows 需要 Visual Studio Build Tools，并下载对应架构的 `mpv-dev` 压缩包：

```bash
rustup target add x86_64-pc-windows-msvc
cargo build --release --target x86_64-pc-windows-msvc
```

将 `mpv-dev` 中的 `mpv.lib` 加入库搜索路径，并将 `libmpv-2.dll` 放到程序旁边。GitHub Actions 会自动下载并配置它。

## 登录状态

程序使用二维码登录，登录状态保存在 `~/.config/biliguga/session`（可通过 `XDG_CONFIG_HOME` 调整）中。Cookie 只用于访问 B 站接口，请不要把 session 文件提交到仓库。

Windows Release 同时提供 `biliguga-windows-x86_64-setup.exe` 安装程序，安装后可从开始菜单启动；Windows 安装包已包含播放器运行库。

Linux Release 同时提供：

- Debian/Ubuntu：`biliguga-linux-x86_64.deb`
- Fedora/RHEL：`biliguga-linux-x86_64.rpm`
- Arch Linux：`biliguga-linux-x86_64.pkg.tar.zst`
- AUR 二进制配方：`biliguga-linux-x86_64-aur.tar.gz`，包名为 `biliguga-bin`，只下载 Release 二进制，不会在用户电脑编译

Arch 用户也可以直接使用 `packaging/arch/PKGBUILD`：

```bash
makepkg -si
```

Release workflow 会在生成 Release 后计算校验和；配置仓库 Secret `AUR_SSH_PRIVATE_KEY` 后，还会自动把 `biliguga-bin` 推送到 AUR。

Linux 安装包依赖系统的 libmpv；Debian/Ubuntu 使用 `libmpv2` 或 `libmpv1`，Fedora 使用 `mpv-libs`，Arch 使用 `mpv`。Wayland 相关依赖是播放器/窗口后端的动态库，不要求用户运行 Wayland compositor；纯 X11 会话会自动选择 X11 后端。

## GitHub Actions

推送普通分支或 Pull Request 会构建并测试三个平台。推送 `v*` 格式的 tag 会构建发布包并自动创建 GitHub Release。

```bash
git tag v0.1.0
git push origin v0.1.0
```

发布包中的播放器动态库可能受各平台发行方式影响；如果系统无法找到 libmpv，请按上面的平台说明安装对应运行库。

## 界面框架

界面使用 [Rune](https://github.com/anomalyco/rune)。它目前尚未发布 crate，所以 `Cargo.toml` 固定了 `dev` 分支的一个 rev；升级时改那一行即可。Rust 工具链版本由 `rust-toolchain.toml` 固定为 Rune 要求的 1.99.1。

相对之前的 GPUI 版本，这次重写换来了几件事：

- **播放不再重跑界面。** mpv 的 worker 线程直接写入一个 `rune::Pixels`，渲染器只上传它并唤醒正在画它的窗口；视频帧不会触发视图重建。
- **输入框是框架自带的。** 原来那 553 行手写 `Element`（UTF-16 选区、自绘光标、`EntityInputHandler`）被 Rune 的 `text_input` 取代，中文输入法、撤销和剪贴板一并有了。
- **不再有借用重入崩溃。** 状态就是普通结构体，处理器拿 `&mut`，没有把实体借出去再放回的模型。
- **图片生命周期交给运行库。** `drop_image`、retired 帧队列、`pending_cover_drops` 那套手工簿记全部删除。

GPUI 版本保留在 `main` 分支上，标签 `gpui-final`。

已知问题：Rune 内置的 Parley 没开 `complex-scripts`，所以中文按字断行而不是按词，启动时 stderr 会打印几行 `ICU4X data error: No segmentation model for complex script: Chinese/Japanese`。这只影响断行的美观，不影响功能；上游修好后会自动消失。

## 许可证

本项目使用 MIT 许可证。项目依赖的 Rune、libmpv、FFmpeg 等组件分别遵循其自身许可证。
