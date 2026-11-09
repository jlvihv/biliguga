# 压力测试与性能分析

这份记录回答三个问题：能不能压测、瓶颈在哪、内存有没有问题。所有数字都是在本机实测的，不是估计。

## 怎么跑

```bash
# 六个负载测试，约 0.3 秒
cargo test --release stress -- --ignored --nocapture

# 单个跑；BILIGUGA_STRESS_FRAMES 提高帧数，供 perf 采样
BILIGUGA_STRESS_FRAMES=200000 cargo test --release scroll_cost -- --ignored --nocapture
```

`#[ignore]` 是刻意的：它们是**报告**，不是断言对错。里面的断言是宽松的上下界，用来抓回归。

测量代码在 `src/metrics.rs`，`#[cfg(test)]`，读 `/proc/self/status` 和 `/proc/self/stat`。

## 本机实测数字

| 负载 | 结果 |
|---|---|
| 首页滚动，120 行，240 次提交 | **0.12–0.14 ms/提交** |
| 视频帧，1920×1080，300 帧 | **0.09–0.10 ms/帧**，CPU 总计 0.03 s |
| 200 条评论，空闲提交 | 0.33 ms/提交 |
| 200 条评论，每次追加一条 | 0.35–0.49 ms/提交 |
| 翻页 800 行，保留 120 行 | 稳定后 RSS 不再增长 |
| 12 轮封面下载 + 行淘汰 | 缓存固定 20 个，RSS 稳定 |

## 瓶颈在哪

`perf record -e task-clock -g`（`perf_event_paranoid=2`，硬件采样被禁，软件计时可用）采了 74,440 个样本：

```
24.2%  Runtime::build                     ← 视图重建
19.3%  rune::growth::GrowTrack::step      ← growth 栈推进
 7.9%  Spring::with_duration              ← 同上，每行每帧重建弹簧
 5.9%  TextSystem::glyphs_in_band_cached  ← 字形
 5.4%  Runtime::paint_content             ← 绘制
 4.6%  render::pack_fade                  ← 淡入淡出打包
 3.1%  roundf / 2.1% floorf               ← 像素对齐
```

**约 27% 花在 Rune 的 growth 栈上**，那是 `virtual_rows` 的必需前置条件。`GrowTrack::step` 遍历所有行，每行无条件执行 `spring.with_duration(..)`，即使行高早已静止。

我试过把 `GrowthSpec` 的 `edge` 从默认 `FEATHER` 换成 `NO_MASK`（关掉 28pt 羽化遮罩和逐词淡入，视频列表用不到），**实测无差异**（19.3% vs 18.9%），所以回退了——不留没有依据的“优化”。这部分开销来自遍历行数，与羽化参数无关，只能等上游修。

`Runtime::build` 的 24% 是视图重建。`virtual_rows` 只构建可见行，所以这个数在滚动时是合理的；评论的 `memo` 把它压在 0.33 ms。

## 内存：没有发现问题

三个测试都看**稳定后**的增长，而不是初始值——前者才是泄漏，后者是正常的缓存填充。

- **翻页**：追加 800 行、保留 120 行，RSS 从 10 页后稳定在 83.4 MiB，**再翻 30 页纹丝不动**（三次独立运行都是 83.4 → 83.4）。
- **封面**：12 轮下载，缓存始终 20 个，稳定后 RSS 不再增长。
- **视频帧**：300 帧后 RSS 80.8 → 80.9 MiB。

这两处正是之前修过的泄漏（`trim_home_items` 不释放封面、`cover_images` 无上限），现在有测试守着。

## 踩到的坑

**jemalloc 的统计在这台机器上不可用。** 我最初的 `metrics` 读 `mallctl("stats.allocated")`，返回 0、errno 22（EINVAL）——系统 jemalloc 编译时没开 `--enable-stats`。所有计数器都是假的，但代码看起来正常。改用 RSS 后，测试证明了它确实跟踪分配：

```
start rss = 2 MiB
after 1x32MiB rss = 34 MiB
...
after free rss = 2 MiB
```

**压测零点写错过两次**，都改掉了：

1. `paging` 拿结束时和**起始**比，量到的是“前几页填满保留窗口”（正常工作量），误报泄漏。改成和第 10 页比。
2. `video` 每帧 `vec![0u8; 8MiB]`，把分配器成本算进了 0.84 ms。改成复用缓冲区后是 0.09 ms——**差 9 倍**。

这两次都是测试自身的问题，不是应用的问题。写下来是因为：一个测错的基准比没有基准更糟。

# 线程账

用户问「为什么这么多线程，能不能压到 10 个以内」。实测下来的答案：**不能**，因为 48 个线程里有 40 个来自两个改不动的第三方库。

## 实测构成（48 线程，播放中）

| 来源 | 数量 | 归属 | 能否削减 |
|---|---|---|---|
| `lua/*` ×6 + `core` | 7 | libmpv | **不能**，见下 |
| `av:h264:df0..3` | 4 | ffmpeg 解码 | 能（`vd-lavc-threads`，已设 2） |
| `worker` ×10 | 10 | ffmpeg/mpv 内部池 | 部分随解码线程数 |
| `demux` ×2、`vo`、`mpv/ao/pipewire`、`module-rt`、`data-loop.0`、`blocking-1`、`async-io` | ~8 | libmpv | 不能 |
| `biliguga-libmpv` ×2 | 2 | mpv 会话 + worker 调度 | 不能（本身是播放器线程） |
| `rune-images` ×4 | 4 | Rune | **不能**，硬编码 `DECODERS: usize = 4` |
| `rune-appearance`、`pool-spawner` | 2 | Rune | 不能 |
| `biligug:disk$0` ×4 | 4 | glibc aio（wgpu 加载驱动触发） | 不能 |
| `gmain`、`gdbus`、`dconf worker` | 3 | GTK/GLib（系统主题、托盘） | 不能 |
| `biliguga-net` ×3 | 3 | 自己的 tokio 运行时 | 已是最低（2 worker + 1 blocking） |
| `biliguga` ×2 | 2 | 主线程 + Rune |

## 关键发现：`load-scripts=no` 不起作用

mpv 的 7 个内置 Lua 脚本（console、stats、context_menu、positioning、select、commands、ytdl_hook）各占一个线程。但**这个构建的 libmpv 关不掉它们**：

```
load-scripts=no     → 10 threads, 7 lua
scripts=""          → 10 threads, 7 lua
```

单独写程序验证，`mpv_set_option_string` 返回 0（成功），但 `mpv_initialize` 之后 7 个 lua 线程照样起来。`ytdl_hook` 那个能关（`ytdl=no` 有效），其余 6 个不行——取决于发行版怎么编译 libmpv。

## 小结

- **应用自己的线程：4 个**（主线程 + 2 网络 + 1 mpv 调度）。这部分已经不能再省。
- **下限约 20 个**：libmpv 初始化即 8 个 + Rune 的 4 个图片线程 + 自己的 4 个 + 主线程。
- **10 个以内的唯一办法是不用 libmpv**：改用系统解码器（GStreamer/VA-API 直接接），那是另一个量级的工程，而且会失去 mpv 的格式兼容性和 seek 精度。

为了这点，我把 `ytdl=no` 和 `vd-lavc-threads=2` 加上了（各少 1 和 2 个线程），`load-scripts=no` 也保留了——在关得掉的 libmpv 构建上它是有效的，只是这台机器上无效。`set_option` 现在会记录被拒绝的选项，不再静默失败。
