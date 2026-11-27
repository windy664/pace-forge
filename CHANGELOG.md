# 变更记录

本文件遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)（每个版本按「新增 / 变更 / 安全 / 修复 / 文档」分组），版本号遵循语义化版本。

判断「要不要升级」看条目前的标签：

- **[行为变化]** 升级后行为会变，旧用法可能失效，升级前先看
- **[仅修复]** 修正错误，行为不变
- **[仅文档]** 只动说明、注释、元信息

详细说明见 `docs/release-notes/<tag>.md`。

## [Unreleased]

> 核心：按 7.4.20 真机（脱壳 + 真机报文）对齐提交体与设备头。**实测一条 2km/12min
> 提交成功、App 详情页 5 个绿点、无 201。** 如果升级后提交异常，回滚 v0.1.10。

### 文档

- 提交报文有了字节级 golden 测试（`tests/golden/submit_payload.txt`）：一组固定
  输入钉住 `objectKey`、签名原文、点位数组和 `save/record` 的每一个字节。
  每次运行都会变的 `geeToken` 与它派生的 `signature` 在比较前抹掉，其余照比。
  拆提交链路时可以确信「只是搬移」，红了就是搬错了。
- `missing_docs` 设为 deny：所有公开项（含字段、变体、方法、crate 本身）
  都有说明，以后新增的公开项没写文档直接编不过。
- 每个源文件只保留一个 `#[cfg(test)]` 和一个测试模块 `mod tests`（放在文件
  末尾）：16 个文件里散落的 39 个测试模块合并进去，4 个挂在单个函数上的
  `#[cfg(test)]` 辅助项也收进测试模块。测试数量不变（531）。

### 变更

- **[仅修复]** `dry_run` 的 ⑦ 自检段拆成四块：`inspect_probe`（报文反解视图交给
  preflight）、`audit_points`（必到点齐 / `isPass` / `type=2` 三道校验）、
  `log_point_distances`（两套坐标系最近距离与首次经过下标）、`warn_point_order`
  （轨迹经过次序 vs 点位数组同序）。本体从约 560 行降到约 280 行。
- **[仅修复]** `track::preflight::inspect_at` 按检查项拆成 `check_timeline` /
  `check_point_times` / `check_distance_and_geo` / `check_checkpoints` /
  `check_buckets` / `check_sampling` / `check_physics` / `check_totals` /
  `check_startup` 九个函数，本体只剩「依次调用、最后拼 Report」。检查项顺序与
  输出文案一字未改（有 golden 之外的既有断言兜底）。
- **[仅修复]** `dry_run` 的 ⑤ 采样段提成 `sample_stage`，产出收成 `SampleStage`
  （上报点、实测里程、仿真输入），本体再降到约 300 行。
- **[仅修复]** `dry_run` 的①策略、②围栏、③打卡点、④规划四段各自提成
  `stage_policy` / `stage_fence` / `stage_points` / `plan_stage`，本体从 1300
  行降到约 560 行，只剩「按什么顺序、失败怎么降级」。日志文案和降级规则一字未改。
- **[仅修复]** `handle_plan` 拆成三条：参数与中心点解析留在本体，联网预演的
  响应体搬到 `respond_online`，本地合成搬到 `respond_local`。纯搬移，响应
  JSON 一字未改。
- **[仅修复]** 网页控制台的请求体收进 `src/web/dto/`：每个路由域一个模块
  （run / accounts / meta / records / map_data），`body["x"].as_f64()` 这种散落
  取值变成 DTO 的字段 + 方法。字段全用宽松反序列化，类型不对仍然是「当没填」，
  默认值与拆前逐字相同（7 条等价测试钉住）。线上请求与响应不
  变，改变的是「这个接口收哪些字段」有了单一出处。
- **[仅修复]** 外部数据入口的数字强转集中到 `src/number.rs`：用户提交的
  JSON、厂商响应、时间戳这些地方不再用裸 `as`，改成 `*_to_*` 检查函数（越界、
  NaN 由调用方决定兜底）。像素、几何计算里的 `as` 保留——那里向零截断本来就
  是想要的语义。`envelope::now_ms()` 返回类型从 `u128` 改为 `i64`（毫秒时间戳
  不需要无符号），调用方少了一轮转换。
- **[行为变化]** 命令行按错误类别分退出码（#62）：登录态 4、限频 5、网络 6、
  厂商拒绝 7、围栏几何 8；其它失败仍是 1，用法错误仍是 2，`cheat` 的 0/2/3
  不变。以前除用法错误外全是 1。完整表见 README「退出码」。
