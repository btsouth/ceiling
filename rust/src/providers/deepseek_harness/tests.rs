use super::{account::*, logs::*};
use chrono::{DateTime, FixedOffset};
use serde_json::{Value, json};
use std::fs;

fn now() -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339("2026-11-05T12:00:00+08:00").unwrap()
}
fn header(version: u64) -> Value {
    json!({"type":"session","version":version,"id":"sample","isSeeded":false})
}
fn event(kind: &str, seq: u64, time: &str, data: Value) -> Value {
    json!({"type":kind,"seq":seq,"time":DateTime::parse_from_rfc3339(time).unwrap().timestamp_millis(),"data":data})
}
fn message(seq: u64, time: &str, input: u64, output: u64, cache: u64) -> Value {
    event(
        "assistant/message",
        seq,
        time,
        json!({"turn":1,"step":seq,"usage":{"inputTokens":input,"outputTokens":output,"cacheReadTokens":cache,"totalTokens":input+output+cache},"stream":[{"chunk":{"type":"usage","usage":{"inputTokens":999999}}}]}),
    )
}
fn lines(rows: &[Value]) -> Vec<u8> {
    rows.iter()
        .map(|v| format!("{v}\n"))
        .collect::<String>()
        .into_bytes()
}

#[test]
fn local_day_and_month_cache_and_embedded_usage_are_counted_once() {
    let rows = vec![
        header(4),
        event(
            "request/header",
            0,
            "2026-10-31T23:59:00+08:00",
            json!({"header":{"config":{"provider":"deepseek-account","model":"deepseek-flash"}}}),
        ),
        message(1, "2026-10-31T23:59:00+08:00", 2, 3, 5),
        message(2, "2026-11-04T23:59:59+08:00", 10, 20, 30),
        message(3, "2026-11-05T00:00:00+08:00", 100, 200, 300),
    ];
    let s = parse_log(&lines(&rows), now()).unwrap();
    assert_eq!(s.today.total, 600);
    assert_eq!(s.month.total, 660);
    assert_eq!(s.lifetime.total, 670);
    assert_eq!(s.month.input, 110);
    assert_eq!(s.month.cache_read, 330);
    assert_eq!(s.month.calls, 2);
    assert_eq!(s.models["deepseek-flash"].total, 660);
}

#[test]
fn highest_generation_and_concatenated_frames_are_read_once() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sessions/project/session-sample");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("session.jsonl"),
        lines(&[
            header(0),
            message(1, "2026-11-05T01:00:00+08:00", 999, 0, 0),
        ]),
    )
    .unwrap();
    let mut bytes = zstd::encode_all(&lines(&[header(4)])[..], 1).unwrap();
    bytes.extend(
        zstd::encode_all(
            &lines(&[message(1, "2026-11-05T01:00:00+08:00", 11, 22, 33)])[..],
            1,
        )
        .unwrap(),
    );
    fs::write(dir.join("session.v4.jsonl.zstd"), bytes).unwrap();
    let s = scan_home(tmp.path(), now()).unwrap();
    assert_eq!(s.month.total, 66);
    assert_eq!(s.month.calls, 1);
    assert_eq!(s.sessions, 1);
    assert!(s.warnings.is_empty());
}

#[test]
fn seeded_history_and_legacy_chunk_duplicate_are_excluded() {
    let mut h = header(4);
    h["isSeeded"] = json!(true);
    let s = parse_log(
        &lines(&[
            h,
            message(1, "2026-11-05T01:00:00+08:00", 500, 0, 0),
            event(
                "session/end-seed",
                2,
                "2026-11-05T01:00:00+08:00",
                json!({}),
            ),
            message(3, "2026-11-05T01:00:00+08:00", 10, 20, 0),
        ]),
        now(),
    )
    .unwrap();
    assert_eq!(s.month.total, 30);
    let chunk = event(
        "assistant/chunk",
        1,
        "2026-11-05T01:00:00+08:00",
        json!({"turn":1,"step":2,"chunk":{"type":"usage","usage":{"inputTokens":10,"outputTokens":20}}}),
    );
    let mut complete = message(2, "2026-11-05T01:00:00+08:00", 10, 20, 0);
    complete["data"]["step"] = json!(2);
    let s = parse_log(&lines(&[header(0), chunk, complete]), now()).unwrap();
    assert_eq!(s.month.total, 30);
    assert_eq!(s.month.calls, 1);
}

