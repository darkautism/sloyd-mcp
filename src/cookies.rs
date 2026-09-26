use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NormalizedCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: Option<String>,
    pub secure: Option<bool>,
    #[serde(rename = "httpOnly")]
    pub http_only: Option<bool>,
    #[serde(rename = "sameSite")]
    pub same_site: Option<String>,
    pub expires: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct CookieImport {
    pub cookies: Vec<NormalizedCookie>,
    pub dropped_non_sloyd: usize,
}

pub async fn import_cookie_file(path: &Path) -> Result<CookieImport> {
    let text = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("read cookie export {}", path.display()))?;
    if text.trim_start().starts_with('[') || text.trim_start().starts_with('{') {
        parse_json(&text)
    } else {
        parse_netscape(&text)
    }
}

fn is_sloyd(domain: &str) -> bool {
    let d = domain.trim_start_matches('.').to_ascii_lowercase();
    d == "sloyd.ai" || d.ends_with(".sloyd.ai")
}

fn parse_json(text: &str) -> Result<CookieImport> {
    let root: Value = serde_json::from_str(text)?;
    let items = match &root {
        Value::Array(v) => v,
        Value::Object(o) => o
            .get("cookies")
            .and_then(Value::as_array)
            .context("cookie JSON must be array or {cookies:[...]}")?,
        _ => bail!("invalid cookie JSON"),
    };
    let mut cookies = Vec::new();
    let mut dropped = 0;
    for item in items {
        let Some(o) = item.as_object() else { continue };
        let Some(name) = o.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(value) = o.get("value").and_then(Value::as_str) else {
            continue;
        };
        let domain = o
            .get("domain")
            .or_else(|| o.get("host"))
            .and_then(Value::as_str);
        let Some(domain) = domain else { continue };
        if !is_sloyd(domain) {
            dropped += 1;
            continue;
        }
        cookies.push(NormalizedCookie {
            name: name.into(),
            value: value.into(),
            domain: domain.into(),
            path: o
                .get("path")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or(Some("/".into())),
            secure: o.get("secure").and_then(Value::as_bool),
            http_only: o.get("httpOnly").and_then(Value::as_bool),
            same_site: o
                .get("sameSite")
                .and_then(Value::as_str)
                .and_then(normalize_same_site),
            expires: o
                .get("expires")
                .and_then(Value::as_f64)
                .or_else(|| o.get("expirationDate").and_then(Value::as_f64)),
        });
    }
    if cookies.is_empty() {
        bail!("cookie export contained no *.sloyd.ai cookies");
    }
    Ok(CookieImport {
        cookies,
        dropped_non_sloyd: dropped,
    })
}

fn normalize_same_site(value: &str) -> Option<String> {
    match value.to_ascii_lowercase().as_str() {
        "strict" => Some("Strict".into()),
        "lax" => Some("Lax".into()),
        "none" | "no_restriction" | "no-restriction" => Some("None".into()),
        _ => None,
    }
}

fn parse_netscape(text: &str) -> Result<CookieImport> {
    let mut cookies = Vec::new();
    let mut dropped = 0;
    for raw in text.lines() {
        if raw.is_empty() || (raw.starts_with('#') && !raw.starts_with("#HttpOnly_")) {
            continue;
        }
        let (line, http_only) = raw
            .strip_prefix("#HttpOnly_")
            .map(|x| (x, true))
            .unwrap_or((raw, false));
        let f: Vec<&str> = line.splitn(7, '\t').collect();
        if f.len() != 7 {
            continue;
        }
        if !is_sloyd(f[0]) {
            dropped += 1;
            continue;
        }
        cookies.push(NormalizedCookie {
            name: f[5].into(),
            value: f[6].into(),
            domain: f[0].into(),
            path: Some(f[2].into()),
            secure: Some(f[3].eq_ignore_ascii_case("TRUE")),
            http_only: Some(http_only),
            same_site: None,
            expires: f[4].parse::<f64>().ok().filter(|v| *v > 0.0),
        });
    }
    if cookies.is_empty() {
        bail!("cookie export contained no *.sloyd.ai cookies");
    }
    Ok(CookieImport {
        cookies,
        dropped_non_sloyd: dropped,
    })
}