- **[行为变化]** 类型化错误铺满全库（#62 第三步）：除命令行入口外，所有
  `Result<_, String>` 换成 `Result<_, Error>`，`src/` 下不再有字符串错误签名。
  网页接口把内部错误原样回给前端时，改用按类别定状态码的响应
  （登录态 401 / 限频 429 / 厂商与网络 502 / 围栏点位 400 / 本机与文件 500），
  以前这些一律是 HTTP 200 + `ok:false`。只靠状态码判断成功的前端代码要改成
  同时看 `ok` 字段。
- **[行为变化]** 控制台口令错误的 401 响应体新增 `"kind":"console_auth"`。
  前端只在「401 且 kind 是 console_auth」时弹解锁框；账号登录态失效
  （`kind:"auth"`）改为显示错误文案，不再误弹解锁框。自己写了脚本只看 401
  的，请改看 `kind`。
- **[行为变化]** 类型化错误第一步（#62）：新增 `src/error.rs` 的 `ErrorKind`
  （Auth / Protocol{厂商码,说明} / Network / RateLimit / Fence / Client /
  Storage / Json），`client.rs` 作为唯一分类点：厂商业务码在此归类
  （`10121`→Auth、`10603`→RateLimit、其余→Protocol），传输失败归 Network。
  本批转换 client + login / records / semester / cheat / ai 五个模块及
  `expect_ok`；flow/submit/fence/points/policy/obs 的边界暂以 `to_string()`
  过渡（下个 PR 转换）。**Web 控制台错误响应变化（对着控制台 API 写脚本的话
  看这里）**：这些端点的厂商错误从一律 400 变为 401（认证）/ 429（限流）/
  502（上游拒绝/网络），响应体新增 `kind` 字段（auth/protocol/network/
  rate_limit/fence/client/storage/json）和 `vendor_code`（有厂商码时）；
  厂商错误文案从「业务错误 error=…」变为结构化中文（如
  「不在有效时段(vendor code 11016)」）。CLI 打印文本随之微调，退出码不变。
- **[行为变化]** 配置系统重写（#53）：新增 `config.toml`（搜索 `./config.toml` →
  `~/.pace-forge/config.toml`，或 `--config <路径>` / `PACE_CONFIG` 指定），
  优先级 `PACE_* 环境变量 > config.toml > 默认值`。配置拆进独立 crate
  `crates/pace-forge-config`：21 个开关（含旧清单漏掉的
  `PACE_EXTRA_CA`/`PACE_IGNORE_DAILY_LIMIT`/`PACE_PREFER_STADIUM`/
  `PACE_CENTER_LAT`/`PACE_CENTER_LNG`/`PACE_FIXED_POINT`/
  `PACE_FORCE_NORMAL_POINT`）一张注册表同时服务 TOML 解析和 env 解析。
  env 名全部保留，老用法不破。**系统性语义变更：非法值从「静默回落默认」
  改为「启动报错」**——包括配置文件未知键（防拼写错误）、词汇外的枚举值、
  缺一半的 `PACE_CENTER_LAT/LNG`（旧版静默忽略）。布尔开关词汇统一为
  0/1、true/false、yes/no（大小写不敏感；旧版各键口径不一，文档里写过的
  值行为不变）。`PACE_DUMP_POLICY` 为空串不再被当成目录名。
  `config.example.toml` 进仓库，键参考以它为准。
- **[行为变化]** 移除 `pace-forge config` 子命令：那张 21 行的开关表与
  `config.example.toml` 逐字重复，没人愿意读。键参考归 docs（example.toml +
  README 配置节）；「哪些开关非默认、来自哪一层」的运行时信息保留在 Web
  启动日志的「非默认开关」一行。
- **[行为变化]** 源码整体搬进 `src/`（`api/` `crypto/` `track/` `web/` 及入口文件），`data/`、`tests/`、`docs/` 位置不变；对外行为不变（#38）
- **[行为变化]** 升到 edition 2024 + rand 0.9，并用 `rust-toolchain.toml` 把工具链钉在 1.99.1。同 seed 下 planner 与配速剖面的输出 hash 与升级前逐字节一致（#38）
- **[行为变化]** `semester` 在 `goalMethod=2` 的学校（服务端次数恒 0）多打一行本地反算：
  「N 次计分跑分布在 M 天」。依据是记录列表里每条都带 `validDis`/`sportType`，
  按学期起止筛一遍即可；不做日上限假设（日上限只影响里程，不影响次数）。
  **已用真实账号验证**：26 次 / 23 天、39.07km 有效（共 55.05km）。
