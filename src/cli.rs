//! 一次性子命令（如 `mneme memory list`）：不启动服务，直接查/改 SQLite。
//! 供 `podman exec <容器> mneme memory list` 这类运维排查用。

use anyhow::{anyhow, bail, Result};

use crate::config::Config;
use crate::store::{self, ListFilter};

const USAGE: &str = "\
用法：
  mneme memory list [--user <id>] [--limit N] [--all] [--json]
  mneme memory show <id> [--json]
  mneme memory forget <id>

选项（list）：
  -u, --user <id>   只看某个用户（如 qq:c2c:xxxx）
  -n, --limit N     最多列出多少条（默认 200）
  -a, --all         包含已失效的记忆（被遗忘/被取代）
  -j, --json        输出 JSON（含 id / 时间戳 / 过期时间等完整字段）

show   按 id 打印单条完整明细（文本、等级、实体、时间线、状态）。
forget 软删除一条记忆（active=0，可在库里保留痕迹），打印被删的文本。";

/// 分发子命令。args 不含程序名（即 std::env::args().skip(1)）。
pub fn run(cfg: &Config, args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("memory") => memory(cfg, &args[1..]),
        Some("--help" | "-h" | "help") => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => bail!("未知子命令：{other}\n\n{USAGE}"),
        None => unreachable!("run 仅在有参数时被调用"),
    }
}

fn memory(cfg: &Config, args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("list") => memory_list(cfg, &args[1..]),
        Some("show") => memory_show(cfg, &args[1..]),
        Some("forget") => memory_forget(cfg, &args[1..]),
        Some(other) => bail!("未知 memory 子命令：{other}\n\n{USAGE}"),
        None => bail!("memory 需要一个动作\n\n{USAGE}"),
    }
}

fn memory_list(cfg: &Config, args: &[String]) -> Result<()> {
    let mut filter = ListFilter {
        user_id: None,
        include_inactive: false,
        limit: 200,
    };
    let mut as_json = false;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--all" | "-a" => filter.include_inactive = true,
            "--json" | "-j" => as_json = true,
            "--user" | "-u" => {
                let value = it.next().ok_or_else(|| anyhow!("--user 需要一个值"))?;
                filter.user_id = Some(value.clone());
            }
            "--limit" | "-n" => {
                let value = it.next().ok_or_else(|| anyhow!("--limit 需要一个值"))?;
                filter.limit = value
                    .parse()
                    .map_err(|_| anyhow!("--limit 必须是数字：{value}"))?;
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => bail!("未知参数：{other}\n\n{USAGE}"),
        }
    }

    let rows = store::cli_list_memories(cfg, &filter)?;

    if as_json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("（没有记忆）");
        return Ok(());
    }

    let scope = if filter.include_inactive { "（含已失效）" } else { "" };
    // 单人库（或已按用户过滤）时不逐行重复用户，只在标题里带一次。
    let users: std::collections::BTreeSet<&str> = rows.iter().map(|r| r.user_id.as_str()).collect();
    let per_row_user = filter.user_id.is_none() && users.len() > 1;
    match &filter.user_id {
        Some(uid) => println!("共 {} 条{}，user={uid}：", rows.len(), scope),
        None if users.len() == 1 => {
            println!("共 {} 条{}，user={}：", rows.len(), scope, users.iter().next().unwrap())
        }
        None => println!("共 {} 条{}（{} 个用户）：", rows.len(), scope, users.len()),
    }
    for row in &rows {
        // active 用 ✓/✗，日期只留到秒，text 放最后免去 CJK 等宽对齐问题。
        let flag = if row.active { '✓' } else { '✗' };
        let when = row.created_at.get(0..19).unwrap_or(&row.created_at);
        let text = truncate(&row.text, 60);
        let mut line = format!(
            "{flag} {when}  L{:<2} {:<11} ×{}",
            row.level, row.kind, row.repetitions
        );
        // 多用户时才附用户尾号区分；id 只显示 8 位前缀（show/forget 认前缀）。
        if per_row_user {
            line.push_str(&format!("  [{}]", user_tail(&row.user_id)));
        }
        line.push_str(&format!("  {}  {}", id_head(&row.id), text));
        println!("{line}");
    }
    let example = rows.first().map(|r| id_head(&r.id)).unwrap_or_default();
    println!("\n（show/forget 用前缀即可，如 mneme memory show {example}）");
    Ok(())
}

