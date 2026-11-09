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

用户问「为什么这么多线程，能不能压到 10 个以内」。实测下来的答案：**10 个以内不能**，但 libmpv 那部分能从 36 压到 32，其中解码线程从 4 个压到 0。

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

## 削掉了什么（实测对比）

用真实 1080p H.264 文件、真实 `MpvPlayer::load()`，等帧真正流起来之后数线程：

| 配置 | 总线程 | `av:` 解码线程 |
|---|---|---|
| mpv 全部默认 | 28 | **16**（一核一个） |
| `vd-lavc-threads=2` 单独 | 36 | **4** |
| `vd-lavc-threads=2` + `hwdec-threads=1` | **32** | **0** |

第一个反直觉的发现：**`hwdec=auto-safe` 会让 `vd-lavc-threads` 失效**。硬件解码走自己的线程池，默认 4 个，完全不看软解线程数。所以只设 `vd-lavc-threads` 什么也省不下，必须同时设 `hwdec-threads`。

`hwdec-threads=1` 之后解码在调用线程上完成，`av:` 归零——播放仍然正常（有测试守着，`BILIGUGA_TEST_CLIP` 指到本地文件就能跑）。

## 关键发现：`load-scripts=no` 不起作用

mpv 的 7 个内置 Lua 脚本（console、stats、context_menu、positioning、select、commands、ytdl_hook）各占一个线程。但**这个构建的 libmpv 关不掉它们**：

```
load-scripts=no     → 10 threads, 7 lua
scripts=""          → 10 threads, 7 lua
```

单独写程序验证，`mpv_set_option_string` 返回 0（成功），但 `mpv_initialize` 之后 7 个 lua 线程照样起来。`ytdl_hook` 那个能关（`ytdl=no` 有效），其余 6 个不行——取决于发行版怎么编译 libmpv。

## 小结

- **应用自己的线程：4 个**（主线程 + 2 网络 + 1 mpv 调度）。这部分已经不能再省。
- **下限约 20 个**：libmpv 初始化即 7 个 lua 线程（编进 libmpv 二进制，`load-scripts`、`scripts`、`no-config` 全部无效，用 `mpv_get_property_string(h, "script-list")` 和独立的 `mpv_create` 程序都验证过）+ Rune 的 4 个图片线程（`DECODERS` 硬编码）+ 自己的 4 个 + 主线程。
- **10 个以内的唯一办法是不用 libmpv**：改用系统解码器（GStreamer/VA-API 直接接），那是另一个量级的工程，而且会失去 mpv 的格式兼容性和 seek 精度。

## 复现方法

```bash
# 造一个 1080p 素材
ffmpeg -f lavfi -i testsrc2=size=1920x1080:rate=30:duration=8 \
       -pix_fmt yuv420p -c:v libx264 -preset ultrafast /tmp/t1080.mp4

# 真实播放器 + 线程统计，断言 av <= 2
BILIGUGA_TEST_CLIP=/tmp/t1080.mp4 \
  cargo test --release player_threads -- --ignored --nocapture
```

为了这点，我把 `ytdl=no` 和 `vd-lavc-threads=2` 加上了（各少 1 和 2 个线程），`load-scripts=no` 也保留了——在关得掉的 libmpv 构建上它是有效的，只是这台机器上无效。`set_option` 现在会记录被拒绝的选项，不再静默失败。

# 播放时的 CPU

用户提到「播放状态 0.4%，竞品 0.3%」。用真实窗口实测，**我们是 0.165–0.183%**，比两个数字都低。下面是测量方法和拆解。

## 怎么测的

```bash
# 造一个 3 分钟 1080p 素材（避免循环与结尾处理干扰）
ffmpeg -f lavfi -i "testsrc2=size=1920x1080:rate=30:duration=180" \
       -f lavfi -i "sine=frequency=440:duration=180" \
       -pix_fmt yuv420p -c:v libx264 -preset ultrafast -c:a aac -shortest /tmp/bench180.mp4

# 用本地片段起真实窗口（BILIGUGA_BENCH_CLIP 是测量入口，不是功能）
WAYLAND_DISPLAY=wayland-1 BILIGUGA_BENCH_CLIP=/tmp/bench180.mp4 ./target/release/biliguga &

# 读 /proc/PID/stat 的 utime+stime，10 秒窗口
```

`/proc/PID/stat` 的 tick 是 100Hz，所以 `tick / 10 = 百分比`（单核）。

## 结果

静置（无视频）：**0.000%**，Rune 的静止窗口不画帧。

播放中，多次独立运行：**0.165%、0.174%、0.172%、0.181%、0.183%**。

10 秒窗口 181 tick 的归属：

