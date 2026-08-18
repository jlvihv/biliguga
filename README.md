# biliguga

一个使用 Rust + GPUI + libmpv 实现的 B 站桌面客户端。

当前版本包含：

- 首页推荐视频和分页加载
- 视频搜索
- 登录、历史、收藏、稍后再看和动态
- libmpv 视频播放
- 播放进度上报和云端续播
- 评论、点赞、投币和收藏
- 窗口内全屏和屏幕全屏

## 平台支持

项目代码支持 Linux、Windows 和 macOS。播放器依赖系统的 libmpv，发布包中的程序需要对应平台的运行库。

| 平台 | 构建依赖 | 运行时依赖 |
| --- | --- | --- |
| Linux | Rust、`libmpv-dev`、GPUI 的图形依赖 | `libmpv.so` |
| Windows | Rust GNU toolchain、MSYS2 `mingw-w64-x86_64-mpv` | `libmpv-2.dll` 及其依赖 |
| macOS | Rust、Homebrew `mpv` | Homebrew mpv 的动态库 |

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

Windows 建议使用 MSYS2 的 MINGW64 环境：

```bash
pacman -S mingw-w64-x86_64-toolchain mingw-w64-x86_64-mpv
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
```

## 登录状态

程序使用二维码登录，登录状态保存在当前用户的数据目录中。Cookie 只用于访问 B 站接口，请不要把 session 文件提交到仓库。

## GitHub Actions

推送普通分支或 Pull Request 会构建并测试三个平台。推送 `v*` 格式的 tag 会构建发布包并自动创建 GitHub Release。

```bash
git tag v0.1.0
git push origin v0.1.0
```

发布包中的播放器动态库可能受各平台发行方式影响；如果系统无法找到 libmpv，请按上面的平台说明安装对应运行库。

## 许可证

本项目使用 MIT 许可证。项目依赖的 GPUI、libmpv、FFmpeg 等组件分别遵循其自身许可证。
