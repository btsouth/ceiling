//! Read-only statistics from DSH's physical session generations.
use chrono::{DateTime, Datelike, TimeZone, Utc};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    io::{ErrorKind, Read},
    path::Path,
};

const MAX_LOG_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
pub(super) struct Counts {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total: u64,
    pub calls: u64,
}
impl Counts {
    fn add(&mut self, other: &Self) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
        self.total = self.total.saturating_add(other.total);
        self.calls = self.calls.saturating_add(other.calls);
    }
    fn from_usage(value: &Value) -> Result<Self, String> {
        let number = |key: &str| -> Result<u64, String> {
            match value.get(key) {
                None => Ok(0),
                Some(v) => v.as_u64().ok_or_else(|| "用量记录包含无效数字".into()),
            }
        };
        if !value.is_object()
            || value.get("inputTokens").is_none()
            || value.get("outputTokens").is_none()
        {
            return Err("用量记录缺少输入或输出 Token".into());
        }
        let input = number("inputTokens")?;
        let output = number("outputTokens")?;
        let cache_read = number("cacheReadTokens")?;
        let cache_write = number("cacheWriteTokens")?;
        // DSH Messages adapters exclude cache from input; other adapters may
        // include it. The provider's total disambiguates these conventions.
        let total = if value.get("totalTokens").is_some() {
            number("totalTokens")?
        } else {
            input
                .saturating_add(output)
                .saturating_add(cache_read)
                .saturating_add(cache_write)
        };
        let non_input = output
            .saturating_add(cache_read)
            .saturating_add(cache_write);
        if total < non_input {
            return Err("用量总数与缓存数字不一致".into());
        }
        Ok(Self {
            input: total - non_input,
            output,
            cache_read,
            cache_write,
            total,
            calls: 1,
        })
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct Summary {
    pub today: Counts,
    pub month: Counts,
    pub lifetime: Counts,
    pub models: BTreeMap<String, Counts>,
    pub sessions: u64,
    pub warnings: Vec<String>,
}
impl Summary {
    fn merge(&mut self, other: Self) {
        self.today.add(&other.today);
        self.month.add(&other.month);
        self.lifetime.add(&other.lifetime);
        self.sessions += other.sessions;
        for (model, counts) in other.models {
            self.models.entry(model).or_default().add(&counts);
        }
        self.warnings.extend(other.warnings);
    }
}

struct UsageRecord {
    priority: u8,
    time: i64,
    model: String,
    counts: Counts,
}

/// Only complete lines count; parse errors never contain source excerpts.
pub(super) fn parse_log(bytes: &[u8], now: DateTime<impl TimeZone>) -> Result<Summary, String> {
    let mut s = Summary::default();
    let end = if bytes.ends_with(b"\n") {
        bytes.len()
    } else {
        s.warnings.push("正在写入的末尾记录暂未计入".into());
        bytes
            .iter()
            .rposition(|&byte| byte == b'\n')
            .map_or(0, |i| i + 1)
    };
    let text =
        std::str::from_utf8(&bytes[..end]).map_err(|_| "会话记录不是有效 UTF-8".to_string())?;
    let rows: Vec<Value> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(|_| "会话记录含损坏的 JSON 行".to_string()))
        .collect::<Result<_, _>>()?;
    let h = rows.first().ok_or("会话记录缺少头部")?;
    let version = h["version"].as_u64().ok_or("会话记录缺少版本")?;
    if h["type"] != "session" || version > 4 {
        return Err("暂不支持此 DSH 会话格式".into());
    }
    let mut cut = h["seedLength"].as_u64().unwrap_or(0) as usize;
    if h["isSeeded"].as_bool() == Some(true) {
        cut = rows
            .iter()
            .rposition(|r| r["type"] == "session/end-seed")
            .ok_or("继承的会话缺少边界，暂未统计以免重复")?;
    }
    if cut >= rows.len() {
        return Err("会话继承边界无效".into());
    }
    // Keep only one usage record per step: assembled usage takes precedence
    // over v0 streaming usage, and embedded stream copies are never traversed.
    let mut records = BTreeMap::<_, UsageRecord>::new();
    let mut model = "未标注模型".to_string();
    for (index, row) in rows.iter().enumerate().skip(1) {
        let kind = row["type"].as_str().unwrap_or("");
        let d = &row["data"];
        if kind == "request/header" {
            if let Some(name) = d.pointer("/header/config/model").and_then(Value::as_str) {
                model = name.to_string();
            }
        } else if kind == "model/selection"
            && let Some(name) = d["model"].as_str()
        {
            model = name.to_string();
        }
        if index <= cut {
            continue;
        }
        let (usage, priority, family) = match kind {
            "assistant/message" => (d.get("usage"), 2, "model"),
            "assistant/chunk"
                if d.pointer("/chunk/type").and_then(Value::as_str) == Some("usage") =>
            {
                (d.pointer("/chunk/usage"), 1, "model")
            }
            "assistant/attempt" => (d.get("usage"), 2, "attempt"),
            "compaction" | "context/compacted" => (d.get("usage"), 2, "compaction"),
            _ => continue,
        };
        let Some(usage) = usage else {
            continue;
        };
        let time = row["time"].as_i64().ok_or("用量记录缺少时间")?;
        let turn = d["turn"].as_u64().unwrap_or(0);
        let step = d["step"]
            .as_u64()
            .unwrap_or(row["seq"].as_u64().unwrap_or(index as u64));
        let key = (turn, step, family);
        let counts = Counts::from_usage(usage)?;
        if records.get(&key).is_none_or(|old| priority >= old.priority) {
            records.insert(
                key,
                UsageRecord {
                    priority,
                    time,
                    model: model.clone(),
                    counts,
                },
            );
        }
    }
    s.sessions = 1;
    for UsageRecord {
        time,
        model,
        counts,
        ..
    } in records.into_values()
    {
        let date = DateTime::<Utc>::from_timestamp_millis(time)
            .ok_or("用量记录时间无效")?
            .with_timezone(&now.timezone());
        if date > now {
            s.warnings.push("有未来时间的记录未计入".into());
            continue;
        }
        s.lifetime.add(&counts);
        if date.year() == now.year() && date.month() == now.month() {
            s.month.add(&counts);
            s.models.entry(model).or_default().add(&counts);
            if date.date_naive() == now.date_naive() {
                s.today.add(&counts);
            }
        }
    }
    Ok(s)
}

