//! 入口：`pace-forge <命令>`
//!
//! 命令：
//! - `web [addr]`      启动 Web 控制台
//! - `dry <距离> <分钟>` 只做流程预演，不发写请求
//! - `stadium <圈数>`   生成操场轨迹并打印参数
//! - `version` / `help`

/// 解析必填或可缺省的数字参数。
///
/// 之前一律 `.and_then(parse().ok()).unwrap_or(默认值)`，于是
/// `run 2 abc` 会被当成 12 分钟**真提交**，`check -1 12` 带着负距离跑完全程，
/// `check 1e9 1e9` 直接 OOM abort。打错字和没写是两回事：
/// 参数存在但解析不出来 → 报错退出；参数不存在 → 用默认值。
fn parse_num(args: &[String], idx: usize, default: f64, name: &str) -> f64 {
    match args.get(idx) {
        None => default,
        Some(raw) => match raw.parse::<f64>() {
            Ok(v) if v.is_finite() => v,
            _ => {
                eprintln!("参数错误：{name} 「{raw}」不是有效数字");
                std::process::exit(2);
            }
        },
    }
}

/// 同 `parse_num`，但要求整数。
fn parse_int(args: &[String], idx: usize, default: i64, name: &str) -> i64 {
    match args.get(idx) {
        None => default,
        Some(raw) => match raw.parse::<i64>() {
            Ok(v) => v,
            Err(_) => {
                eprintln!("参数错误：{name} 「{raw}」不是整数");
                std::process::exit(2);
            }
        },
    }
}

/// 读 `--flag N` 这类旗标后的整数；没写旗标用默认值，写了但解析不了直接报错。
fn flag_int(args: &[String], flag: &str, default: i64) -> i64 {
    let Some(i) = args.iter().position(|a| a == flag) else {
        return default;
    };
    let Some(raw) = args.get(i + 1) else {
        eprintln!("参数错误：{flag} 后面要跟一个整数");
        std::process::exit(2);
    };
    match raw.parse::<i64>() {
        Ok(v) => v,
        Err(_) => {
            eprintln!("参数错误：{flag} 的值「{raw}」不是整数");
            std::process::exit(2);
        }
    }
}

/// 校验公里/分钟，和库层 `RunParams::validate` 保持同一套上限。
fn checked_km_min(dist_km: f64, dur_min: f64) -> (f64, f64) {
    if !(0.0..=42.0).contains(&dist_km) {
        eprintln!("参数错误：距离要在 0~42 公里之间，收到 {dist_km}");
        std::process::exit(2);
    }
    if !(0.0..=120.0).contains(&dur_min) {
        eprintln!("参数错误：时长要在 0~120 分钟之间，收到 {dur_min}");
        std::process::exit(2);
    }
    (dist_km, dur_min)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("web");

    match cmd {
        "web" => {
            let addr = args
                .get(1)
                .cloned()
                .unwrap_or_else(|| "127.0.0.1:9999".into());
            if let Some(line) = pace_forge::config::non_default_line() {
                println!("非默认开关: {line}");
            }
            if let Err(e) = pace_forge::web::serve(&addr) {
                eprintln!("启动失败: {e}");
                std::process::exit(1);
            }
        }
        "semester" => show_semester(),
        "login" => {
            // login <手机号> <密码>：无头登录（极验本地求解），会话落盘。
            let user = args.get(1).cloned().unwrap_or_default();
            let pass = args.get(2).cloned().unwrap_or_default();
            if user.is_empty() || pass.is_empty() {
                eprintln!("用法: pace-forge login <手机号> <密码>");
                std::process::exit(2);
            }
            do_login(&user, &pass);
        }
        "cheat" => do_cheat(),
        "logout" => do_logout(),
        "relogin" => do_relogin(),
        "check" => {
            let dist_km = parse_num(&args, 1, 2.0, "距离(km)");
            let dur_min = parse_num(&args, 2, 12.0, "时长(min)");
            let (dist_km, dur_min) = checked_km_min(dist_km, dur_min);
            local_check(dist_km, dur_min);
        }
        "run" => {
            let dist_km = parse_num(&args, 1, 2.0, "距离(km)");
            let dur_min = parse_num(&args, 2, 12.0, "时长(min)");
            let (dist_km, dur_min) = checked_km_min(dist_km, dur_min);
            let yes = args.iter().any(|a| a == "-y" || a == "--yes");
            do_run(dist_km, dur_min, yes);
        }
        "run-free" => {
            // run-free [km] [min] [-y]
            // 户外跑 = 自由跑（App 同一类），sportType=4，不计学期成绩。
            let dist_km = parse_num(&args, 1, 2.0, "距离(km)");
            let dur_min = parse_num(&args, 2, 12.0, "时长(min)");
            let (dist_km, dur_min) = checked_km_min(dist_km, dur_min);
            let yes = args.iter().any(|a| a == "-y" || a == "--yes");
            run_with_start(dist_km, dur_min, None, yes, Some(4));
        }
        "ai-list" => do_ai_list(),
        "ai" => {
            // ai <项目id> <成绩> [--minutes] [--days N] [-y]
            // 成绩默认按次数；--minutes 按时长（分钟）。--days N 补签 N 天前。
            let sport = parse_int(&args, 1, 0, "项目id");
            let score = parse_int(&args, 2, 0, "成绩(次数或分钟)");
            let minutes = args.iter().any(|a| a == "--minutes");
            let days = if args.iter().any(|a| a == "--days") {
                Some(flag_int(&args, "--days", 0))
            } else {
                None
            };
            let yes = args.iter().any(|a| a == "-y" || a == "--yes");
            do_ai(sport, score, minutes, days, yes);
        }
        "ai-records" => {
            let sport = parse_int(&args, 1, 0, "项目id");
            let n = parse_int(&args, 2, 20, "条数");
            do_ai_records(sport, n);
        }
        "record" => {
            // record <rrid>：回读单条记录详情（看 sportType / 服务端判定）。
            let rrid = parse_int(&args, 1, 0, "记录id");
            if rrid <= 0 {
                eprintln!("用法: pace-forge record <rrid>");
                std::process::exit(2);
            }
            let client = ai_client();
            match pace_forge::api::records::get_record(&client, rrid) {
                Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default()),
                Err(e) => {
                    eprintln!("回读失败: {e}");
                    std::process::exit(1);
                }
            }
        }
        "dry" => {
            let dist_km = parse_num(&args, 1, 2.0, "距离(km)");
            let dur_min = parse_num(&args, 2, 12.0, "时长(min)");
            let (dist_km, dur_min) = checked_km_min(dist_km, dur_min);
            dry_run(dist_km, dur_min);
        }
        "at" => {
            // at <HH:MM> <km> <min> [--days N]
            let hhmm = args.get(1).cloned().unwrap_or_default();
            let dist_km = parse_num(&args, 2, 2.0, "距离(km)");
            let dur_min = parse_num(&args, 3, 12.0, "时长(min)");
            let (dist_km, dur_min) = checked_km_min(dist_km, dur_min);
            let days: i64 = flag_int(&args, "--days", 0);
            let yes = args.iter().any(|a| a == "-y" || a == "--yes");
            do_at(&hhmm, days, dist_km, dur_min, yes);
        }
        "ago" => {
            // ago <分钟> <km> <min>
            let mins: i64 = parse_int(&args, 1, 60, "分钟数");
            let dist_km = parse_num(&args, 2, 2.0, "距离(km)");
            let dur_min = parse_num(&args, 3, 12.0, "时长(min)");
            let (dist_km, dur_min) = checked_km_min(dist_km, dur_min);
            let yes = args.iter().any(|a| a == "-y" || a == "--yes");
            do_ago(mins, dist_km, dur_min, yes);
        }
        "rand" => {
            // rand <km> <min> [--days N]
            let dist_km = parse_num(&args, 1, 2.0, "距离(km)");
            let dur_min = parse_num(&args, 2, 12.0, "时长(min)");
            let days: i64 = flag_int(&args, "--days", 0);
            let yes = args.iter().any(|a| a == "-y" || a == "--yes");
            do_rand(days, dist_km, dur_min, yes);
        }
        "stadium" => {
            let laps = parse_num(&args, 1, 5.0, "圈数");
            let lane = parse_int(&args, 2, 1, "道次") as u8;
            stadium_probe(laps, lane);
        }
        "config" => pace_forge::config::print_table(),
        "devices" => print!("{}", pace_forge::api::device_table::report()),
        "crosscheck" => do_crosscheck(args.get(1).map(|s| s.as_str())),
        "audit" => do_audit(false, args.get(1).map(|s| s.as_str())),
        "suggest" => do_audit(true, args.get(1).map(|s| s.as_str())),
        "review" => {
            let n: u32 = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(10);
            do_review(n);
        }
        "tracks" => match pace_forge::api::audit::parse_tracks_args(&args[1..]) {
            Ok((file, top)) => do_tracks(file.as_deref(), top),
            Err(e) => {
                eprintln!("{e}\n用法: pace-forge tracks <records.json> [--top N]");
                std::process::exit(2);
            }
        },
        "export-records" => do_export_records(&args[1..]),
        "version" | "-V" | "--version" => {
            println!("pace-forge {}", pace_forge::VERSION);
        }
        "help" | "-h" | "--help" => {
            println!(
                "pace-forge {} — 运动世界校园协议工具\n",
                pace_forge::VERSION
            );
            println!("用法:");
            println!("  pace-forge web [addr]          启动 Web 控制台（默认 127.0.0.1:9999）");
            println!("  pace-forge semester            查学期进度（记分跑）");
            println!("  pace-forge run [km] [min] [-y] 刷跑并提交（-y 跳过确认）");
            println!("  pace-forge run-free [km] [min] [-y]  户外跑（=自由跑，不计成绩）");
            println!("  pace-forge ai-list             AI 运动项目列表");
            println!(
                "  pace-forge ai <id> <成绩> [--minutes] [--days N] [-y]  AI 运动提交（--days 补签）"
            );
            println!("  pace-forge ai-records <id> [N] AI 运动记录");
            println!("  pace-forge check [km] [min]    离线体检轨迹（不联网、不登录）");
            println!("  pace-forge dry [km] [min]      只预演，不提交（要登录）");
            println!("  pace-forge at <HH:MM> [km] [min] [--days N]  补跑：指定当天/往前的时刻");
            println!("  pace-forge ago <分钟> [km] [min]            补跑：N 分钟前");
            println!("  pace-forge rand [km] [min] [--days N]       补跑：当天 7-20 点随机时刻");
            println!("  pace-forge stadium [圈] [道次] 操场轨迹参数");
            println!("  pace-forge config              列出全部 PACE_* 开关与当前取值");
            println!("  pace-forge devices             机型/SoC 表：内容、当前 cpuModel、怎么补表");
            println!(
                "  pace-forge crosscheck <json>   跨记录自检：连续天数、起跑时刻、离散度、轨迹重合"
            );
            println!(
                "  pace-forge export-records [N] [out.json]  导出最近 N 条记录（含轨迹）给 crosscheck"
            );
            println!(
                "  pace-forge audit <json>        账号体检：连续性/时段/离散/重合，命中只提示"
            );
            println!("  pace-forge suggest <json>      同上，再加一段「下一条怎么跑」的动作建议");
            println!(
                "  pace-forge review [N]        逐条查审核状态（status/isCheat/appeal），默认 10 条"
            );
            println!(
                "  pace-forge tracks <json> [--top N]  两两重合矩阵，区分同向与反向；--top 只看前 N 对"
            );
            println!("  pace-forge version             版本号");
            println!();
            println!("规则：开始 + 用时 + 5s 余量 ≤ 现在，最多往前补 3 天。");
            println!(
                "run/dry/at/ago/rand 需要已保存的登录会话（$HOME/.pace-forge/session.json）。"
            );
        }
        other => {
            eprintln!("未知命令: {other}\n用 `pace-forge help` 查看用法");
            std::process::exit(2);
        }
    }
}