- **[行为变化]** 日上限改为读服务端 `runRuleModel.dayGoal`（实测某校 3000m），不再写死 5000m。`runMode=1` 在有效时段外取不到 policy，自动用 `runMode=2` 补一次；都取不到才退回 5000。超过上限直接拦（`PACE_IGNORE_DAILY_LIMIT=1` 仍可硬跑）。**已用真实账号验证**：2km 放行、4km 被拦。
- **[行为变化]** 新增户外跑（= 自由跑，App 同一类）：Web 跑法下拉新增「户外跑」，CLI `run-free [km] [min]`。sportType=4（不计学期成绩），策略走 runMode=2（无时段限制），不拉围栏/打卡点、轨迹绕锚点自由生成，提交报文省略 `fivePointJson`（OBS 明细仍发空数组 envelope）。**已真机验证**：北京奥森 2km 提交 rrid=1375931242，服务端回 sportType=4、complete=true、validDis 全额、isCheat=0。映射依据 neko 实测：2/5 被拒「uid 不存在」、3 有特殊签名规则
- **[行为变化]** 新增 AI 运动模块（仅 CLI）：`ai-list` / `ai <id> <成绩> [--minutes] [--days N]` / `ai-records <id> [N]`。计次/计时两类数值仿真人（±10% 抖动），`--days` 补签过去时刻，无需视频。**尚未真机验证**
- **[行为变化]** `CustomDeviceId` 纳入 `PACE_DEVICE_HW_HEADERS` 开关（默认仍发派生值）；五个硬件头一起清空，`DeviceId` 不受影响
- **[行为变化]** 提交前拦 `isPass=false`：App 详情页会显示橙点、这次打卡等于没算，宁可本地失败

### 安全

- **[仅修复]** CI 新增 `cargo-deny`：RustSec 漏洞与 yanked 依赖直接红，许可是白名单制，多版本只警告。首跑修掉 `url` 的 RUSTSEC-2026-0592（`Url::origin()` 嵌套 `blob:` 栈溢出，升到 2.5.9）；`rsa` 的 RUSTSEC-2023-0071 带理由忽略（我们只做公钥加密/验签，无私钥运算）（#38）

### 修复

- **[仅修复]** `PACE_IGNORE_DAILY_LIMIT=0` 现在是「不绕过日上限」。旧实现用存在性判断（`is_ok()`），设成 `0`、`false`、空串之外的任何值都会**开启**绕过——想关都关不掉
- **[仅修复]** `key_sources_fall_back_past_the_posix_defaults` 在没有机器标识取源的机器上（macOS：无 `/etc/machine-id`、`/etc/hostname`，未设 `COMPUTERNAME`）跳过，不再让 `cargo test` 在开发机常红
- **[仅修复]** 打卡点回钉改为按数组次序（`pin_checkpoints_in_order`）：无序遍历会把「去 #2 途中擦过 #4」的 #4 钉在前面，首过次序变成 0,2,3,4,1，服务端按顺序判定直接失败。钉不进次序的会报出距路径多少米。**只跑过单测，夜间点位接口限流（10603）拿不到打卡点，还没真机复测。**
- **[仅修复]** 路网 A* 对「未轮到的打卡点」改为硬禁（12m 半径，走不通再退回软罚）：原来 25m 半径 + ×5 软罚在单校主路上等于没禁，实测 5 点位仍被判首过乱序。**同上，只跑过单测。**
- **[仅修复]** `suggest` 补一条出口建议（三个风控维度里唯一能当场查的）
- **[仅修复]** `fivePointJson` 的点位条目改由 `FixedPointItem` 结构体 serde 序列化，不再手拼 `json!`；键序与原报文逐字节一致，有测试锁着（#38）

### 文档

- **[仅文档]** 本文件改为 Keep a Changelog 格式：每个版本按「新增 / 变更 / 安全 / 修复 / 文档」分组，三种标签保留；v0.1.10 补上实际发布日期（2026-11-21）

## [0.1.11] - 2026-11-25

> 核心：按 7.4.20 脱壳 dex 对齐提交体与五点位。实测 6 个账号提交成功、
> App 详情页 5 绿、服务端 complete=true 且 validDis 全额。
> 如果升级后提交异常，回滚 v0.1.10。

