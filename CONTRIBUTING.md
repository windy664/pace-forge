# 参与贡献

三条通道，按你顺手的方式选：

- **报 bug / 实测反馈** → 开 issue，用 Bug 模板，`[track]` 日志必须全贴
- **协议核对 / 逆向结论** → 开 issue，用「协议核对」模板，注明证据等级
- **代码** → PR，一个提交一个主题，单独可过测试

## 证据分级（这个项目最重要的规矩）

**真机样本 > 线上探测 > APK 静态分析 > 推断。**

任何「字段是 X」「服务端按 Y 校验」的说法都要标依据等级。说「真机样本依据」就给出 rrid 或可复核的描述（比如 obs_real 那条的口径）。只有推断的，写成推断——猜对猜错都有价值，但标错了等级会误导下一个来的人。

## 不能碰的东西

- **真机校准过的数值**：起步窗口分布、配速剖面、步频回归锚点、海拔噪声、抖动参数在 14.9m 间距下的标定、`obs_real` 逐字节断言涉及的所有口径。要动它们，先拿到一条新的真机样本，并说明为什么新样本更可信。
- **报文字段语义不许猜着改**：没有证据的语义改动，先发「协议核对」issue 讨论，别直接改代码。

## 提交纪律

- `cargo test --lib` 全绿、`cargo clippy --all-targets` 0 警告。
- 两套检查码别混：`PACE_*` 是配置开关（env 覆盖 `config.toml`，键/词汇表/默认值
  都在 `crates/pace-forge-config` 的注册表一张表里，新增开关三处同步：
  注册表 + `Config` 字段 + `key_values` 显示，少一处测试直接红），`AUDIT_*` 是
  `audit` 的阈值命中（`src/api/audit.rs` 的常量表），`preflight` 的检查码是第三套。
- 提交信息只描述问题本身：哪里会坏、什么条件下坏、怎么改的。不用任何编号体系（不要写 B-01 这类），一个提交讲一个主题，能独立通过测试。
- 行为修复带回归测试；只改注释也欢迎（把失效的依据更正掉同样是贡献）。

## 环境

- Rust stable；`cargo test --lib` 不需要网络。
- 涉及 `src/web/index.html` 的改动：内联 JS 过一遍 `node --check`；凡是上游可控的文本（点位名、围栏名、报错原文）一律走 `esc()`，数值字段原样。

## 不会 Rust 也能帮上

- **逆向**：7.4.01+ 的 APK、爱加密壳、`libnetseckit` 签名层的任何进展，直接开 issue 贴结论，比 PR 更受欢迎。
- **协议核对**：对着真机样本核字段语义——`src/api/submit.rs` 注释里有逐点对拍表和允许误差，拿你的设备跑一条来对照就能发现新差异。
- **实测反馈**：带 `[track]` 日志和截图的反馈，比「不好使」值钱一百倍。

## 提交前的本地检查

Linux 和 macOS 在本仓库运行 `git config --local core.hooksPath .githooks`。
Windows 确认 `python` 指向 Python 3.10 或以上版本，然后运行
`git config --local core.hooksPath .githooks/windows`。
如已配置其他钩子目录，请先保留其中的钩子。

之后每次 `git commit` 会先跑 `tools/check_secrets.py --staged`，扫到 GitHub token、云厂商 key、
私钥块、11 位手机号就拦下。CI 有同一道（`secrets` job），本地这层只是让人早一步知道。

命中的原文不会打出来，只报文件名、行号和类型——日志本身经常被贴进 issue，
把 key 打进日志等于换个地方泄露。测试用的假号码写在 `EXAMPLE_PHONE_NUMBERS`。