fn dry_run(dist_km: f64, dur_min: f64) {
    use pace_forge::api::{flow, session};

    let sess = session::load_session();
    if !sess.is_valid() {
        eprintln!("未登录：先通过 Web 控制台登录一次（会话存在 $HOME/.pace-forge/session.json）");
        std::process::exit(1);
    }
    let client =
        pace_forge::api::client::Client::new(pace_forge::api::client::DeviceProfile::default())
            .with_credentials(pace_forge::api::client::Credentials {
                uid: Some(sess.uid.to_string()),
                token: Some(sess.token.clone()),
                unid: Some(sess.unid.clone()),
            });

    // 中心点必须从真实围栏反推。
    //
    // CLI 原来走 RunParams::default()，center 是 (0.0, 0.0)——赤道的几内亚湾。
    // 围栏选择按「最近」兜底，会选中 2677 公里外的学校，再交给
    // route_through_in_order 就算出 7640 公里、152 万个点的鬼轨迹。
    // 有 PACE_CENTER_LAT/LNG 锚点用锚点；学校没配围栏又没锚点时报可操作的错误，
    // 不再对空数组取 [0]（issue #36）。
    let (clat, clng) = match pace_forge::api::fence::fetch_center(&client) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("拿不到围栏中心点，无法规划轨迹: {e}");
            std::process::exit(1);
        }
    };
    println!("围栏中心: ({clat:.5}, {clng:.5})");

    let p = flow::RunParams {
        center_lat: clat,
        center_lng: clng,
        distance_m: dist_km * 1000.0,
        duration_sec: (dur_min * 60.0).round() as i64,
        prefer_stadium: flow::prefer_stadium_env(),
        ..Default::default()
    };

    println!(
        "预演：{:.1} km / {:.0} 分钟（不发任何写请求）\n",
        dist_km, dur_min
    );
    let mut log = |s: &str| println!("{s}");
    match flow::dry_run(&client, &p, &mut log) {
        Ok(o) => {
            println!("\n=== 完成 ===");
            println!("  规划点数    {}", o.track_points);
            println!("  实际里程    {:.1} m", o.actual_length_m);
            println!("  总步数      {}", o.total_steps);
            println!("  平均步频    {:.1} 步/分", o.avg_cadence);
            println!("  自检        {}", o.report.summary());
            println!(
                "  已提交      {}",
                if o.submitted {
                    "是"
                } else {
                    "否（预演）"
                }
            );
        }
        Err(e) => {
            eprintln!("\n预演失败: {e}");
            std::process::exit(1);
        }
    }
}