### 变更

- 提交体四字段对齐真机 7.4.20：`segmentJson` 空串、`more=true`、`prizeName=null`、`appVersion` 默认 `7.4.20`（`PACE_APP_VERSION` 可覆盖）
- **`isFixed` 默认全部发 0**：脱壳确认 App 按 `isFixed∈{1,2}` 分「必经点/普通点」两套图标，必经点那套固定画橙。全 0 后详情页 5 绿；代价是 reasonList 里「经过必经点位」显示 ✗，成绩不扣（实测 semesterValidDis 全额）。`PACE_FORCE_NORMAL_POINT=0` 可发回真实值
- **`isPass` 恒发 true**：它是 App 本地 Room 字段，服务端不认；早先按严格命中发 false 会让详情页画橙
- 自检新增逐点与 10 秒桶的配速/步频检查（`PACE_OUT_BODY` / `BUCKET_PACE_OUT` / `BUCKET_CADENCE_OUT`），阈值按真机基线定（点级 15%、桶级 40%），只提示不拦

### 新增

- **提交前四道闸门**：违规名单自查（`v78/cheat/cheatlist`）、跑步冻结（`policy.freezeRunTime`）、日上限（今日累计 + 本次 > 5000m 直接拦，`PACE_IGNORE_DAILY_LIMIT=1` 放行）、有效时段（`getHistoryConfig` 的 `randomValidTimes`）
- **提交后回读服务端判定**：`get_one_record` 的 `isCheat` / `stepFreqStatus` / `status` / `validDis` / `reasonList` 逐条打印，不达标时给下一步动作
- 月度信用分核验提醒（`creditscore/v70390/popup`）
- 提交失败提示 3 天补传窗口
- **终点可选**：跑完收到指定终点，不再强制绕回起点（路网必经点 + 禁末尾折返）
- 手绘：橡皮擦（局部删点）、里程差提示、首尾缺口警告、改目标距离实时重算

### 修复

- `audit` 三处：闭合轨迹反向判定（原先恒判反向）、只看最高一对导致漏报、`suggest` 打出字面 `{}`
- `tracks --top N` 参数校验
- `PACE_FORGE_DATA_DIR` 真正生效
- 空白瓦片当合法结果（高德无道路区回 ~179B 纯色 PNG）
- 路网凑里程加重重复惩罚（重复路段 41% → 25%，折返 3 → 0）
- 13baa08 之前的 type=2 数量检查降级为警告（拦过一次，把正常提交也挡住了）

### 文档

- `docs/notes/REVERSE-BASELINE.md`：撤回三条旧结论、consts 出处更正、真机采样点距差 3 倍
- `docs/notes/SERVER-JUDGEMENT.md`：服务端判定面全字段、reasonList 结构、状态枚举、违规冻结链

### 已知问题

- 15km 这类超长目标生成不到目标里程（围栏周长限制，实测目标 15km → 实际 10.5km）
- 日上限 5000m 是按单校实测值写的，别校不同要改

## [0.1.10] - 2026-11-21

### 变更

- **[行为变化]** 新增 `pace-forge audit` / `suggest` / `tracks`：账号体检（连续性、时段集中度、里程/配速离散、轨迹重合矩阵），命中只提示、不拦提交；检查码前缀 `AUDIT_*`，与 preflight 的 `PACE_*` 分开（#14）
- **[行为变化]** 新增 `PACE_EGRESS_PROXY`：提交链路可走 `http://` CONNECT 代理，用于「人在外地 + 有一条回学校同城的线路」；出口本机直连 / 代理 / 隧道都行，不设则逐字节等同直连，值写错报一次并按直连（#10）
- **[行为变化]** Overpass 改成三端点**并发竞速**（窗口 6 秒），原来串行最坏 30 秒；单端点超时 10s→6s。校园网络全线不通时从「卡半分钟」变成「6 秒进冷却」（#2）
- **[行为变化]** 网页「设备机型」一节：按账号选机型并落盘，选中后填平台代号即可让 `cpuModel` 发 SoC 型号；映射写 `<数据目录>/device_models.json`，只影响本机；改机型不动 `device_id`（`GET/POST /api/device`）
- **[行为变化]** 新增 `PACE_DEVICE_MODEL` 与 `data/devices.json`：命中时 `cpuModel` 发 SoC 型号（`kalama`→`SM8550`，17 条平台映射），未收录仍发 ABI `arm64-v8a` 并提示；机型层只收有出处的条目，当前为空。`pace-forge devices` 打印表与补表方法（#18）
- **[行为变化]** 新增 `PACE_DEVICE_HW_HEADERS=empty`：四个可选硬件头改发空串（默认仍发派生值，`DeviceId` 不动）
- **[行为变化]** `export-records` 条数非法或超出 1..100 直接退出 2；目标文件已存在时拒绝写入，需 `--force`
- **[行为变化]** `run`/`ago`/`rand`/`stadium`/`--days` 的数字参数写错时直接报错，不再静默回落默认值
- **[行为变化]** 新增 `PACE_FIXED_STRICT=1`：必到点只认 10 米内，够不着报缺失，不自动放宽半径
- **[行为变化]** 新增 `pace-forge export-records [N] [out.json]`：导出最近 N 条记录及轨迹，供 crosscheck 使用
- **[行为变化]** 新增 `pace-forge crosscheck <json>`：跨记录自检连续天数、起跑时刻集中度、里程/配速离散度、轨迹两两重合度