fn generation(name: &str) -> Option<u64> {
    let stem = name
        .strip_suffix(".zstd")
        .unwrap_or(name)
        .strip_suffix(".jsonl")?;
    if stem == "session" {
        Some(0)
    } else {
        stem.strip_prefix("session.v")?.parse().ok()
    }
}

/// Directory depth is fixed by DSH; links are not followed.
pub(super) fn scan_home(home: &Path, now: DateTime<impl TimeZone>) -> Result<Summary, String> {
    let root = home.join("sessions");
    if !root.exists() {
        return Err("未找到 DSH 会话目录，请先在 Harness 中进行一次对话".into());
    }
    let mut total = Summary::default();
    for project in fs::read_dir(root).map_err(|_| "无法读取 DSH 会话目录")? {
        let project = project.map_err(|_| "无法读取 DSH 项目")?;
        if !project
            .file_type()
            .map_err(|_| "无法读取 DSH 项目")?
            .is_dir()
        {
            continue;
        }
        for session in fs::read_dir(project.path()).map_err(|_| "无法读取 DSH 项目目录")? {
            let session = session.map_err(|_| "无法读取 DSH 会话")?;
            if !session
                .file_type()
                .map_err(|_| "无法读取 DSH 会话")?
                .is_dir()
            {
                continue;
            }
            let mut selected: Option<(u64, std::path::PathBuf)> = None;
            let mut ambiguous = false;
            for file in fs::read_dir(session.path()).map_err(|_| "无法读取 DSH 会话目录")? {
                let file = file.map_err(|_| "无法读取 DSH 会话文件")?;
                if !file
                    .file_type()
                    .map_err(|_| "无法读取 DSH 会话文件")?
                    .is_file()
                {
                    continue;
                }
                if let Some(v) = generation(&file.file_name().to_string_lossy()) {
                    if selected.as_ref().is_none_or(|(current, _)| v > *current) {
                        selected = Some((v, file.path()));
                        ambiguous = false;
                    } else if selected.as_ref().is_some_and(|(current, _)| v == *current) {
                        ambiguous = true;
                    }
                }
            }
            let Some((_, file)) = selected else {
                continue;
            };
            if ambiguous {
                total
                    .warnings
                    .push("一个会话存在同版本的多份记录，暂未计入".into());
                continue;
            }
            let result = (|| -> Result<Summary, String> {
                let handle = fs::File::open(&file).map_err(|_| "会话文件无法打开")?;
                if handle.metadata().map_err(|_| "会话文件无法读取")?.len() > MAX_LOG_BYTES
                {
                    return Err("会话文件超过读取上限".into());
                }
                let mut decoded = Vec::new();
                let mut torn = false;
                if file.extension().is_some_and(|e| e == "zstd") {
                    let decoder =
                        zstd::stream::read::Decoder::new(handle).map_err(|_| "会话压缩头部损坏")?;
                    if let Err(error) = decoder.take(MAX_LOG_BYTES + 1).read_to_end(&mut decoded) {
                        if error.kind() == ErrorKind::UnexpectedEof {
                            torn = true;
                        } else {
                            return Err("会话压缩数据损坏".into());
                        }
                    }
                } else {
                    handle
                        .take(MAX_LOG_BYTES + 1)
                        .read_to_end(&mut decoded)
                        .map_err(|_| "会话文件读取失败")?;
                }
                if decoded.len() as u64 > MAX_LOG_BYTES {
                    return Err("会话解压后超过读取上限".into());
                }
                let mut s = parse_log(&decoded, now.clone())?;
                if torn {
                    s.warnings.push("正在写入的压缩末尾暂未完全计入".into());
                }
                Ok(s)
            })();
            match result {
                Ok(s) => total.merge(s),
                Err(error) => total.warnings.push(error),
            }
        }
    }
    total.warnings.sort();
    total.warnings.dedup();
    if total.sessions == 0 && !total.warnings.is_empty() {
        return Err(total.warnings.join("；"));
    }
    Ok(total)
}