/// 完全离线的轨迹体检：不登录、不联网，只看本地合成的数据像不像真跑。
fn local_check(dist_km: f64, dur_min: f64) {
    use pace_forge::track::{
        KinParams, SimParams, StadiumSpec,
        buckets::{make_jitter, speed_buckets},
        detect::{PointRule, inspect},
        simulate,
    };

    let total_m = dist_km * 1000.0;
    let total_sec = (dur_min * 60.0).round() as i64;
    let start_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    println!("离线体检：{dist_km:.1} km / {dur_min:.0} 分钟");
    println!("（不登录、不发请求，只验证本地合成数据）\n");

    // 用操场轨迹，几何确定，便于看点位间距
    let spec = StadiumSpec::new(30.5, 104.0).with_lane(1);
    let lap = pace_forge::track::lap_length(1);
    let laps = (total_m / lap).max(0.5);
    let xy = pace_forge::track::stadium_track(&spec, laps, 5.0);
    println!("  轨迹点      {}", xy.len());
    println!("  绕圈        {laps:.2} 圈（单圈 {lap:.1} m）");

    // 上报采样：等时间 5 秒（与 `flow::dry_run` 同一条路径）
    let times = pace_forge::track::sample_times(
        total_sec,
        start_ms,
        pace_forge::track::SAMPLE_INTERVAL_SEC,
    );
    let track_points = pace_forge::track::sample_by_time(
        &xy,
        &times,
        start_ms as u64,
        &KinParams::default(),
        1.0,
        8.0,
        &[],
        &[],
    );
    let real_len = pace_forge::track::geo::path_length(
        &track_points
            .iter()
            .map(|q| (q.lat, q.lon))
            .collect::<Vec<_>>(),
    )
    .round() as i64;
    let cad = pace_forge::track::cadence_stats(&track_points);
    println!(
        "  上报节奏    {} 点 / {:.2}s {:.1}m 一点（真机 {:.2}s {:.1}m）",
        cad.points,
        cad.mean_dt_sec,
        cad.mean_dd_m,
        pace_forge::track::REAL_MEAN_INTERVAL_SEC,
        14.9,
    );
    let pts3: Vec<(f64, f64, i64)> = track_points
        .iter()
        .map(|q| (q.lat, q.lon, q.time_ms))
        .collect();

    // 分桶
    let sb = speed_buckets(&pts3, start_ms, total_sec);
    // 这里原来还造了一份 step_buckets(..., 180, &jit) 赋给 stb 却从没用过
    // （clippy 一直报 unused variable）——分桶由 simulate() 产出。
    let _ = make_jitter(sb.len().max(1), 1.0, start_ms as u64);

    // 合成
    let sp = SimParams {
        total_distance_m: real_len as f64,
        total_sec,
        start_ms,
        seed: start_ms as u64,
        ..Default::default()
    };
    let sim = match simulate(&pts3, &sp) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("合成失败: {e}");
            return;
        }
    };

    println!("  实际里程    {:.1} m", real_len);
    println!("  总步数      {}", sim.total_steps);
    println!("  平均步频    {:.1} 步/分", sim.avg_cadence);

    // 反检测体检
    let rep = inspect(
        &pts3.iter().map(|(a, b, _)| (*a, *b)).collect::<Vec<_>>(),
        &sb,
        PointRule::default(),
        start_ms + pace_forge::track::startup_sec(total_sec) * 1000,
    );
    println!("\n=== 点位体检（模拟 7.4.01 本地规则）===");
    println!("  {}", rep.summary());
    for line in rep.lines().iter().take(8) {
        println!("  · {line}");
    }
    println!(
        "\n  判定: {}",
        if rep.blocked() {
            "✗ 会被判异常"
        } else {
            "✓ 通过"
        }
    );

    // 分桶统计
    let caps: Vec<f64> = sb.iter().map(|b| b.distance).collect();
    if !caps.is_empty() {
        let mx = caps.iter().cloned().fold(f64::MIN, f64::max);
        let mn = caps.iter().cloned().fold(f64::MAX, f64::min);
        let avg = caps.iter().sum::<f64>() / caps.len() as f64;
        println!(
            "  分桶        {} 个，{:.1}~{:.1} m（均值 {:.1}）",
            caps.len(),
            mn,
            mx,
            avg
        );
    }
}

/// 用本地保存的密码重新登录，刷新 token。
///
/// 密码在 accounts.json 里是加密存的，这里解出来直接用，不用重新输入。
fn do_login(username: &str, password: &str) {
    use pace_forge::api::{client, login as login_api, session};
    let mut c = client::Client::new(client::DeviceProfile::default());
    match login_api::login(&mut c, username, password) {
        Ok(login_api::LoginOutcome::Ok(prof)) => {
            let creds = &c.creds;
            let sess = session::Session {
                uid: creds.uid.clone().unwrap_or_default(),
                token: creds.token.clone().unwrap_or_default(),
                unid: creds.unid.clone().unwrap_or_default(),
                username: username.to_string(),
                name: prof.name.clone(),
                campus_name: prof.campus_name.clone(),
                depart: prof.depart.clone(),
                grade_class: prof.grade_class.clone(),
                device_id: String::new(),
            };
            if let Err(e) = session::save_session(&sess) {
                eprintln!("session 写入失败: {e}");
                std::process::exit(1);
            }
            println!(
                "✅ 登录成功：{}（{} / {}）",
                sess.name, sess.campus_name, username
            );
        }
        Ok(login_api::LoginOutcome::NeedCaptcha { message, .. }) => {
            eprintln!("需要过滑动验证码（{message}），请在网页里登录一次");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("登录失败: {e}");
            std::process::exit(1);
        }
    }
}

fn do_relogin() {
    use pace_forge::api::{accounts, client, login as login_api, session};

    let mut book = accounts::load();
    let acc = match book.accounts.first().cloned() {
        Some(a) => a,
        None => {
            eprintln!("本地没有账号记录");
            std::process::exit(1);
        }
    };
    if acc.password_enc.is_empty() {
        eprintln!("账号 {} 没存密码，需要重新输入", acc.username);
        std::process::exit(1);
    }
    let password = match accounts::open_password(&acc.password_enc) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("密码解密失败: {e}");
            std::process::exit(1);
        }
    };

    let mut c = client::Client::new(client::DeviceProfile::default());
    match login_api::login(&mut c, &acc.username, &password) {
        Ok(login_api::LoginOutcome::Ok(prof)) => {
            let creds = &c.creds;
            let sess = session::Session {
                uid: creds.uid.clone().unwrap_or_default(),
                token: creds.token.clone().unwrap_or_default(),
                unid: creds.unid.clone().unwrap_or_default(),
                username: acc.username.clone(),
                name: prof.name.clone(),
                campus_name: prof.campus_name.clone(),
                depart: prof.depart.clone(),
                grade_class: prof.grade_class.clone(),
                device_id: String::new(),
            };
            if let Err(e) = session::save_session(&sess) {
                eprintln!("session 写入失败: {e}");
            }
            if let Some(a) = book.get_mut(&acc.id) {
                a.uid = sess.uid.clone();
                a.token = sess.token.clone();
                a.unid = sess.unid.clone();
            }
            let _ = accounts::save(&book);
            println!("✅ 重新登录成功：{} ({})", sess.name, acc.username);
        }
        Ok(login_api::LoginOutcome::NeedCaptcha { message, .. }) => {
            eprintln!("需要过滑动验证码（{message}），请在网页里登录一次");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("登录失败: {e}");
            std::process::exit(1);
        }
    }
}

