//! YAML configuration router.
//!
//! `zwtcfg_router.yaml` lists the paths of several JSON config files, and the "quick-switch"
//! hotkey cycles through them in order.
//!
//! The router file has a fixed schema (a list of strings under `config:`), so a tiny
//! hand-written parser is enough — no full YAML dependency. As it parses, it records each
//! entry's line number so that, after validation, an invalid entry can be annotated in place
//! with a marker comment.

use crate::lang::tr;
use crate::tfmt;

/// A single router entry: config file name + its 0-based line number in the raw text.
#[derive(Debug)]
pub struct RouterItem {
    pub name: String,
    pub line: usize,
}

/// Parse the file list under the `config:` key of `zwtcfg_router.yaml`.
///
/// Supports three forms (blank lines and `#` comments are ignored):
/// ```yaml
/// config:
///  - first.json
/// ```
/// ```yaml
/// config: [first.json, second.json]
/// ```
/// ```yaml
/// config: first.json
/// ```
///
/// Returns `Err` when the structure violates the expected schema; the caller should then
/// simply skip the load. Error cases:
/// - a top-level key other than `config`;
/// - a non-list item inside the `config` block;
/// - an unclosed inline list (missing `]`);
/// - a duplicate `config` key;
/// - an empty `config` file list.
pub fn parse_router_yaml(text: &str) -> Result<Vec<RouterItem>, String> {
    let mut items: Vec<RouterItem> = Vec::new();
    let mut saw_config = false;
    let mut in_config = false;
    for (idx, raw) in text.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("config:") {
            if saw_config {
                return Err(tfmt!(
                    "第 {} 行: 出现重复的 config 键",
                    "line {}: duplicate config key",
                    line_no
                ));
            }
            saw_config = true;
            let rest = rest.trim();
            if rest.is_empty() {
                in_config = true; // block form: following `- item` lines belong to config
            } else if rest.starts_with('[') {
                if !rest.ends_with(']') {
                    return Err(tfmt!(
                        "第 {} 行: 流式列表未闭合 (缺少 ])",
                        "line {}: inline list not closed (missing ])",
                        line_no
                    ));
                }
                let inner = &rest[1..rest.len() - 1];
                for item in inner.split(',').map(clean) {
                    if !item.is_empty() {
                        items.push(RouterItem { name: item, line: idx });
                    }
                }
                in_config = false;
            } else {
                // single value: config: a.json
                let item = clean(rest);
                if !item.is_empty() {
                    items.push(RouterItem { name: item, line: idx });
                }
                in_config = false;
            }
            continue;
        }
        if in_config {
            if let Some(item) = line.strip_prefix("- ") {
                let item = clean(item);
                if !item.is_empty() {
                    items.push(RouterItem { name: item, line: idx });
                }
            } else if line.starts_with('-') {
                let item = clean(line.trim_start_matches('-'));
                if !item.is_empty() {
                    items.push(RouterItem { name: item, line: idx });
                }
            } else {
                return Err(tfmt!(
                    "第 {} 行: config 列表中出现非列表项 {}",
                    "line {}: non-list item in config list {}",
                    line_no,
                    line
                ));
            }
        } else {
            return Err(tfmt!(
                "第 {} 行: 无法识别的配置 {}，预期为 config 文件列表",
                "line {}: unrecognized config {}, expected a config file list",
                line_no,
                line
            ));
        }
    }
    if items.is_empty() {
        return Err(tr("config 文件列表为空", "config file list is empty").into());
    }
    Ok(items)
}

