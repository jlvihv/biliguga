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