/// 通知服务端注销当前账号，成功后清掉本地 token。
///
/// 只删本地 session 不等于退出登录——服务端的 token 仍然有效，
/// 换台设备照样能用。这里真的打一次 logout 接口。
fn do_logout() {
    let book = pace_forge::api::accounts::load();
    // 账号簿没记录时退回无头登录存的 session（login 命令只写 session）。
    let (uid, token, unid, username) = match book.accounts.first() {
        Some(a) => (
            a.uid.clone(),
            a.token.clone(),
            a.unid.clone(),
            a.username.clone(),
        ),
        None => {
            let s = pace_forge::api::session::load_session();
            if !s.is_valid() {
                eprintln!("本地没有登录态，无需注销");
                return;
            }
            (
                s.uid.clone(),
                s.token.clone(),
                s.unid.clone(),
                s.username.clone(),
            )
        }
    };
    let mut client =
        pace_forge::api::client::Client::new(pace_forge::api::client::DeviceProfile::default())
            .with_credentials(pace_forge::api::client::Credentials {
                token: Some(token),
                uid: Some(uid),
                unid: Some(unid),
            });

    match pace_forge::api::login::logout(&mut client) {
        Ok(true) => println!("✅ 服务端已注销 {username}"),
        Ok(false) => println!("服务端没有返回明确的注销结果，可能是 token 已失效"),
        Err(e) => {
            eprintln!("注销请求失败: {e}");
            std::process::exit(1);
        }
    }

    // 本地也要清，否则下次还会带着这枚已经作废的 token 去请求
    if let Err(e) = pace_forge::api::accounts::clear_tokens() {
        eprintln!("本地 token 清理失败: {e}");
    } else {
        println!("本地 token 已清除");
    }
    pace_forge::api::session::clear_session();
}

/// 违规名单自查。提交成功不代表没被标记——跑批判定是事后做的。
/// 跨记录自检：读一份记录 JSON，打印横向指标。纯离线，不登录。
/// 读 `export-records` 的产物给 audit/suggest/tracks 用。
///
/// 三条命令的输入形状一样，解析和报错也应当一样——抽出来免得三份各写一遍，
/// 日后改字段别名要改三处。
fn read_audit_records(
    file: Option<&str>,
    usage: &str,
) -> Vec<pace_forge::api::crosscheck::RunRecord> {
    use pace_forge::api::crosscheck as cc;
    let Some(path) = file else {
        eprintln!(
            "用法: {usage}\n文件可以是 JSON 数组，也可以是 {{\"records\":[...]}}，（`pace-forge export-records` 导出的就是后者）。"
        );
        std::process::exit(2);
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("读不到 {path}: {e}");
            std::process::exit(1);
        }
    };
    let v: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{path} 不是合法 JSON: {e}");
            std::process::exit(1);
        }
    };
    let records = cc::parse_records(&v);
    if records.is_empty() {
        eprintln!("没读到记录：需要至少有 startTime（或 beginTime）和 totalDis（或 distance）。");
        std::process::exit(1);
    }
    records
}

/// 逐条查审核状态：列表接口不返回 `status`，只能一条条拉详情。
///
/// 起因很具体：一条记录提交后**第 3 天**才被驳回（`APP 参数数据异常 201`），
/// 而列表接口里所有记录看起来都是「正常」。不逐条拉详情，根本不知道哪条被
/// 标记、违规计数有没有涨。
fn do_review(n: u32) {
    use pace_forge::api::{client, records, session};

    let sess = session::load_session();
    if !sess.is_valid() {
        eprintln!("未登录：先执行 `pace-forge web` 在网页里登录一次");
        std::process::exit(1);
    }
    let c = client::Client::new(client::DeviceProfile::default()).with_credentials(
        client::Credentials {
            uid: Some(sess.uid.to_string()),
            token: Some(sess.token.clone()),
            unid: Some(sess.unid.clone()),
        },
    );
    let list = match records::list_records(&c, 1, n, 0) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("拉取记录失败: {e}");
            std::process::exit(1);
        }
    };
    let rows = list
        .get("records")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    println!("审核状态（最近 {} 条，只读）\n", rows.len());
    println!(
        "{:>14} {:>7} {:>8} {:>6}  备注",
        "rrid", "状态", "里程(m)", "申诉"
    );
    let mut flagged = 0;
    for r in &rows {
        let rrid = r.get("rrid").and_then(|v| v.as_i64()).unwrap_or(0);
        let dis = r.get("totalDis").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let Ok(d) = records::get_record(&c, rrid) else {
            continue;
        };
        let status = d.get("status").and_then(|v| v.as_i64()).unwrap_or(-1);
        let cheat = d.get("isCheat").and_then(|v| v.as_i64()).unwrap_or(0);
        let appeal = d.get("appealStatus").and_then(|v| v.as_i64()).unwrap_or(-1);
        let info: String = d
            .get("statusInfo")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .chars()
            .take(26)
            .collect();
        let flag = if status == 0 && cheat == 0 {
            "正常"
        } else {
            flagged += 1;
            "可疑"
        };
        println!("{rrid:>14} {flag:>7} {dis:>8.0} {appeal:>6}  {info}");
    }
    println!();
    if flagged == 0 {
        println!("没有发现被标记的记录。");
    } else {
        println!("⚠ {flagged} 条被标记：审核是跑批的，提交成功不等于安全，隔几天再看一次。");
    }
}

/// `audit` / `suggest` 共用：前者只打发现，后者多打一段动作建议。
fn do_audit(with_suggestions: bool, file: Option<&str>) {
    use pace_forge::api::audit;
    use pace_forge::api::crosscheck as cc;

    let usage = if with_suggestions {
        "pace-forge suggest <records.json>"
    } else {
        "pace-forge audit <records.json>"
    };
    let records = read_audit_records(file, usage);
    let m = audit::metrics(&records);

    println!("[时区] {}", m.tz_desc);
    println!("[记录] {} 条，跨 {} 天", m.count, m.span_days);
    println!(
        "[连续] 最长 {} 天，当前一段 {} 天",
        m.streak_best, m.streak_cur
    );
    println!(
        "[起跑时刻] 最集中 {:02}:00，占 {:.0}%",
        m.hour,
        m.hour_share * 100.0
    );
    println!(
        "[里程] 均值 {:.0}m，离散 {:.1}%",
        m.dist_mean,
        m.dist_cv * 100.0
    );
    println!(
        "[配速] 均值 {}/km，离散 {:.1}%",
        cc::format_pace(m.pace_mean),
        m.pace_cv * 100.0
    );
    match m.overlap {
        Some((a, b, ov)) => {
            let tail = if m.reversed.map(|(ri, rj, _)| (ri, rj)) == Some((a, b)) {
                "（反向，真人行为）"
            } else {
                "（同向）"
            };
            println!(
                "[重合] 第 {} 条 vs 第 {} 条 {:.0}%{tail}（{:.0}m 缓冲，不含时间对齐）",
                a + 1,
                b + 1,
                ov * 100.0,
                cc::OVERLAP_RADIUS_M
            );
        }
        None => println!("[重合] 带轨迹的记录不足两条，算不了"),
    }

    let found = audit::findings(&m);
    println!();
    if found.is_empty() {
        println!("[发现] 没有命中阈值。");
    } else {
        for f in &found {
            let tag = match f.level {
                audit::Level::Warn => "!",
                audit::Level::Info => "·",
            };
            println!("{tag} [{}] {}", f.code, f.text);
        }
    }

    if with_suggestions {
        println!();
        println!("[建议]");
        for (i, s) in audit::suggestions(&m, &found).iter().enumerate() {
            println!("  {}. {s}", i + 1);
        }
    }
    println!("\n阈值是经验值不是协议规则：命中只说明「审核的人会先看这条」，不代表会被判异常。");
    println!("检查码 `AUDIT_*` 与 preflight 的 `PACE_*` 不是一套东西，别混。");
}