/// Strip surrounding whitespace and any matched quotes (`"..."` or `'...'`).
fn clean(s: &str) -> String {
    let s = s.trim();
    let unquoted = s
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .or_else(|| s.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')));
    unquoted.unwrap_or(s).to_string()
}

/// Rewrite the router file from its original text: insert a marker comment above each
/// invalid entry, while clearing any `# [无效]` markers left over from a previous run so
/// they don't accumulate.
///
/// `invalid` is `(0-based line of the entry, comment text)`. The line numbers must come from
/// `parse_router_yaml` run on the very same `text`.
pub fn rewrite_with_markers(text: &str, invalid: &[(usize, String)]) -> String {
    use std::collections::HashMap;
    let map: HashMap<usize, &String> = invalid.iter().map(|(l, c)| (*l, c)).collect();
    let mut out: Vec<String> = Vec::new();
    for (idx, raw) in text.lines().enumerate() {
        if raw.trim().starts_with("# [无效]") {
            continue; // drop stale markers, regenerate from the current validation result
        }
        if let Some(comment) = map.get(&idx) {
            out.push((*comment).clone());
        }
        out.push(raw.to_string());
    }
    let mut result = out.join("\n");
    if text.ends_with('\n') {
        result.push('\n');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(items: &[RouterItem]) -> Vec<&str> {
        items.iter().map(|i| i.name.as_str()).collect()
    }

    #[test]
    fn parses_block_list() {
        let yaml = "\
# 路由配置
config:
 - first.json
 - second.json

 - third.json
";
        let items = parse_router_yaml(yaml).unwrap();
        assert_eq!(names(&items), vec!["first.json", "second.json", "third.json"]);
        // 0-based line numbers: first=2, second=3, third=5 (blank line in between).
        assert_eq!(items[0].line, 2);
        assert_eq!(items[1].line, 3);
        assert_eq!(items[2].line, 5);
    }

    #[test]
    fn parses_inline_flow_list() {
        let items = parse_router_yaml("config: [a.json, b.json, c.json]").unwrap();
        assert_eq!(names(&items), vec!["a.json", "b.json", "c.json"]);
    }

    #[test]
    fn parses_single_value() {
        let items = parse_router_yaml("config: only.json").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "only.json");
    }

    #[test]
    fn strips_quotes_and_whitespace() {
        let yaml = "config:\n - \"x y.json\"\n - 'z.json'\n -  spaced.json \n";
        let items = parse_router_yaml(yaml).unwrap();
        assert_eq!(names(&items), vec!["x y.json", "z.json", "spaced.json"]);
    }

    #[test]
    fn empty_or_missing_config_returns_error() {
        assert!(parse_router_yaml("").is_err());
        assert!(parse_router_yaml("# 只有注释\n").is_err());
        assert!(parse_router_yaml("other: 1\n").is_err());
    }

    #[test]
    fn rejects_unexpected_top_level_key() {
        let err = parse_router_yaml("foo: bar\nconfig:\n - a.json\n").unwrap_err();
        assert!(err.contains('1'), "err: {err}");
    }

    #[test]
    fn rejects_non_list_item_in_block() {
        let err = parse_router_yaml("config:\n - a.json\nother: 1\n").unwrap_err();
        assert!(err.contains('3'), "err: {err}"); // line 3 (1-based)
    }

    #[test]
    fn rejects_unclosed_flow_list() {
        assert!(parse_router_yaml("config: [a.json, b.json\n").is_err());
    }

    #[test]
    fn rejects_duplicate_config_key() {
        let err = parse_router_yaml("config:\n - a.json\nconfig:\n - b.json\n").unwrap_err();
        assert!(err.contains('3'), "err: {err}");
    }

    #[test]
    fn rewrite_inserts_clears_and_dedups_markers() {
        let original = "config:\n - ok.json\n - bad.json\n - ok2.json\n";
        let markers = vec![(2, "# [无效] bad.json: 文件不存在".to_string())];
        let out = rewrite_with_markers(original, &markers);
        assert_eq!(
            out,
            "config:\n - ok.json\n# [无效] bad.json: 文件不存在\n - bad.json\n - ok2.json\n"
        );

        // Reload (re-parsing shifts the line number, bad.json is now line 3): no accumulation.
        let again = rewrite_with_markers(&out, &[(3, "# [无效] bad.json: 文件不存在".to_string())]);
        assert_eq!(again, out);

        // After fixing (no markers): stale markers are cleared.
        let fixed = rewrite_with_markers(&out, &[]);
        assert_eq!(fixed, "config:\n - ok.json\n - bad.json\n - ok2.json\n");
    }
}
