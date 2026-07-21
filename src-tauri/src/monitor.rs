use tauri::State;

use crate::{
    models::ServerStats,
    ssh::{exec_collect, get_session},
    AppState,
};

/// 单次 exec 采集所有指标 (两次采样 /proc/stat 与 /proc/net/dev, 间隔 1 秒)
const PROBE_SCRIPT: &str = r#"
echo "@host"; hostname
echo "@stat1"; grep '^cpu ' /proc/stat
echo "@net1"; cat /proc/net/dev
sleep 1
echo "@stat2"; grep '^cpu ' /proc/stat
echo "@net2"; cat /proc/net/dev
echo "@mem"; grep -E '^(MemTotal|MemAvailable|SwapTotal|SwapFree):' /proc/meminfo
echo "@disk"; df -B1 / | tail -1
echo "@uptime"; cat /proc/uptime
echo "@load"; cat /proc/loadavg
echo "@os"; uname -srm
echo "@cores"; nproc
"#;

fn section<'a>(out: &'a str, tag: &str) -> &'a str {
    let marker = format!("@{tag}\n");
    let start = match out.find(&marker) {
        Some(i) => i + marker.len(),
        None => return "",
    };
    let rest = &out[start..];
    let end = rest.find("\n@").map(|i| i + 1).unwrap_or(rest.len());
    rest[..end].trim_end_matches('\n')
}

fn parse_cpu_times(line: &str) -> Option<(u64, u64)> {
    // cpu  user nice system idle iowait irq softirq steal ...
    let nums: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|s| s.parse().ok())
        .collect();
    if nums.len() < 5 {
        return None;
    }
    let idle = nums[3] + nums[4];
    let total: u64 = nums.iter().sum();
    Some((total, idle))
}

fn parse_net(out: &str) -> (u64, u64) {
    let mut rx = 0u64;
    let mut tx = 0u64;
    for line in out.lines().skip(2) {
        let Some((iface, rest)) = line.split_once(':') else {
            continue;
        };
        let iface = iface.trim();
        if iface == "lo" {
            continue;
        }
        let nums: Vec<u64> = rest
            .split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect();
        if nums.len() >= 9 {
            rx += nums[0];
            tx += nums[8];
        }
    }
    (rx, tx)
}

fn meminfo_kb(out: &str, key: &str) -> u64 {
    out.lines()
        .find(|l| l.starts_with(key))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

#[tauri::command]
pub async fn server_stats(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<ServerStats, String> {
    let session = get_session(&state, &session_id)?;
    let out = exec_collect(&session.handle, PROBE_SCRIPT).await?;

    let mut stats = ServerStats {
        hostname: section(&out, "host").trim().to_string(),
        os: section(&out, "os").trim().to_string(),
        cpu_cores: section(&out, "cores").trim().parse().unwrap_or(0),
        ..Default::default()
    };

    // CPU: 两次采样差值
    if let (Some((t1, i1)), Some((t2, i2))) = (
        parse_cpu_times(section(&out, "stat1")),
        parse_cpu_times(section(&out, "stat2")),
    ) {
        let dt = t2.saturating_sub(t1) as f64;
        let di = i2.saturating_sub(i1) as f64;
        if dt > 0.0 {
            stats.cpu_percent = ((1.0 - di / dt) * 100.0).clamp(0.0, 100.0);
        }
    }

    // 网络: 两次采样差值 / 1 秒
    let (rx1, tx1) = parse_net(section(&out, "net1"));
    let (rx2, tx2) = parse_net(section(&out, "net2"));
    stats.net_rx_bps = rx2.saturating_sub(rx1) as f64;
    stats.net_tx_bps = tx2.saturating_sub(tx1) as f64;

    // 内存
    let mem = section(&out, "mem");
    stats.mem_total = meminfo_kb(mem, "MemTotal") * 1024;
    let avail = meminfo_kb(mem, "MemAvailable") * 1024;
    stats.mem_used = stats.mem_total.saturating_sub(avail);
    stats.swap_total = meminfo_kb(mem, "SwapTotal") * 1024;
    stats.swap_used = stats
        .swap_total
        .saturating_sub(meminfo_kb(mem, "SwapFree") * 1024);

    // 磁盘: df -B1 / => Filesystem 1B-blocks Used Available Use% Mounted
    if let Some(line) = section(&out, "disk").lines().next() {
        let nums: Vec<u64> = line
            .split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect();
        if nums.len() >= 3 {
            stats.disk_total = nums[0];
            stats.disk_used = nums[1];
        }
    }

    if let Some(first) = section(&out, "uptime").split_whitespace().next() {
        stats.uptime_secs = first.parse::<f64>().unwrap_or(0.0) as u64;
    }
    let load: Vec<f64> = section(&out, "load")
        .split_whitespace()
        .take(3)
        .filter_map(|s| s.parse().ok())
        .collect();
    if load.len() == 3 {
        stats.load1 = load[0];
        stats.load5 = load[1];
        stats.load15 = load[2];
    }

    Ok(stats)
}