/// `tracks`：两两重合矩阵。同向同线是复制特征，反向是真人行为，分开列。
fn do_tracks(file: Option<&str>, top: Option<usize>) {
    // `--top` 只在打印层生效：判定、阈值、findings 一个字节都不动。
    use pace_forge::api::audit;
    use pace_forge::api::crosscheck as cc;

    let records = read_audit_records(file, "pace-forge tracks <records.json>");
    let pairs = audit::overlap_pairs(&records, cc::OVERLAP_RADIUS_M);
    let with_track = records.iter().filter(|r| r.track.len() >= 2).count();

    println!("[时区] {}", cc::tz_description());
    println!(
        "[重合] {} 条记录，其中 {} 条带轨迹（{:.0}m 缓冲，仅空间形状）",
        records.len(),
        with_track,
        cc::OVERLAP_RADIUS_M
    );
    if pairs.is_empty() {
        println!("带轨迹的记录不足两条，没有可比的对。");
        return;
    }
    // 100 条记录就是 4950 对，全打出来没法看；`--top N` 只留两种：
    // ① 同向且达到告警阈值的（真复制）；② 反向的（真人行为，但值得知道）。
    let shown: Vec<&(usize, usize, f64, bool)> = match top {
        Some(n) => {
            let mut v: Vec<&(usize, usize, f64, bool)> = pairs
                .iter()
                .filter(|(_, _, ov, rev)| *rev || *ov >= audit::OVERLAP_WARN)
                .collect();
            v.truncate(n);
            v
        }
        None => pairs.iter().collect(),
    };
    println!();
    if top.is_some() && shown.len() < pairs.len() {
        println!(
            "  只列 {} 对（共 {} 对；同向未达 {:.0}% 的、以及超出 --top 的都省略）",
            shown.len(),
            pairs.len(),
            audit::OVERLAP_WARN * 100.0
        );
    }
    println!("  #a  #b    重合度  方向");
    for (i, j, ov, rev) in &shown {
        let flag = if *rev { "反向（正常）" } else { "同向" };
        println!(
            "  {:>2}  {:>2}   {:>6.1}%  {flag}",
            i + 1,
            j + 1,
            ov * 100.0
        );
    }
    let cloned = pairs
        .iter()
        .filter(|(_, _, ov, rev)| *ov >= audit::OVERLAP_WARN && !*rev)
        .count();
    println!();
    if cloned > 0 {
        println!(
            "! [AUDIT_OVERLAP] 有 {cloned} 对同向重合 ≥ {:.0}%，建议下一条换方向或换线。",
            audit::OVERLAP_WARN * 100.0
        );
    } else {
        println!("没发现同向高重合的对。");
    }
}

fn do_crosscheck(file: Option<&str>) {
    use pace_forge::api::crosscheck as cc;
    let Some(path) = file else {
        eprintln!(
            "用法: pace-forge crosscheck <records.json>\n文件可以是 JSON 数组，也可以是包含 records/list 数组的对象。"
        );
        std::process::exit(2);
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("读不到 {path}: {e}");
            std::process::exit(1);
        }
    };
    let v: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{path} 不是合法 JSON: {e}");
            std::process::exit(1);
        }
    };
    let imported = match cc::parse_local_records(&v) {
        Ok(imported) => imported,
        Err(error) => {
            eprintln!("{path}：{error}");
            std::process::exit(1);
        }
    };
    for issue in &imported.issues {
        eprintln!("{path}：{issue}");
    }
    let records = &imported.records;
    if records.is_empty() {
        eprintln!("{path}：没有可统计的有效记录。");
        std::process::exit(1);
    }
    let tz = cc::tz_offset_sec();
    println!("[时区] {}", cc::tz_description());
    let first = records.iter().map(|r| r.start_ms).min().unwrap_or(0);
    let last = records.iter().map(|r| r.start_ms).max().unwrap_or(0);
    let (y1, m1, d1) = cc::local_date(first, tz);
    let (y2, m2, d2) = cc::local_date(last, tz);
    let span = cc::calendar_span(first, last, tz);
    println!(
        "[记录] {} 条，{y1}-{m1:02}-{d1:02} ~ {y2}-{m2:02}-{d2:02}（跨 {span} 天）",
        records.len()
    );

    let (best, cur) = cc::streak_days(records, tz);
    println!("[连续] 最长 {best} 天，当前一段 {cur} 天");

    let (hour, share) = cc::start_hour_concentration(records, tz);
    println!("[起跑时刻] 最集中 {hour:02}:00，占 {:.0}%", share * 100.0);

    let dists: Vec<f64> = records.iter().map(|r| r.distance_m).collect();
    let (dm, dcv) = cc::dispersion(&dists);
    println!("[里程] 均值 {:.0}m，离散 {:.1}%", dm, dcv * 100.0);

    let paces: Vec<f64> = records
        .iter()
        .filter_map(|record| {
            let pace = (record.duration_sec as f64 * 1000.0) / record.distance_m;
            (pace > 0.0 && pace < i64::MAX as f64).then_some(pace)
        })
        .collect();
    if paces.is_empty() {
        println!("[配速] 无可用记录");
    } else {
        let (pm, pcv) = cc::dispersion(&paces);
        println!(
            "[配速] 均值 {}/km，离散 {:.1}%",
            cc::format_pace(pm),
            pcv * 100.0
        );
    }

    match cc::max_pair_overlap(records, cc::OVERLAP_RADIUS_M) {
        Some((a, b, ov, pairs)) => println!(
            "[重合] 第 {} 条 vs 第 {} 条 最高 {:.0}%（{pairs} 对，{:.0}m 缓冲；仅空间形状，不含时间对齐）",
            imported.row_numbers[a],
            imported.row_numbers[b],
            ov * 100.0,
            cc::OVERLAP_RADIUS_M
        ),
        None => println!("[重合] 带轨迹的记录不足两条，算不了"),
    }
    let near_midnight = cc::near_midnight_count(records, tz);
    if near_midnight > 0 {
        println!("[提示] {near_midnight} 条记录起跑时刻距本地零点不足 1 小时，日期归属受时区影响");
    }
    println!("\n只给数，不给阈值：判不判异常取决于各校规则和自己的实测定论。");
}