| 线程 | tick | 占整进程 |
|---|---|---|
| `biliguga-libmpv`（mpv 渲染循环 + 软渲染） | 37 | 20% |
| **`biliguga`（我们的 UI 主线程）** | **23** | **13%** |
| `worker` ×8（ffmpeg 解码） | ~110 | 61% |
| `core`/`demux`/`vo` | ~11 | 6% |

**我们自己的 UI 只占 0.023%。八成以上是 ffmpeg 在解码**——这部分任何用 libmpv 的客户端都一样。

## 试过但没收益的

1. **把时钟更新改成不等重建。** `poll` 里每 0.2 秒推进播放头时会 `changed = true`，触发一次提交。我怀疑这是主要开销，实测禁用后 **0.183% vs 0.181%**——没有差异。Rune 的 diff 只重建变化的子树，那点工作可以忽略。
2. **`vd-lavc-threads` 2 → 1**：0.172% vs 0.181%，在噪声内。线程少了，单线程解码时间变长，总量守恒。
3. **live 绑定驱动进度条**：写了一半发现没有收益，且实现绕（需要视图把轨道宽度写回状态），已完整回退，没有留下半成品。

## 结论

**没有输。** 0.17% 比 0.3% 低，而且差距不在我们能控制的地方——UI 只占 0.023%，剩下的全是 ffmpeg。

需要考虑的是测量口径可能不同：竞品的 0.3% 如果是「在线播放 + 弹幕渲染」的数字，而我们是本地文件无弹幕，那就不可比。真要对比，得在同等条件下测。

真要继续压 CPU，唯一有效的方向是**降低解码负载**，比如选更低的画质，或者用硬解且避免 `hwdec` 的 copy 路径（当前是 `vulkan-copy`，有一次 GPU→CPU 拷贝）。那是画质/兼容性的取舍，不是代码问题。

# 更正：btop 的口径，以及和 piliplus 的实际对比

前面几节里我把 CPU 口径搞错了一次，这里更正，并给出和竞品的同条件对比。

## 口径错了

我自己算的「0.13%」和用户从 btop 看到的「0.4%」是同一件事，只是分母不同：

```
进程自身： 260 ticks / 20 秒 = 13.0% 占单核
btop 口径：13.0% / 32 核 = 0.41%   ← btop 显示这个
```

btop 的公式（源码 `btop_collect.cpp:3465`，`proc_per_core` 默认 false）：

```c
cpu_p = 100 * (进程 CPU 时间增量) / (整机 jiffies 增量)
```

分母是**整机所有核**的时间增量。所以我少除了一次核数，把 13% 读成了 0.13%，并据此错误地否定了用户的观察。**用户的读数是准的。**

## 和 piliplus（Flutter 客户端）同条件对比

两个客户端同时运行、同时测量（20 秒窗口）：

| | CPU（占单核） | RSS |
|---|---|---|
| **biliguga** | **20.65%** | **278 MiB** |
| piliplus | 27.30% | 512 MiB |

空闲（都不播放）：biliguga **194 MiB**，piliplus **495 MiB**。

内存这个指标不受 CPU 争抢影响，更能说明问题：**少用 61%**。

CPU 的数字在同机抢核时波动很大（同一进程连续测 5 次能差 18.85%–23.75%），所以 CPU 的对比只在同一窗口内有效，不能跨次比较。

## 这轮修掉的两处真浪费

即使结论被推翻过，这两处是实打实的：

1. **每帧的 alpha 填充**。mpv 的 `rgb0` 第 4 字节是 "uninitialized garbage"（mpv 头文件原话），而 Rune 把它当 alpha 读，必须填 255。
   `step_by(4)` 逐字节：1080p **316 µs/帧**；改成 `u32` 或运算：**121 µs**。
2. **每帧的缓冲清零**。`vec![0u8; len]` 是 **117 µs**，其中清零占大部分。
   写程序验证过：用 0xAB 填满缓冲后让 mpv 渲染，**0 个字节未被触碰**，所以清零是白费的。改用 `set_len` 跳过：**15 ns**。

两处合计每帧省约 300 µs，30fps 下约 0.9% 单核。在 13% 的基数上是 7% 的相对改进。

## 测量方法（下次用这个）

```bash
# 占单核百分比，最直观
python3 - <<'EOF'
import time
pid = int(open('/tmp/pid').read())
def cpu():
    with open(f'/proc/{pid}/stat') as f:
        x = f.read().rsplit(')',1)[1].split()
    return int(x[11]) + int(x[12])
a = cpu(); time.sleep(20); b = cpu()
print(f'{(b-a)/20/100*100:.2f}% 单核   (btop 会显示 {(b-a)/20/100*100/32:.3f}%)')
EOF
```

注意两点：**分母是整机核数时得到的是 btop 的数字**；**连续测量要取多次中位数**，同机有别的负载时波动能到 25%。