### 修复

- **[仅修复]** `PACE_FORGE_DATA_DIR` 真正生效：设了就整个数据目录跟着走（账号库、会话、设备标识、缓存），不再出现「config 表里说生效、实际仍读写 `~/.pace-forge`」（#21）
- **[仅修复]** 空白瓦片当合法结果：高德在无道路区域回 ~179B 纯色 PNG，原来被「>512 字节」判失败并进 10 分钟冷却，西部/新校区整片规划失败（#2）
- **[仅修复]** 删掉 `DeviceProfile` 里从未上行的 `mac_address`；README 写清设备身份口径（cpuModel 发 ABI、MAC 为虚拟网卡、不做机型表）
- **[仅修复]** 新增 fivePointJson 内外层键序断言：形状变了才红，改数值不红
- **[仅修复]** `config` 表补上 `PACE_FORGE_DATA_DIR`；值解析不了时标「无效值（实际用 X）」，不再显示成生效
- **[仅修复]** `check_secrets.py` 不在 git 仓库时退回全量扫描，不再抛 Python 堆栈；全量扫描跳过 `node_modules`/`vendor`/`dist`/.venv 等目录
- **[仅修复]** `crosscheck` 首行打印时区来源；重合度标注「仅空间形状，不含时间对齐」；配速先整体进位再拆分，不再出现 `7'60"`
- **[仅修复]** 必到点匹配改为逐点返回结果（正常命中/放宽命中/缺失），某个点没命中不再让后续告警错位
- **[仅修复]** 生成流程加一行「目标里程 → 实测里程」诊断，粗采样或吸附后偏差直接可见
- **[仅修复]** 自检对超长记录补一条不阻断提示：起步窗口按 15.5% 外推，超过 86 秒的部分没有真机样本

### 文档

- **[仅文档]** `config` 表尾补一句：`PACE_OUT`/`PACE_FAST` 是 preflight 检查码，不是开关
- **[仅文档]** 新增 OBS 对象格式、`avgStep` 口径复核、必到点匹配三篇依据文档
- **[仅文档]** 仓库内坐标整体平移 20°、地名与校区点位名替换，公开仓库不再指向具体学校
- **[仅文档]** 新增凭证/个人信息扫描（CI + pre-commit），命中只报位置与类型，不打印原文
- **[仅文档]** 起步窗口诊断表补里程预算与首段配速（555/1200/1800 三档），并记下「开跑前已锁定 GPS」未建模

## [0.1.9] - 2026-11-20

### 变更

- **[行为变化]** 新增 `pace-forge config` 子命令：一张表列出全部 `PACE_*` 开关、默认值和影响；口令只显示「已设置」
- **[行为变化]** `web` 启动时先打一行非默认开关，方便确认环境变量有没有生效

### 修复

- **[仅修复]** 退出登录不再假装成功：服务端没确认时明确提示「半小时内别重登」
- **[仅修复]** 「非默认」判定不再拿描述性默认值比较（原先自己设成默认值也会被标成非默认）

### 文档

- **[仅文档]** 仓库根新增本文件，按标签汇总 v0.1.1～v0.1.9

## [0.1.8] - 2026-11-20

### 变更