/// 导出最近 N 条记录（带轨迹）成一份 JSON，直接喂给 `crosscheck`。
///
/// 列表接口不带轨迹，逐条拉详情；详情里有没有轨迹取决于服务端配置，没有就
/// 留空——`crosscheck` 会把它排除在重合度之外，其余指标照算。
fn do_export_records(rest: &[String]) {
    use pace_forge::api::{client, crosscheck as cc, records, session};
    // 「没写」和「写错」分开：以前 `parse().ok().unwrap_or(20)` 把两者并成一条路，
    // 用户以为导了 50 条、实际导了 20 条，还没有任何提示。
    let force = rest.iter().any(|a| a == "--force");
    let positional: Vec<&String> = rest.iter().filter(|a| !a.starts_with("--")).collect();
    if positional.len() > 2 {
        eprintln!("参数过多：export-records [N] [out.json] [--force]");
        std::process::exit(2);
    }
    let n: u32 = match positional.first() {
        None => 20,
        Some(raw) => match raw.parse::<u32>() {
            Ok(v) if (1..=100).contains(&v) => v,
            Ok(v) => {
                eprintln!("条数必须在 1..100 之间，收到 {v}");
                std::process::exit(2);
            }
            Err(_) => {
                eprintln!("条数不是整数：「{raw}」");
                std::process::exit(2);
            }
        },
    };
    let out_path = positional
        .get(1)
        .map(|s| s.as_str())
        .unwrap_or("records.json");
    if std::path::Path::new(out_path).exists() && !force {
        eprintln!("{out_path} 已存在，加 --force 覆盖，或换一个文件名");
        std::process::exit(2);
    }

    let sess = session::load_session();
    if !sess.is_valid() {
        eprintln!("未登录：先执行 `pace-forge web` 在网页里登录一次");
        std::process::exit(1);
    }
    let c = client::Client::new(client::DeviceProfile::default()).with_credentials(
        client::Credentials {
            uid: Some(sess.uid.to_string()),
            token: Some(sess.token.clone()),
            unid: Some(sess.unid.clone()),
        },
    );

    let list = match records::list_records(&c, 1, n, 1) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("拉记录列表失败: {e}");
            std::process::exit(1);
        }
    };
    let rows = list
        .get("records")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if rows.is_empty() {
        eprintln!("列表是空的：确认账号有计分跑记录，或换用网页里的记录页。");
        std::process::exit(1);
    }

    let mut out: Vec<serde_json::Value> = Vec::with_capacity(rows.len());
    let mut with_track = 0usize;
    for (i, row) in rows.iter().enumerate() {
        let rrid = row
            .get("rrid")
            .or_else(|| row.get("recordId"))
            .or_else(|| row.get("id"))
            .and_then(|v| v.as_i64());
        let detail = rrid
            .and_then(|r| match records::get_record(&c, r) {
                Ok(d) => Some(d),
                Err(e) => {
                    eprintln!("! 第 {} 条（rrid={r}）详情拉取失败: {e}", i + 1);
                    None
                }
            })
            .unwrap_or_else(|| row.clone());
        let track = cc::extract_track(&detail);
        if track.len() >= 2 {
            with_track += 1;
        }
        let start = ["startTime", "start_time", "beginTime"]
            .iter()
            .find_map(|k| detail.get(*k).and_then(|v| v.as_i64()))
            .or_else(|| {
                detail
                    .get("startTime")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<i64>().ok())
            })
            .unwrap_or(0);
        let dist = detail
            .get("totalDistance")
            .and_then(|v| v.as_f64())
            .or_else(|| detail.get("totalDis").and_then(|v| v.as_f64()))
            .unwrap_or(0.0);
        let dur = detail
            .get("totalTime")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let pts: Vec<serde_json::Value> = track
            .iter()
            .map(|(la, ln)| serde_json::json!([la, ln]))
            .collect();
        out.push(serde_json::json!({
            "rrid": rrid,
            "startTime": start,
            "totalDis": dist,
            "totalTime": dur,
            "points": pts,
        }));
        log_export(i + 1, rows.len(), rrid, track.len());
    }

    let body = serde_json::json!({ "exportedAt": now_ms(), "records": out });
    let result = serde_json::to_vec_pretty(&body)
        .map_err(std::io::Error::other)
        .and_then(|bytes| write_record_export(std::path::Path::new(out_path), &bytes, force));
    match result {
        Ok(()) => println!(
            "已导出 {} 条到 {out_path}（带轨迹 {with_track} 条）。\n下一步: pace-forge crosscheck {out_path}",
            out.len()
        ),
        Err(e) if !force && e.kind() == std::io::ErrorKind::AlreadyExists => {
            eprintln!("{out_path} 已存在，加 --force 覆盖，或换一个文件名");
            std::process::exit(2);
        }
        Err(e) => {
            eprintln!("写 {out_path} 失败: {e}");
            std::process::exit(1);
        }
    }
}