fn memory_show(cfg: &Config, args: &[String]) -> Result<()> {
    let (id, as_json) = parse_id_and_json(args)?;
    let Some(m) = store::cli_show_memory(cfg, &id)? else {
        bail!("没有 id 为 {id} 的记忆");
    };
    if as_json {
        println!("{}", serde_json::to_string_pretty(&m)?);
        return Ok(());
    }
    let status = if m.active { "活跃" } else { "已失效" };
    println!("id         {}", m.id);
    println!("user       {}", m.user_id);
    println!("状态       {status}（subject={}, source={}）", m.subject, m.source);
    println!("分类/等级  {} / L{}", m.kind, m.level);
    println!("计数       重复 ×{}，被检索 ×{}", m.repetitions, m.access_count);
    println!("创建       {}", m.created_at);
    println!("最近提及   {}", m.last_seen_at);
    if let Some(v) = &m.last_accessed_at {
        println!("最近检索   {v}");
    }
    match &m.expires_at {
        Some(v) => println!("过期       {v}"),
        None => println!("过期       永不（L10 或未设）"),
    }
    if let Some(v) = &m.forgotten_at {
        println!("遗忘于     {v}");
    }
    if let Some(v) = &m.superseded_by {
        println!("被取代为   {v}（superseded_at={}）", m.superseded_at.as_deref().unwrap_or("?"));
    }
    if !m.entities.is_empty() {
        let joined = m
            .entities
            .iter()
            .map(|e| format!("{}({})", e.name, e.kind))
            .collect::<Vec<_>>()
            .join("、");
        println!("实体       {joined}");
    }
    println!("\n{}", m.text);
    Ok(())
}

fn memory_forget(cfg: &Config, args: &[String]) -> Result<()> {
    let (id, _) = parse_id_and_json(args)?;
    match store::cli_forget_memory(cfg, &id)? {
        Some(text) => {
            println!("已遗忘（软删除，active=0）：");
            println!("{}", truncate(&text, 200));
            Ok(())
        }
        None => bail!("没有活跃的 id 为 {id} 的记忆（可能不存在或已失效）"),
    }
}

/// 解析 `<id> [--json]`：第一个非选项参数当 id。
fn parse_id_and_json(args: &[String]) -> Result<(String, bool)> {
    let mut id: Option<String> = None;
    let mut as_json = false;
    for arg in args {
        match arg.as_str() {
            "--json" | "-j" => as_json = true,
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other if other.starts_with('-') => bail!("未知参数：{other}\n\n{USAGE}"),
            other => {
                if id.is_some() {
                    bail!("只能指定一个 id");
                }
                id = Some(other.to_string());
            }
        }
    }
    let id = id.ok_or_else(|| anyhow!("需要一个记忆 id\n\n{USAGE}"))?;
    Ok((id, as_json))
}

/// 按字符数截断并加省略号（避免在多字节字符中间切断）。
fn truncate(text: &str, max_chars: usize) -> String {
    let flat = text.replace('\n', " ");
    if flat.chars().count() <= max_chars {
        return flat;
    }
    let mut out: String = flat.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// user_id 取尾 8 字符，便于多用户时人眼区分而不刷屏。
fn user_tail(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 10 {
        value.to_string()
    } else {
        format!("…{}", chars[chars.len() - 8..].iter().collect::<String>())
    }
}

/// 记忆 id 取前 8 字符（UUID 前缀，个人库唯一性足够）；show/forget 认前缀。
fn id_head(id: &str) -> String {
    id.chars().take(8).collect()
}