- **[行为变化]** 「自动规划」不再被操场反推覆盖：只有明确选「操场绕圈」才按打卡点反推跑道，其余走路线规划
- **[行为变化]** 计分跑围栏接口失败时直接报错停下，不再静默按自由跑提交（空列表仍按原行为继续）
- **[行为变化]** 新增「纯几何（跳过路网）」开关：不拉 OSM、不拉瓦片、不查建筑水体

### 修复

- **[仅修复]** 路网/瓦片拉取超时收紧（Overpass 10 秒、瓦片 6 秒），全线失败后冷却 10 分钟
- **[仅修复]** 生成流程按阶段报耗时；控制台新增「网络自检」
- **[仅修复]** IMEI 改为 15 位（含 Luhn 校验位），原实现产出 17 位
- **[仅修复]** 账号密文改随机盐 + IV，账号库落密钥指纹
- **[仅修复]** 控制台口令改常量时间比较；请求并发加上限（超出回 503）

### 文档

- **[仅文档]** 免责声明补禁止用途与「官方联系即停更」；加贡献指南与 issue 模板；加 `rust-version = "1.88"`

## [0.1.7] - 2026-11-19

### 变更

- **[行为变化]** 配速图纵轴回退真机口径：起步窗口的配速恢复为 0（不再造 90~110 分/公里的假速度）

### 修复

- **[仅修复]** 操场绕圈的 GPS 抖动按采样间距缩放，单圈里程膨胀从 7.3% 回落到 1% 上下
- **[仅修复]** 场心剔除误判（4 个角点位时会错剔一个角点）、打卡点吸附详情进日志、生成后复核贴合度
- **[仅修复]** 顶栏显示出口 IP，并标出是否走了代理
- **[仅修复]** 必到点标记被兜底挪出命中圈时告警，不再静默

## [0.1.6] - 2026-11-18

### 变更

- **[行为变化]** 计分跑提交报文恢复 `type=2` 打卡标记（撤销 v0.1.5 的改动），修复「计分跑被判自由跑」

### 修复

- **[仅修复]** 慢跑（约 12 分/公里）步频、步幅曲线与实测样本对齐
- **[仅修复]** 手绘轨迹配速顶到 2'39" 的问题

## [0.1.5] - 2026-11-16

### 变更

- **[行为变化]** 提交报文键序改为与真机一致（字母序、flag 开头），修复 App 详情页只画 4 个必到点

### 修复

- **[仅修复]** 拐角补位后间距被顶回去
- **[仅修复]** 提交前核对当前会话账号
- **[仅修复]** 采样点 `type` 按最新真机口径

## [0.1.4] - 2026-11-16

### 修复

- **[仅修复]** 禁区坐标少转一道（GCJ-02 → BD-09 偏 950m），禁区此前完全无效
- **[仅修复]** 生成路线把已查到的点位刷没
- **[仅修复]** 跑道轨迹穿进足球场
- **[仅修复]** 多校区场景只有一个场地可选

## [0.1.3] - 2026-11-16

### 修复

- **[仅修复]** 禁区盖住打卡点时绕行等于没绕
- **[仅修复]** 生成路线时点位被替换

## [0.1.2] - 2026-11-16

### 修复

- **[仅修复]** 控制台 XSS：服务端返回的文本全部先转义再插入页面
- **[仅修复]** 按钮卡在「进行中」态、请求无超时
- **[仅修复]** 一张坏瓦片毒化整格网缓存

## [0.1.1] - 2026-11-16

### 修复

- **[仅修复]** 短距离轨迹（点距 < 2.79m）在起步段压缩时 panic
- **[仅修复]** 「现在跑」生成的记录整条落在服务器未来轴
- **[仅修复]** 路网规划漏打卡点却报成功
- **[仅修复]** 口令模式下控制台打不通

[Unreleased]: https://github.com/IRmsk/pace-forge/compare/v0.1.11...HEAD
[0.1.11]: https://github.com/IRmsk/pace-forge/compare/v0.1.10...v0.1.11
[0.1.10]: https://github.com/IRmsk/pace-forge/compare/v0.1.9...v0.1.10
[0.1.9]: https://github.com/IRmsk/pace-forge/compare/v0.1.8...v0.1.9
[0.1.8]: https://github.com/IRmsk/pace-forge/compare/v0.1.7...v0.1.8
[0.1.7]: https://github.com/IRmsk/pace-forge/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/IRmsk/pace-forge/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/IRmsk/pace-forge/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/IRmsk/pace-forge/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/IRmsk/pace-forge/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/IRmsk/pace-forge/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/IRmsk/pace-forge/releases/tag/v0.1.1