fn write_record_export(path: &std::path::Path, bytes: &[u8], force: bool) -> std::io::Result<()> {
    use std::io::Write;

    if force {
        return std::fs::write(path, bytes);
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    if let Err(error) = file.write_all(bytes) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(())
}

fn log_export(i: usize, total: usize, rrid: Option<i64>, pts: usize) {
    println!(
        "  [{i}/{total}] rrid={} 轨迹 {pts} 点",
        rrid.map(|r| r.to_string()).unwrap_or_else(|| "-".into())
    );
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn do_cheat() {
    let sess = pace_forge::api::session::load_session();
    if !sess.is_valid() {
        eprintln!("未登录：先执行 `pace-forge web` 在网页里登录一次");
        std::process::exit(1);
    }
    let client =
        pace_forge::api::client::Client::new(pace_forge::api::client::DeviceProfile::default())
            .with_credentials(pace_forge::api::client::Credentials {
                uid: Some(sess.uid.to_string()),
                token: Some(sess.token.clone()),
                unid: Some(sess.unid.clone()),
            });
    let mut last = String::new();
    match pace_forge::api::cheat::query(&client, 1, &mut |l| {
        last = l.to_string();
    }) {
        Ok(report) => {
            println!("{last}");
            // 「未知」不能报成「清白」——退出码区分开，脚本里才判得对。
            use pace_forge::api::cheat::Status;
            match report.status() {
                Status::Flagged => std::process::exit(2),
                Status::Unknown | Status::Stale => std::process::exit(3),
                Status::NoRecord => {}
            }
        }
        Err(e) => {
            eprintln!("查询失败: {e}");
            std::process::exit(1);
        }
    }
}

fn show_semester() {
    use pace_forge::api::{semester, session};

    let sess = session::load_session();
    if !sess.is_valid() {
        eprintln!("未登录：先执行 `pace-forge web` 在网页里登录一次");
        std::process::exit(1);
    }
    let client =
        pace_forge::api::client::Client::new(pace_forge::api::client::DeviceProfile::default())
            .with_credentials(pace_forge::api::client::Credentials {
                uid: Some(sess.uid.to_string()),
                token: Some(sess.token.clone()),
                unid: Some(sess.unid.clone()),
            });

    let goal = semester::fetch_goal(&client, 1);
    match semester::fetch_summary(&client) {
        Ok(sm) => {
            println!("{}", sm.summary());
            println!("  有效比例 {:.1}%", sm.valid_ratio() * 100.0);
            // 实测（goalMethod=2 的学校）：里程正常返回，次数两项服务端自己给 0——
            // 它按里程考核，不统计次数。不说明的话看起来像没查到。
            let by_distance = goal.as_ref().map(|g| g.goal_method == 2).unwrap_or(false);
            if by_distance && sm.semester_count == 0 && sm.semester_valid_count == 0 {
                println!("  · 本校按里程考核，服务端不统计次数（上面的 0 次不是查询失败）");
                // 服务端不给，就按记录列表自己数：每条带 validDis/sportType，
                // 按学期起止筛一遍即可（不做日上限假设，次数不受上限影响）。
                if let Ok(g) = goal.as_ref()
                    && g.begin_ms > 0
                {
                    match semester::derived_count(&client, g.begin_ms, g.end_ms) {
                        Ok((runs, days)) => println!(
                            "  · 本地按记录反算：{runs} 次计分跑分布在 {days} 天（仅供参考）"
                        ),
                        Err(e) => println!("  · 本地反算失败：{e}"),
                    }
                }
            }
        }
        Err(e) => eprintln!("查学期汇总失败: {e}"),
    }

    match goal {
        Ok(g) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            if !g.in_term(now) {
                println!("⚠ 当前不在学期内");
            }
            if g.target_dis > 0.0 {
                println!(
                    "学期目标 {:.1}km / {} 次，还剩 {} 天",
                    g.target_dis / 1000.0,
                    g.target_count,
                    g.days_left(now)
                );
            } else if g.days_left(now) >= 0 {
                println!(
                    "· 服务端没有下发学期目标里程（semesterGoal=0），学期还剩 {} 天",
                    g.days_left(now)
                );
            } else {
                println!("· 未取到学期目标");
            }
        }
        Err(e) => eprintln!("查学期目标失败: {e}"),
    }
}

fn do_run(dist_km: f64, dur_min: f64, yes: bool) {
    run_with_start(dist_km, dur_min, None, yes, None);
}

/// 通用提交：`start_at` 为 `None` 表示补一条**刚跑完**的记录
///（结束时刻 = 服务器现在，起跑时刻倒推一整个时长）。
///
/// `free_sport`：`Some(4)` 户外跑（= 自由跑，App 同一类）——不拉围栏/打卡点、
/// 报文省略 fivePointJson（neko 实测映射）；`None` 保持原来的刷跑行为。
fn run_with_start(
    dist_km: f64,
    dur_min: f64,
    start_at: Option<i64>,
    yes: bool,
    free_sport: Option<i32>,
) {
    use pace_forge::api::{flow, session};

    let sess = session::load_session();
    if !sess.is_valid() {
        eprintln!("未登录：先执行 `pace-forge web` 在网页里登录一次");
        std::process::exit(1);
    }
    let client =
        pace_forge::api::client::Client::new(pace_forge::api::client::DeviceProfile::default())
            .with_credentials(pace_forge::api::client::Credentials {
                uid: Some(sess.uid.to_string()),
                token: Some(sess.token.clone()),
                unid: Some(sess.unid.clone()),
            });

    // 中心点同样必须从真实围栏反推，不能用默认的 (0,0)。
    // PACE_CENTER_LAT / PACE_CENTER_LNG 显式给了锚点就优先用（自由跑换城市跑、
    // 或学校没配围栏时用）；否则拉围栏取中心。空围栏无锚点不 panic（issue #36）。
    let (clat, clng) = match pace_forge::api::fence::fetch_center(&client) {
        Ok(c) => c,
        Err(e) => {
            if free_sport.is_some() {
                eprintln!(
                    "自由跑拿不到围栏中心（{e}）：请设 PACE_CENTER_LAT / PACE_CENTER_LNG 指定锚点"
                );
            } else {
                eprintln!("拿不到围栏中心点，无法规划轨迹: {e}");
            }
            std::process::exit(1);
        }
    };

    let p = flow::RunParams {
        center_lat: clat,
        center_lng: clng,
        distance_m: dist_km * 1000.0,
        duration_sec: (dur_min * 60.0).round() as i64,
        start_at_ms: start_at,
        custom_path: None,
        sport_type: free_sport.unwrap_or(1),
        free_run: free_sport.is_some(),
        // 户外跑没有服务端时间窗口；记分跑(runMode=1)有，脚本跑容易被 11016 挡掉。
        // 自由跑固定 runMode=2（neko 实测 0/2~6 服务端都当自由跑，无时段限制）。
        run_mode: if free_sport.is_some() {
            2
        } else {
            std::env::var("PACE_RUN_MODE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2)
        },
        sample_step_m: std::env::var("PACE_STEP_M")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5.0),
        prefer_stadium: flow::prefer_stadium_env(),
        ..Default::default()
    };

    let mode_label = match free_sport {
        Some(_) => "户外跑",
        _ => "刷跑",
    };
    println!("{mode_label}：{:.1} km / {:.0} 分钟\n", dist_km, dur_min);
    let mut log = |s: &str| println!("{s}");
    let mut confirm = |o: &flow::RunOutcome| -> bool {
        println!();
        println!("=== 待提交 ===");
        println!("  轨迹点   {}", o.track.len());
        println!("  里程     {:.1} m", o.actual_length_m);
        println!("  步数     {}", o.total_steps);
        println!("  步频     {:.1} 步/分", o.avg_cadence);
        println!("  自检     {}", o.report.summary());
        if yes {
            println!("  （-y 已跳过确认）");
            return true;
        }
        print!("\n确认提交？[y/N] ");
        use std::io::Write;
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok();
        matches!(line.trim(), "y" | "Y" | "yes")
    };

    match flow::run_full(&client, &p, &mut log, &mut confirm) {
        Ok(o) => {
            if o.submitted {
                println!("\n✅ 提交成功，记录 id={:?}", o.record_id);
                println!("   {} 秒内不要再跑", flow::SILENCE_SEC);
                // 提交成功 ≠ 安全：服务端是**事后回扫**判定的（上游 issue #51 实测
                // pace-forge 生成的记录提交时六项全绿，4~7 天后仍被判 isCheat=1、
                // 申诉通道关闭；同账号真机记录一条都没被标）。这里把复查办法直接
                // 印出来，免得过几天发现被判违规却不知道从哪查。
                if let Some(rrid) = o.record_id {
                    println!(
                        "   ⚠ 事后复查：4 天后跑 `pace-forge records 1` 看记录 {rrid} 还在不在，"
                    );
                    println!(
                        "     或 `pace-forge cheat` 看违规条数（被回扫了会 +1，申诉也会关掉）"
                    );
                }
            } else {
                println!("\n已取消");
            }
        }
        Err(e) => {
            eprintln!("\n失败: {e}");
            std::process::exit(1);
        }
    }
}

fn stadium_probe(laps: f64, lane: u8) {
    use pace_forge::track::{
        StadiumSpec, arc_and_curvature, lap_length, path_length, stadium_track,
    };

    let spec = StadiumSpec::new(30.5, 104.0).with_lane(lane);
    let pts = stadium_track(&spec, laps, 5.0);
    let len = path_length(&pts);
    let (_, k) = arc_and_curvature(&pts);
    let max_k = k.iter().cloned().fold(0.0f64, f64::max);

    println!("操场第 {lane} 道");
    println!("  单圈      {:.2} m", lap_length(lane));
    println!("  {laps:.1} 圈      {:.1} m", len);
    println!("  采样点    {}", pts.len());
    println!(
        "  最大曲率  {max_k:.5} /m（理论 {:.5}）",
        1.0 / (36.5 + 0.3 + 1.22 * (lane as f64 - 1.0))
    );
    println!();
    println!(
        "建议：{:.0}m 跑 {:.0} 秒（配速 {:.2} 分/km）",
        len,
        (len / 2.78).round(),
        1000.0 / 2.78 / 60.0
    );
}

// ---------------------------------------------------------------------------
// 补跑：指定时间
// ---------------------------------------------------------------------------

fn show_plan(st: pace_forge::api::start_time::StartTime, dur_min: f64) {
    let dur_s = (dur_min * 60.0).round() as i64;
    println!(
        "开始 {}",
        pace_forge::api::start_time::format_ms(
            st.ms,
            pace_forge::api::start_time::default_tz_offset_min()
        )
    );
    println!(
        "结束 {}",
        pace_forge::api::start_time::format_ms(
            st.stop_ms(dur_s),
            pace_forge::api::start_time::default_tz_offset_min()
        )
    );
    if st.clamped {
        println!("注意：原时刻会让结束时间落在未来，已自动提前");
    }
}

fn do_at(hhmm: &str, days: i64, dist_km: f64, dur_min: f64, yes: bool) {
    use pace_forge::api::start_time as st_mod;
    let (h, m) = match st_mod::parse_hhmm(hhmm) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let now = pace_forge::crypto::envelope::now_ms() as i64;
    let dur_s = (dur_min * 60.0).round() as i64;
    let st = match st_mod::specified(now, days, h, m, dur_s) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    show_plan(st, dur_min);
    run_with_start(dist_km, dur_min, Some(st.ms), yes, None);
}

fn do_ago(mins: i64, dist_km: f64, dur_min: f64, yes: bool) {
    use pace_forge::api::start_time as st_mod;
    let now = pace_forge::crypto::envelope::now_ms() as i64;
    let dur_s = (dur_min * 60.0).round() as i64;
    let st = match st_mod::ago(now, mins, dur_s) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    show_plan(st, dur_min);
    run_with_start(dist_km, dur_min, Some(st.ms), yes, None);
}

fn do_rand(days: i64, dist_km: f64, dur_min: f64, yes: bool) {
    use pace_forge::api::start_time as st_mod;
    use rand::Rng;
    let now = pace_forge::crypto::envelope::now_ms() as i64;
    let dur_s = (dur_min * 60.0).round() as i64;
    let r: f64 = rand::rng().random();
    let st = match st_mod::random_in_day(now, days, dur_s, r) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    show_plan(st, dur_min);
    run_with_start(dist_km, dur_min, Some(st.ms), yes, None);
}

// ---------------------------------------------------------------------------
// AI 运动
// ---------------------------------------------------------------------------

/// 建一个带已存会话的客户端；未登录直接退出。
fn ai_client() -> pace_forge::api::client::Client {
    let sess = pace_forge::api::session::load_session();
    if !sess.is_valid() {
        eprintln!("未登录：先执行 `pace-forge web` 在网页里登录一次");
        std::process::exit(1);
    }
    pace_forge::api::client::Client::new(pace_forge::api::client::DeviceProfile::default())
        .with_credentials(pace_forge::api::client::Credentials {
            uid: Some(sess.uid.to_string()),
            token: Some(sess.token.clone()),
            unid: Some(sess.unid.clone()),
        })
}

fn do_ai_list() {
    use pace_forge::api::ai;
    let client = ai_client();
    match ai::fetch_list(&client) {
        Ok(list) => {
            println!("AI 运动项目（{} 个）：", list.len());
            for s in &list {
                println!("  {:>3}  {}", s.id, s.name);
            }
        }
        Err(e) => {
            eprintln!("拉项目列表失败: {e}");
            std::process::exit(1);
        }
    }
}

fn do_ai(sport: i64, score: i64, minutes: bool, days: Option<i64>, yes: bool) {
    use pace_forge::api::ai::{self, AiMode};
    if sport <= 0 || score <= 0 {
        eprintln!(
            "用法: pace-forge ai <项目id> <成绩> [--minutes] [--days N] [-y]\n先用 ai-list 查项目 id"
        );
        std::process::exit(2);
    }
    let client = ai_client();
    let mode = if minutes {
        AiMode::Minutes(score)
    } else {
        AiMode::Count(score)
    };
    let info = ai::fetch_info(&client, sport);
    let name = ai::fetch_list(&client)
        .ok()
        .and_then(|l| l.into_iter().find(|s| s.id == sport).map(|s| s.name))
        .unwrap_or_else(|| format!("项目{sport}"));
    let kind = if info.sport_type == 2 {
        "计时类"
    } else {
        "计次类"
    };
    let score_txt = if minutes {
        format!("{score} 分钟")
    } else {
        format!("{score} 个")
    };

    // 补签：N 天前 7-20 点随机时刻（与 rand 补跑同一套时间合同）。
    let at = match days {
        Some(d) => {
            use pace_forge::api::start_time as st_mod;
            use rand::Rng;
            let now = pace_forge::crypto::envelope::now_ms() as i64;
            let dur_s = if minutes {
                score * 60
            } else {
                ((score as f64 / 90.0) * 60.0) as i64
            }
            .max(30);
            let r: f64 = rand::rng().random();
            match st_mod::random_in_day(now, d, dur_s, r) {
                Ok(st) => Some(st.ms),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
        None => None,
    };
    let when = match at {
        Some(ms) => pace_forge::api::start_time::format_ms(
            ms,
            pace_forge::api::start_time::default_tz_offset_min(),
        ),
        None => "现在（刚练完）".to_string(),
    };
    println!("AI 运动：{name}（{kind}）{score_txt}，时间 {when}");
    if !yes {
        print!("确认提交？[y/N] ");
        use std::io::Write;
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok();
        if !matches!(line.trim(), "y" | "Y" | "yes") {
            println!("已取消");
            return;
        }
    }
    match ai::upload(&client, sport, mode, at) {
        Ok(v) => println!("✅ 提交成功：{}", v.get("data").unwrap_or(&v)),
        Err(e) => {
            eprintln!("提交失败: {e}");
            std::process::exit(1);
        }
    }
}

fn do_ai_records(sport: i64, n: i64) {
    use pace_forge::api::ai;
    if sport <= 0 {
        eprintln!("用法: pace-forge ai-records <项目id> [条数]");
        std::process::exit(2);
    }
    let client = ai_client();
    match ai::fetch_records(&client, sport, n.max(1)) {
        Ok(data) => {
            let tz = pace_forge::api::start_time::default_tz_offset_min();
            let fmt = |ms: i64| pace_forge::api::start_time::format_ms(ms, tz);
            let groups = data
                .get("list")
                .and_then(|x| x.as_array())
                .cloned()
                .unwrap_or_default();
            if groups.is_empty() {
                println!("该项目暂无记录");
                return;
            }
            for g in &groups {
                let day = g.get("scoreDate").and_then(|x| x.as_i64()).unwrap_or(0);
                println!(
                    "{}（{} 次）",
                    fmt(day),
                    g.get("frequency").and_then(|x| x.as_i64()).unwrap_or(0)
                );
                for r in g
                    .get("recordInfos")
                    .and_then(|x| x.as_array())
                    .cloned()
                    .unwrap_or_default()
                {
                    println!(
                        "  #{} {}  score={}  用时={}s  status={}",
                        r.get("id").and_then(|x| x.as_i64()).unwrap_or(0),
                        r.get("name").and_then(|x| x.as_str()).unwrap_or(""),
                        r.get("score").map(|x| x.to_string()).unwrap_or_default(),
                        r.get("timeConsume").and_then(|x| x.as_i64()).unwrap_or(0) / 1000,
                        r.get("status").and_then(|x| x.as_i64()).unwrap_or(0),
                    );
                }
            }
        }
        Err(e) => {
            eprintln!("拉记录失败: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod record_export_tests {
    use super::write_record_export;
    use std::{fs, io::ErrorKind, sync::Barrier, thread};

    #[test]
    fn concurrent_exports_keep_the_first_file_unless_forced() {
        let directory = std::env::temp_dir().join(format!(
            "pace-forge-export-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>(),
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("records.json");
        let start = Barrier::new(2);
        let (left, right) = thread::scope(|scope| {
            let other = scope.spawn(|| {
                start.wait();
                write_record_export(&path, b"left", false)
            });
            start.wait();
            let result = write_record_export(&path, b"right", false);
            (other.join().unwrap(), result)
        });
        let saved = fs::read(&path);
        let forced = write_record_export(&path, b"replacement", true);
        let replacement = fs::read(&path);
        fs::remove_dir_all(directory).unwrap();

        let (error, expected) = match (left, right) {
            (Ok(()), Err(error)) => (error, b"left".as_slice()),
            (Err(error), Ok(())) => (error, b"right".as_slice()),
            _ => panic!("exactly one export should succeed"),
        };
        assert_eq!(error.kind(), ErrorKind::AlreadyExists);
        assert_eq!(saved.unwrap(), expected);
        forced.unwrap();
        assert_eq!(replacement.unwrap(), b"replacement");
    }
}