#[test]
fn usage_without_turn_or_step_is_skipped_with_a_warning() {
    let mut no_step = message(1, "2026-11-05T01:00:00+08:00", 10, 20, 0);
    no_step["data"].as_object_mut().unwrap().remove("step");
    let mut no_turn = message(2, "2026-11-05T01:00:00+08:00", 1, 2, 0);
    no_turn["data"].as_object_mut().unwrap().remove("turn");
    let counted = message(3, "2026-11-05T01:00:00+08:00", 100, 200, 0);
    let s = parse_log(&lines(&[header(0), no_step, no_turn, counted]), now()).unwrap();
    assert_eq!(s.month.total, 300);
    assert_eq!(s.month.calls, 1);
    assert!(s.warnings.iter().any(|w| w.contains("turn or step")));
}

#[test]
fn input_including_cache_is_normalized_from_reported_total() {
    let mut r = message(1, "2026-11-05T01:00:00+08:00", 100, 20, 60);
    r["data"]["usage"]["totalTokens"] = json!(120);
    let s = parse_log(&lines(&[header(4), r]), now()).unwrap();
    assert_eq!(s.month.total, 120);
    assert_eq!(s.month.input, 40);
    assert_eq!(s.month.cache_read, 60);
}

#[test]
fn malformed_and_future_logs_are_not_silently_zero() {
    assert!(parse_log(&lines(&[header(99)]), now()).is_err());
    assert!(parse_log(b"{bad json}\n", now()).is_err());
    let mut raw = lines(&[header(4), message(1, "2026-11-05T01:00:00+08:00", 2, 3, 0)]);
    raw.extend(b"{incomplete\xe4");
    let s = parse_log(&raw, now()).unwrap();
    assert_eq!(s.month.total, 5);
    assert!(!s.warnings.is_empty());
    let mut seeded = header(4);
    seeded["isSeeded"] = json!(true);
    assert!(parse_log(&lines(&[seeded]), now()).is_err());
}

#[test]
fn unsafe_issuers_and_invalid_money_are_rejected_without_leaking_secrets() {
    for issuer in [
        "http://platform.deepseek.com",
        "https://platform.deepseek.com.evil.test",
        "https://platform.deepseek.com@evil.test",
        "https://platform.deepseek.com/path",
    ] {
        assert!(parse_grant(&format!("version: 1\nrecords:\n  deepseek-account-platform/default:\n    kind: grant\n    payload:\n      version: 1\n      issuer: '{issuer}'\n      token: secret-never-show\n")).is_err());
    }
    let broken = "version: 1\nrecords: [secret-never-show";
    let error = parse_grant(broken).err().unwrap();
    assert!(!error.contains("secret-never-show"));
    let good = "version: 1\nrecords:\n  deepseek-account-platform/default:\n    kind: grant\n    payload:\n      version: 1\n      issuer: https://platform.deepseek.com\n      token: example\n";
    assert!(parse_grant(good).unwrap().is_some());
    let wallets=parse_wallets(&json!({"code":0,"data":{"biz_code":0,"biz_data":{"normal_wallets":[{"currency":"CNY","balance":"42.125"}],"bonus_wallets":[{"currency":"CNY","balance":"0E-16"}]}}})).unwrap();
    assert_eq!(wallets.normal[0].balance, 42.125);
    assert_eq!(wallets.bonus[0].balance, 0.0);
    assert!(parse_wallets(&json!({"code":40003})).is_err());
    assert!(parse_wallets(&json!({"code":0,"data":{"biz_code":0,"biz_data":{"normal_wallets":[{"currency":"CNY","balance":"NaN"}],"bonus_wallets":[]}}})).is_err());
}

#[cfg(unix)]
#[test]
fn an_unreadable_session_folder_does_not_hide_the_others() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let good = tmp.path().join("sessions/project/good");
    let locked = tmp.path().join("sessions/project/locked");
    fs::create_dir_all(&good).unwrap();
    fs::create_dir_all(&locked).unwrap();
    let rows = [
        header(4),
        message(1, "2026-11-05T01:00:00+08:00", 10, 20, 0),
    ];
    fs::write(good.join("session.jsonl"), lines(&rows)).unwrap();
    fs::write(locked.join("session.jsonl"), lines(&rows)).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let readable_anyway = fs::read_dir(&locked).is_ok();
    let result = scan_home(tmp.path(), now());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    if readable_anyway {
        return; // Running as root; permissions can't be tested here.
    }
    let s = result.unwrap();
    assert_eq!(s.sessions, 1);
    assert_eq!(s.month.total, 30);
    assert!(s.warnings.iter().any(|w| w.contains("session folder")));
}
