use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, Result};
use fancy_regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

pub const CONFIG_VERSION: u64 = 1;
pub const REPO_CONFIG_FILE: &str = ".t3-workflow.json";
pub const SLOTS: &[&str] = &["review", "passes", "credentials", "delivery", "models"];
const TOP_KEYS: &[&str] = &["version", "review", "passes", "credentials", "delivery", "models", "writtenBy", "steps"];
pub const DELIVERY_MODES: &[&str] = &["merge", "tag-deploy"];
pub const DELIVERY_KINDS: &[&str] = &["builtin", "skill", "manual"];
pub const DELIVERY_GATES: &[&str] = &["always", "chain", "never"];
pub const DELIVERY_SCOPES: &[&str] = &["pr", "ticket", "post-handoff"];
pub const DELIVERY_CLOSE_AFTER: &[&str] = &["pr", "ticket", "all"];
pub const DEFAULT_CLOSE_AFTER: &str = "ticket";
const DELIVERY_STEP_KEYS: &[&str] = &["id", "kind", "skill", "prompt", "gate", "scope", "paths", "watch"];
pub const MODEL_ROLES: &[&str] = &["ticket", "worker"];
const DEFAULT_REVIEW: &str = "sol-loop";
pub const DEFAULT_PROVIDER: &str = "claudeAgent";
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
pub const DEFAULT_EFFORT: &str = "high";
const REVIEW_SUGGESTION: &str = "sol-loop from https://github.com/t3dotgg/tstack";
const POOL_NOTE: &str = "t3cm picks the instance at launch with wf.pick";

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("built-in pattern")
}

static HEADING: LazyLock<Regex> = LazyLock::new(|| re(r"^(#{1,6})\s+(.*?)\s*#*\s*$"));
static BULLET: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*[-*]\s+([^:]+?):\s*(.*)$"));
static TICKED: LazyLock<Regex> = LazyLock::new(|| re(r"`([^`]+)`"));
static ARROW: LazyLock<Regex> = LazyLock::new(|| re(r"\s*(?:->|→)\s*"));
static POOL: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^\s*(?:the\s+)?pool:?\s+(.*)$"));
static EFFORT_WORD: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\b(low|medium|high|xhigh|max|ultrathink)\b"));
static FRONTMATTER: LazyLock<Regex> = LazyLock::new(|| re(r"(?s)^---\s*\n(.*?)\n---\s*(?:\n|$)"));
static META_LINE: LazyLock<Regex> = LazyLock::new(|| re(r"^([A-Za-z0-9_-]+):\s*(.*)$"));

fn captures<'t>(r: &Regex, text: &'t str) -> Option<fancy_regex::Captures<'t>> {
    r.captures(text).ok().flatten()
}

fn group<'t>(c: &fancy_regex::Captures<'t>, i: usize) -> &'t str {
    c.get(i).map_or("", |m| m.as_str())
}

pub const EFFORT_OPTIONS: &[&str] = &["effort", "thinking_level", "reasoningEffort"];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ReFlags {
    ascii: bool,
    icase: bool,
    verbose: bool,
}

impl ReFlags {
    fn engine_icase(self) -> bool {
        self.icase && !self.ascii
    }
}

struct ParsedFlags {
    consumed: usize,
    on: String,
    off: String,
    scoped: bool,
}

fn parse_inline_flags(chars: &[char]) -> Option<ParsedFlags> {
    let mut i = 0;
    let mut neg = false;
    let mut on = String::new();
    let mut off = String::new();
    let mut has_a = false;
    let mut has_u = false;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ')' | ':' => {
                let scoped = c == ':';
                if has_a && has_u {
                    return None;
                }
                if neg && off.is_empty() {
                    return None;
                }
                if !scoped && on.is_empty() && off.is_empty() {
                    return None;
                }
                return Some(ParsedFlags { consumed: i + 1, on, off, scoped });
            }
            '-' if !neg => neg = true,
            'a' if !neg => {
                has_a = true;
                on.push(c);
            }
            'u' if !neg => {
                has_u = true;
                on.push(c);
            }
            'i' | 'm' | 's' | 'x' => {
                if neg {
                    off.push(c);
                } else {
                    on.push(c);
                }
            }
            _ => return None,
        }
        i += 1;
    }
    None
}

fn consume_escape(chars: &[char], idx: &mut usize, in_class: bool) -> String {
    let mut out = String::new();
    out.push(chars[*idx]);
    *idx += 1;
    let Some(&next) = chars.get(*idx) else { return out };
    out.push(next);
    *idx += 1;
    match next {
        'x' => {
            if chars.get(*idx) == Some(&'{') {
                while let Some(&c) = chars.get(*idx) {
                    out.push(c);
                    *idx += 1;
                    if c == '}' {
                        break;
                    }
                }
            } else {
                for _ in 0..2 {
                    if let Some(&c) = chars.get(*idx).filter(|c| c.is_ascii_hexdigit()) {
                        out.push(c);
                        *idx += 1;
                    } else {
                        break;
                    }
                }
            }
        }
        'u' => {
            if chars.get(*idx) == Some(&'{') {
                while let Some(&c) = chars.get(*idx) {
                    out.push(c);
                    *idx += 1;
                    if c == '}' {
                        break;
                    }
                }
            } else {
                for _ in 0..4 {
                    if let Some(&c) = chars.get(*idx).filter(|c| c.is_ascii_hexdigit()) {
                        out.push(c);
                        *idx += 1;
                    } else {
                        break;
                    }
                }
            }
        }
        'U' => {
            for _ in 0..8 {
                if let Some(&c) = chars.get(*idx).filter(|c| c.is_ascii_hexdigit()) {
                    out.push(c);
                    *idx += 1;
                } else {
                    break;
                }
            }
        }
        'N' | 'p' | 'P' if chars.get(*idx) == Some(&'{') => {
            while let Some(&c) = chars.get(*idx) {
                out.push(c);
                *idx += 1;
                if c == '}' {
                    break;
                }
            }
        }
        'g' if !in_class && chars.get(*idx) == Some(&'<') => {
            while let Some(&c) = chars.get(*idx) {
                out.push(c);
                *idx += 1;
                if c == '>' {
                    break;
                }
            }
        }
        _ => {}
    }
    out
}

fn ascii_class_escape(next: char) -> Option<&'static str> {
    match next {
        'w' => Some("0-9A-Za-z_"),
        'W' => Some(r"\x00-\x2f\x3a-\x40\x5b-\x5e\x60\x7b-\u{10ffff}"),
        'd' => Some("0-9"),
        'D' => Some(r"\x00-\x2f\x3a-\u{10ffff}"),
        's' => Some(r"\x09-\x0d\x20"),
        'S' => Some(r"\x00-\x08\x0e-\x1f\x21-\u{10ffff}"),
        _ => None,
    }
}

fn parse_escaped_char(esc: &str, in_class: bool) -> Option<char> {
    let rest = esc.strip_prefix('\\')?;
    let mut chars = rest.chars();
    let first = chars.next()?;
    let tail = chars.as_str();
    match first {
        'x' | 'u' => {
            let hex = tail.strip_prefix('{').and_then(|s| s.strip_suffix('}')).unwrap_or(tail);
            if hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
        }
        'U' if tail.len() == 8 && tail.chars().all(|c| c.is_ascii_hexdigit()) => {
            u32::from_str_radix(tail, 16).ok().and_then(char::from_u32)
        }
        'n' => Some('\n'),
        'r' => Some('\r'),
        't' => Some('\t'),
        'f' => Some('\x0c'),
        'v' => Some('\x0b'),
        'a' => Some('\x07'),
        'b' if in_class => Some('\x08'),
        c if tail.is_empty() && !c.is_ascii_alphanumeric() => Some(c),
        _ => None,
    }
}

enum ClassAtom {
    Shorthand(&'static str),
    Literal { raw: String, ch: Option<char> },
}

fn next_class_atom(chars: &[char], idx: &mut usize, flags: ReFlags) -> ClassAtom {
    let c = chars[*idx];
    if c == '\\' {
        if flags.ascii
            && let Some(&next) = chars.get(*idx + 1)
            && let Some(expanded) = ascii_class_escape(next)
        {
            *idx += 2;
            return ClassAtom::Shorthand(expanded);
        }
        let raw = consume_escape(chars, idx, true);
        let ch = parse_escaped_char(&raw, true);
        ClassAtom::Literal { raw, ch }
    } else {
        *idx += 1;
        ClassAtom::Literal { raw: c.to_string(), ch: Some(c) }
    }
}

fn translate_class(chars: &[char], idx: &mut usize, flags: ReFlags, out: &mut String) {
    out.push('[');
    *idx += 1;
    if chars.get(*idx) == Some(&'^') {
        out.push('^');
        *idx += 1;
    }
    if chars.get(*idx) == Some(&']') {
        out.push(']');
        *idx += 1;
    }
    while *idx < chars.len() {
        if chars[*idx] == ']' {
            out.push(']');
            *idx += 1;
            break;
        }
        let first = next_class_atom(chars, idx, flags);
        match first {
            ClassAtom::Shorthand(expanded) => {
                out.push_str(expanded);
                if chars.get(*idx) == Some(&'-') && chars.get(*idx + 1).is_some_and(|&n| n != ']') {
                    out.push_str(r"\-");
                    *idx += 1;
                }
            }
            ClassAtom::Literal { raw: lo_raw, ch: lo_ch } => {
                if flags.ascii && flags.icase {
                    if let Some(lo) = lo_ch
                        && chars.get(*idx) == Some(&'-')
                        && chars.get(*idx + 1).is_some_and(|&n| n != ']')
                    {
                        let dash_idx = *idx;
                        *idx += 1;
                        if let ClassAtom::Literal { raw: hi_raw, ch: Some(hi) } = next_class_atom(chars, idx, flags) {
                            out.push_str(&lo_raw);
                            out.push('-');
                            out.push_str(&hi_raw);
                            if lo <= hi {
                                let (l0, l1) = (lo.max('a'), hi.min('z'));
                                if l0 <= l1 {
                                    out.push(l0.to_ascii_uppercase());
                                    out.push('-');
                                    out.push(l1.to_ascii_uppercase());
                                }
                                let (u0, u1) = (lo.max('A'), hi.min('Z'));
                                if u0 <= u1 {
                                    out.push(u0.to_ascii_lowercase());
                                    out.push('-');
                                    out.push(u1.to_ascii_lowercase());
                                }
                            }
                            continue;
                        }
                        *idx = dash_idx;
                    }
                    if let Some(ch) = lo_ch.filter(char::is_ascii_alphabetic) {
                        out.push(ch.to_ascii_lowercase());
                        out.push(ch.to_ascii_uppercase());
                        continue;
                    }
                }
                out.push_str(&lo_raw);
            }
        }
    }
}

fn translate_python_ascii(pattern: &str) -> std::borrow::Cow<'_, str> {
    if !pattern.contains("(?") {
        return std::borrow::Cow::Borrowed(pattern);
    }
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::with_capacity(pattern.len());
    let mut stack: Vec<ReFlags> = Vec::new();
    let mut flags = ReFlags::default();
    let mut idx = 0;
    while idx < chars.len() {
        let c = chars[idx];
        if flags.verbose && c == '#' {
            while idx < chars.len() {
                let ch = chars[idx];
                out.push(ch);
                idx += 1;
                if ch == '\n' {
                    break;
                }
            }
            continue;
        }
        if c == '\\' {
            if flags.ascii
                && let Some(&next) = chars.get(idx + 1)
            {
                let replacement = match next {
                    'w' => Some("[0-9A-Za-z_]"),
                    'W' => Some("[^0-9A-Za-z_]"),
                    'd' => Some("[0-9]"),
                    'D' => Some("[^0-9]"),
                    's' => Some(r"[\x09-\x0d\x20]"),
                    'S' => Some(r"[^\x09-\x0d\x20]"),
                    'b' => Some("(?:(?<=[0-9A-Za-z_])(?![0-9A-Za-z_])|(?<![0-9A-Za-z_])(?=[0-9A-Za-z_]))"),
                    'B' => Some("(?:(?<=[0-9A-Za-z_])(?=[0-9A-Za-z_])|(?<![0-9A-Za-z_])(?![0-9A-Za-z_]))"),
                    _ => None,
                };
                if let Some(r) = replacement {
                    out.push_str(r);
                    idx += 2;
                    continue;
                }
            }
            let esc = consume_escape(&chars, &mut idx, false);
            if flags.ascii
                && flags.icase
                && let Some(ch) = parse_escaped_char(&esc, false).filter(char::is_ascii_alphabetic)
            {
                out.push('[');
                out.push(ch.to_ascii_lowercase());
                out.push(ch.to_ascii_uppercase());
                out.push(']');
            } else {
                out.push_str(&esc);
            }
            continue;
        }
        if c == '[' {
            translate_class(&chars, &mut idx, flags, &mut out);
            continue;
        }
        if c == '(' {
            if chars.get(idx + 1) == Some(&'?') {
                if chars.get(idx + 2) == Some(&'#') {
                    while idx < chars.len() {
                        let ch = chars[idx];
                        out.push(ch);
                        idx += 1;
                        if ch == ')' {
                            break;
                        }
                    }
                    continue;
                }
                if let Some(parsed) = parse_inline_flags(&chars[idx + 2..]) {
                    let old = flags;
                    let mut new = flags;
                    for f in parsed.on.chars() {
                        match f {
                            'a' => new.ascii = true,
                            'u' => new.ascii = false,
                            'i' => new.icase = true,
                            'x' => new.verbose = true,
                            _ => {}
                        }
                    }
                    for f in parsed.off.chars() {
                        match f {
                            'i' => new.icase = false,
                            'x' => new.verbose = false,
                            _ => {}
                        }
                    }
                    if !old.ascii && !new.ascii {
                        for &ch in &chars[idx..idx + 2 + parsed.consumed] {
                            out.push(ch);
                        }
                    } else {
                        let mut em_on: String = parsed.on.chars().filter(|&f| !matches!(f, 'a' | 'i')).collect();
                        let mut em_off: String = parsed.off.chars().filter(|&f| f != 'i').collect();
                        if new.engine_icase() && !old.engine_icase() {
                            em_on.push('i');
                        } else if !new.engine_icase() && old.engine_icase() {
                            em_off.push('i');
                        }
                        if parsed.scoped {
                            out.push_str("(?");
                            out.push_str(&em_on);
                            if !em_off.is_empty() {
                                out.push('-');
                                out.push_str(&em_off);
                            }
                            out.push(':');
                        } else if !em_on.is_empty() || !em_off.is_empty() {
                            out.push_str("(?");
                            out.push_str(&em_on);
                            if !em_off.is_empty() {
                                out.push('-');
                                out.push_str(&em_off);
                            }
                            out.push(')');
                        }
                    }
                    if parsed.scoped {
                        stack.push(old);
                    }
                    flags = new;
                    idx += 2 + parsed.consumed;
                    continue;
                }
                if chars.get(idx + 2) == Some(&'P') && chars.get(idx + 3) == Some(&'=') {
                    while idx < chars.len() {
                        let ch = chars[idx];
                        out.push(ch);
                        idx += 1;
                        if ch == ')' {
                            break;
                        }
                    }
                    continue;
                }
                let is_named = (chars.get(idx + 2) == Some(&'P') && chars.get(idx + 3) == Some(&'<'))
                    || (chars.get(idx + 2) == Some(&'<') && !matches!(chars.get(idx + 3), Some('=' | '!')));
                stack.push(flags);
                if is_named {
                    while idx < chars.len() {
                        let ch = chars[idx];
                        out.push(ch);
                        idx += 1;
                        if ch == '>' {
                            break;
                        }
                    }
                } else {
                    out.push('(');
                    out.push('?');
                    idx += 2;
                    if matches!(chars.get(idx), Some('=' | '!' | '>')) {
                        out.push(chars[idx]);
                        idx += 1;
                    } else if chars.get(idx) == Some(&'<') && matches!(chars.get(idx + 1), Some('=' | '!')) {
                        out.push('<');
                        out.push(chars[idx + 1]);
                        idx += 2;
                    }
                }
                continue;
            }
            stack.push(flags);
            out.push('(');
            idx += 1;
            continue;
        }
        if c == ')' {
            if let Some(prev) = stack.pop() {
                flags = prev;
            }
            out.push(')');
            idx += 1;
            continue;
        }
        if flags.ascii && flags.icase && c.is_ascii_alphabetic() {
            out.push('[');
            out.push(c.to_ascii_lowercase());
            out.push(c.to_ascii_uppercase());
            out.push(']');
            idx += 1;
            continue;
        }
        out.push(c);
        idx += 1;
    }
    std::borrow::Cow::Owned(out)
}

pub fn compile(pattern: &str) -> std::result::Result<Regex, String> {
    Regex::new(&translate_python_ascii(pattern)).map_err(|e| e.to_string())
}

pub fn search(pattern: &str, text: &str) -> std::result::Result<bool, String> {
    compile(pattern)?.is_match(text).map_err(|e| e.to_string())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folders {
    pub home: PathBuf,
    pub claude: PathBuf,
    pub codex: PathBuf,
    pub gemini: PathBuf,
    pub config: PathBuf,
}

impl Folders {
    pub fn from_env() -> Result<Folders> {
        let var = |k| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = crate::skills::user_home().context("no home folder: set HOME (USERPROFILE on Windows)")?;
        let mut f = Folders::under(&home, var("XDG_CONFIG_HOME").as_deref());
        if let Some(p) = var("T3WF_CONFIG") {
            f.config = p;
        }
        if let Some(p) = var("CLAUDE_CONFIG_DIR") {
            f.claude = p;
        }
        if let Some(p) = var("CODEX_HOME") {
            f.codex = p;
        }
        if let Some(p) = var("GEMINI_HOME") {
            f.gemini = p;
        }
        Ok(f)
    }

    pub fn under(home: &Path, config_home: Option<&Path>) -> Folders {
        let config = config_home.map(Path::to_path_buf).unwrap_or_else(|| home.join(".config"));
        Folders {
            home: home.to_path_buf(),
            claude: home.join(".claude"),
            codex: home.join(".codex"),
            gemini: home.join(".gemini"),
            config: config.join("t3-workflow").join("config.json"),
        }
    }

    pub fn default_claude(&self) -> PathBuf {
        self.home.join(".claude")
    }

    pub fn default_codex(&self) -> PathBuf {
        self.home.join(".codex")
    }

    pub fn manifest(&self) -> PathBuf {
        self.config.parent().map_or_else(|| PathBuf::from("installed.json"), |d| d.join("installed.json"))
    }
}

fn text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

pub fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => py_str(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => {
            let items: Vec<String> = o.iter().map(|(k, v)| format!("{}: {}", py_str(k), py_repr(v))).collect();
            format!("{{{}}}", items.join(", "))
        }
    }
}

fn py_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::from(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

fn py_list(items: &[String]) -> String {
    py_repr(&json!(items))
}

fn skill_ref_ok(s: &str) -> bool {
    let s = s.strip_suffix('\n').unwrap_or(s);
    let word = |w: &str, extra: &[char]| {
        !w.is_empty()
            && w.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') || extra.contains(&c))
    };
    match s.rsplit_once(':') {
        Some((ns, name)) => word(ns, &['/']) && word(name, &[]),
        None => word(s, &[]),
    }
}

fn skill_ref_problem(at: &str, v: &Value) -> Option<String> {
    match v {
        Value::String(s) if skill_ref_ok(s) => None,
        _ => Some(format!("{at}: {} is not a skill name (bare `name` or `namespace:name`)", py_repr(v))),
    }
}

fn nonempty_str(v: &Value) -> bool {
    v.as_str().is_some_and(|s| !s.is_empty())
}

fn step_id_ok(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else { return false };
    first.is_ascii_alphanumeric() && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn scope_rank(scope: &str) -> Option<usize> {
    DELIVERY_SCOPES.iter().position(|s| *s == scope)
}

fn normalize_rel_path(path: &str) -> String {
    let replaced = path.replace('\\', "/");
    let mut s = replaced.as_str();
    loop {
        if let Some(rest) = s.strip_prefix("./") {
            s = rest;
        } else if let Some(rest) = s.strip_prefix('/') {
            s = rest;
        } else {
            break;
        }
    }
    s.to_string()
}

const MAX_GLOB_BRACE_DEPTH: usize = 16;

fn push_glob_segment(
    chars: &[char],
    idx: &mut usize,
    depth: usize,
    out: &mut String,
) -> std::result::Result<(), String> {
    let in_brace = depth > 0;
    while *idx < chars.len() {
        let c = chars[*idx];
        if in_brace && (c == ',' || c == '}') {
            return Ok(());
        }
        match c {
            '\\' => {
                *idx += 1;
                let Some(&next) = chars.get(*idx) else {
                    return Err("trailing backslash".into());
                };
                out.push_str(&fancy_regex::escape(&next.to_string()));
                *idx += 1;
            }
            '*' => {
                if chars.get(*idx + 1) == Some(&'*') {
                    let start = *idx;
                    while chars.get(*idx) == Some(&'*') {
                        *idx += 1;
                    }
                    let at_start = start == 0
                        || chars.get(start.wrapping_sub(1)) == Some(&'/')
                        || (in_brace && matches!(chars.get(start.wrapping_sub(1)), Some('{' | ',')));
                    if chars.get(*idx) == Some(&'/') && at_start {
                        *idx += 1;
                        out.push_str("(?:[^/]+/)*");
                    } else {
                        out.push_str(".*");
                    }
                } else {
                    *idx += 1;
                    out.push_str("[^/]*");
                }
            }
            '?' => {
                *idx += 1;
                out.push_str("[^/]");
            }
            '[' => {
                *idx += 1;
                let mut negated = false;
                if matches!(chars.get(*idx), Some('!' | '^')) {
                    negated = true;
                    *idx += 1;
                }
                let mut content = String::new();
                if chars.get(*idx) == Some(&']') {
                    content.push_str(r"\]");
                    *idx += 1;
                }
                let mut closed = false;
                while *idx < chars.len() {
                    let ch = chars[*idx];
                    *idx += 1;
                    if ch == ']' {
                        closed = true;
                        break;
                    }
                    if ch == '\\' {
                        let Some(&esc) = chars.get(*idx) else {
                            return Err("unclosed character class '['".into());
                        };
                        *idx += 1;
                        content.push('\\');
                        content.push(esc);
                    } else {
                        content.push(ch);
                    }
                }
                if !closed {
                    return Err("unclosed character class '['".into());
                }
                if content.is_empty() {
                    return Err("empty character class '[]'".into());
                }
                if negated {
                    out.push_str("[^/");
                    out.push_str(&content);
                    out.push(']');
                } else {
                    out.push_str("(?:(?!/)[");
                    out.push_str(&content);
                    out.push_str("])");
                }
            }
            '{' => {
                if depth >= MAX_GLOB_BRACE_DEPTH {
                    return Err(format!("brace nesting exceeds maximum depth ({MAX_GLOB_BRACE_DEPTH})"));
                }
                *idx += 1;
                let mut alts = Vec::new();
                loop {
                    let mut branch = String::new();
                    push_glob_segment(chars, idx, depth + 1, &mut branch)?;
                    alts.push(branch);
                    match chars.get(*idx) {
                        Some(',') => *idx += 1,
                        Some('}') => {
                            *idx += 1;
                            break;
                        }
                        _ => return Err("unclosed brace group '{'".into()),
                    }
                }
                if alts.iter().all(String::is_empty) {
                    return Err("empty brace group '{}'".into());
                }
                out.push_str("(?:");
                out.push_str(&alts.join("|"));
                out.push(')');
            }
            '}' => return Err("unmatched '}'".into()),
            other => {
                *idx += 1;
                out.push_str(&fancy_regex::escape(&other.to_string()));
            }
        }
    }
    Ok(())
}

pub fn compile_glob(pattern: &str) -> std::result::Result<Regex, String> {
    let mut trimmed = pattern.trim();
    loop {
        if let Some(rest) = trimmed.strip_prefix("./") {
            trimmed = rest;
        } else if let Some(rest) = trimmed.strip_prefix('/') {
            trimmed = rest;
        } else {
            break;
        }
    }
    if trimmed.is_empty() {
        return Err("glob pattern must not be empty".into());
    }
    let effective = if trimmed.ends_with('/') { format!("{trimmed}**") } else { trimmed.to_string() };
    let chars: Vec<char> = effective.chars().collect();
    let mut idx = 0;
    let mut regex_src = String::from("^");
    push_glob_segment(&chars, &mut idx, 0, &mut regex_src)?;
    regex_src.push('$');
    Regex::new(&regex_src).map_err(|e| e.to_string())
}

pub fn validate_glob(pattern: &str) -> std::result::Result<(), String> {
    compile_glob(pattern).map(|_| ())
}

pub fn glob_matches(pattern: &str, file: &str) -> std::result::Result<bool, String> {
    let re = compile_glob(pattern)?;
    let normalized = normalize_rel_path(file);
    re.is_match(&normalized).map_err(|e| e.to_string())
}

fn validate_delivery_steps(steps_val: &Value, at: &str, problems: &mut Vec<Option<String>>) {
    let Some(steps) = steps_val.as_array() else {
        problems.push(Some(format!("{at}.delivery.steps: must be a list of step objects")));
        return;
    };
    if steps.is_empty() {
        problems.push(Some(format!("{at}.delivery.steps: must contain at least one step")));
        return;
    }
    let mut seen_ids = HashSet::new();
    let mut saw_builtin_merge = false;
    let mut saw_always_gate = false;
    let mut max_scope_rank = 0usize;
    for (i, step_val) in steps.iter().enumerate() {
        let w = format!("{at}.delivery.steps[{i}]");
        let Some(step) = step_val.as_object() else {
            problems.push(Some(format!("{w}: must be an object")));
            continue;
        };
        let mut extra: Vec<String> =
            step.keys().filter(|k| !DELIVERY_STEP_KEYS.contains(&k.as_str())).cloned().collect();
        if !extra.is_empty() {
            extra.sort();
            problems.push(Some(format!("{w}: unknown keys {}", py_list(&extra))));
        }
        let id = match step.get("id") {
            Some(Value::String(s)) if step_id_ok(s) => {
                if !seen_ids.insert(s.clone()) {
                    problems.push(Some(format!("{w}.id: duplicate step id {}", py_str(s))));
                }
                Some(s.as_str())
            }
            Some(v) => {
                problems.push(Some(format!(
                    "{w}.id: {} must be a non-empty identifier (letters, digits, `-`, `_`, `.`)",
                    py_repr(v)
                )));
                None
            }
            None => {
                problems.push(Some(format!("{w}.id: required")));
                None
            }
        };
        let kind = match step.get("kind") {
            None => None,
            Some(Value::String(k)) if DELIVERY_KINDS.contains(&k.as_str()) => Some(k.as_str()),
            Some(v) => {
                problems.push(Some(format!("{w}.kind: {} is not one of {}", py_repr(v), DELIVERY_KINDS.join(", "))));
                Some("<invalid>")
            }
        };
        match kind {
            Some("builtin") => {
                if id.is_some_and(|s| s != "merge") {
                    problems.push(Some(format!("{w}: kind 'builtin' is only supported for id 'merge'")));
                }
                if saw_builtin_merge {
                    if id != Some("merge") {
                        problems.push(Some(format!("{w}: builtin merge step may appear at most once")));
                    }
                } else if i != 0 {
                    problems.push(Some(format!("{w}: builtin merge step must be first")));
                }
                saw_builtin_merge = true;
                if step.contains_key("skill") {
                    problems.push(Some(format!("{w}: builtin merge step cannot set skill")));
                }
                if step.contains_key("prompt") {
                    problems.push(Some(format!("{w}: builtin merge step cannot set prompt")));
                }
            }
            Some("manual") => {
                if id == Some("merge") {
                    problems.push(Some(format!("{w}: id 'merge' is reserved for kind 'builtin'")));
                }
                if step.contains_key("skill") {
                    problems.push(Some(format!("{w}: manual step cannot set skill")));
                }
                if step.get("prompt").is_none_or(|v| v.as_str().is_none_or(|s| s.trim().is_empty())) {
                    problems.push(Some(format!("{w}.prompt: manual step requires a non-empty prompt string")));
                }
            }
            Some("skill") | None => {
                if id == Some("merge") {
                    problems.push(Some(format!("{w}: id 'merge' is reserved for kind 'builtin'")));
                }
                if step.contains_key("prompt") {
                    problems.push(Some(format!("{w}: skill step cannot set prompt")));
                }
                match step.get("skill") {
                    Some(s) => problems.push(skill_ref_problem(&format!("{w}.skill"), s)),
                    None if kind == Some("skill") => {
                        problems.push(Some(format!("{w}.skill: kind 'skill' requires a skill name")));
                    }
                    None => {
                        problems.push(Some(format!("{w}: must set kind ('builtin' or 'manual') or skill")));
                    }
                }
            }
            Some(_) => {}
        }
        match step.get("gate") {
            Some(Value::String(g)) if DELIVERY_GATES.contains(&g.as_str()) => {
                if g == "always" {
                    saw_always_gate = true;
                } else if g == "chain" && !saw_always_gate {
                    problems.push(Some(format!("{w}.gate: 'chain' requires a preceding step with gate 'always'")));
                }
            }
            Some(v) => {
                problems.push(Some(format!("{w}.gate: {} is not one of {}", py_repr(v), DELIVERY_GATES.join(", "))))
            }
            None => problems.push(Some(format!("{w}.gate: required (one of {})", DELIVERY_GATES.join(", ")))),
        }
        let scope = match step.get("scope") {
            None => Some("pr"),
            Some(Value::String(s)) if DELIVERY_SCOPES.contains(&s.as_str()) => Some(s.as_str()),
            Some(v) => {
                problems.push(Some(format!("{w}.scope: {} is not one of {}", py_repr(v), DELIVERY_SCOPES.join(", "))));
                None
            }
        };
        if let Some(scope) = scope {
            if kind == Some("builtin") && scope != "pr" {
                problems.push(Some(format!("{w}.scope: builtin merge step must have scope 'pr'")));
            }
            if let Some(rank) = scope_rank(scope) {
                if rank < max_scope_rank {
                    problems.push(Some(format!("{w}.scope: {scope:?} cannot follow a broader scope")));
                } else {
                    max_scope_rank = rank;
                }
            }
        }
        if let Some(paths_val) = step.get("paths") {
            match paths_val.as_array() {
                None => problems.push(Some(format!("{w}.paths: must be a list of glob patterns"))),
                Some(a) if a.is_empty() => {
                    problems.push(Some(format!("{w}.paths: must be a non-empty list of glob patterns")))
                }
                Some(a) => {
                    for (j, p) in a.iter().enumerate() {
                        match p.as_str() {
                            Some(s) => {
                                if let Err(e) = validate_glob(s) {
                                    problems.push(Some(format!("{w}.paths[{j}]: invalid glob {}: {e}", py_str(s))));
                                }
                            }
                            None => problems.push(Some(format!("{w}.paths[{j}]: {} must be a string", py_repr(p)))),
                        }
                    }
                }
            }
        }
        if let Some(watch_val) = step.get("watch")
            && !watch_val.is_boolean()
        {
            problems.push(Some(format!("{w}.watch: must be a boolean")));
        }
    }
}

pub fn validate_slots(cfg: &Map<String, Value>, at: &str) -> Vec<String> {
    let mut problems: Vec<Option<String>> = Vec::new();
    if let Some(r) = cfg.get("review").filter(|r| !r.is_null()) {
        problems.push(skill_ref_problem(&format!("{at}.review"), r));
    }
    if let Some(p) = cfg.get("passes") {
        match p.as_array() {
            None => problems.push(Some(format!("{at}.passes: must be a list of skill names"))),
            Some(a) => {
                for (i, v) in a.iter().enumerate() {
                    problems.push(skill_ref_problem(&format!("{at}.passes[{i}]"), v));
                }
            }
        }
    }
    if let Some(c) = cfg.get("credentials") {
        match c.as_array() {
            None => {
                problems.push(Some(format!("{at}.credentials: must be a list of {{\"match\", \"skill\"}} objects")))
            }
            Some(a) => {
                for (i, c) in a.iter().enumerate() {
                    let w = format!("{at}.credentials[{i}]");
                    let keys: HashSet<&str> =
                        c.as_object().map(|o| o.keys().map(String::as_str).collect()).unwrap_or_default();
                    if !c.is_object() || keys != HashSet::from(["match", "skill"]) {
                        problems.push(Some(format!("{w}: needs exactly the keys match and skill")));
                        continue;
                    }
                    problems.push(skill_ref_problem(&format!("{w}.skill"), &c["skill"]));
                    let compiled = match c["match"].as_str() {
                        Some(p) => compile(p),
                        None => Err("first argument must be string or compiled pattern".into()),
                    };
                    if let Err(e) = compiled {
                        problems.push(Some(format!("{w}.match: not a regular expression: {e}")));
                    }
                }
            }
        }
    }
    if let Some(d) = cfg.get("delivery") {
        match d.as_object() {
            None => problems.push(Some(format!(
                "{at}.delivery: must be an object with steps (and optional closeAfter) or mode (and skill for tag-deploy)"
            ))),
            Some(d) => {
                let allowed = ["mode", "skill", "steps", "closeAfter"];
                let mut extra: Vec<String> = d.keys().filter(|k| !allowed.contains(&k.as_str())).cloned().collect();
                if !extra.is_empty() {
                    extra.sort();
                    problems.push(Some(format!("{at}.delivery: unknown keys {}", py_list(&extra))));
                }
                let has_legacy = d.contains_key("mode") || d.contains_key("skill");
                let has_new = d.contains_key("steps") || d.contains_key("closeAfter");
                if has_legacy && has_new {
                    problems.push(Some(format!(
                        "{at}.delivery: cannot mix steps/closeAfter with legacy mode/skill"
                    )));
                }
                if let Some(m) = d.get("mode")
                    && !m.as_str().is_some_and(|m| DELIVERY_MODES.contains(&m))
                {
                    problems.push(Some(format!(
                        "{at}.delivery.mode: {} is not one of {}",
                        py_repr(m),
                        DELIVERY_MODES.join(", ")
                    )));
                }
                if let Some(s) = d.get("skill") {
                    problems.push(skill_ref_problem(&format!("{at}.delivery.skill"), s));
                }
                if let Some(ca) = d.get("closeAfter")
                    && !ca.as_str().is_some_and(|c| DELIVERY_CLOSE_AFTER.contains(&c))
                {
                    problems.push(Some(format!(
                        "{at}.delivery.closeAfter: {} is not one of {}",
                        py_repr(ca),
                        DELIVERY_CLOSE_AFTER.join(", ")
                    )));
                }
                if let Some(steps) = d.get("steps") {
                    validate_delivery_steps(steps, at, &mut problems);
                }
            }
        }
    }
    if let Some(m) = cfg.get("models") {
        match m.as_object() {
            None => problems.push(Some(format!("{at}.models: must be an object keyed by ticket and worker"))),
            Some(m) => {
                for (role, spec) in m {
                    let w = format!("{at}.models.{role}");
                    let obj = spec.as_object();
                    if !MODEL_ROLES.contains(&role.as_str()) {
                        problems.push(Some(format!("{w}: unknown role; roles are {}", MODEL_ROLES.join(", "))));
                    } else if let Some(o) = obj.filter(|o| o.contains_key("pool")) {
                        if o.len() != 1 {
                            problems.push(Some(format!("{w}: a pool sets the model and effort, so pool stands alone")));
                        } else if !nonempty_str(&o["pool"]) {
                            problems.push(Some(format!("{w}.pool: must be a non-empty string")));
                        }
                    } else if obj
                        .is_none_or(|o| o.keys().any(|k| !["model", "effort", "provider"].contains(&k.as_str())))
                    {
                        problems.push(Some(format!(
                            "{w}: must be an object with model, and optionally effort and provider, or with pool alone"
                        )));
                    } else if let Some(o) = obj {
                        for (k, v) in o {
                            if !nonempty_str(v) {
                                problems.push(Some(format!("{w}.{k}: must be a non-empty string")));
                            }
                        }
                    }
                }
            }
        }
    }
    problems.into_iter().flatten().collect()
}

fn version_ok(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() == Some(CONFIG_VERSION as f64),
        _ => false,
    }
}

pub fn validate_config(cfg: &Map<String, Value>, at: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let mut extra: Vec<String> = cfg.keys().filter(|k| !TOP_KEYS.contains(&k.as_str())).cloned().collect();
    if !extra.is_empty() {
        extra.sort();
        problems.push(format!("{at}: unknown keys {}; keys are {}", py_list(&extra), TOP_KEYS.join(", ")));
    }
    if let Some(v) = cfg.get("version").filter(|v| !version_ok(v)) {
        problems.push(format!("{at}.version: {}, expected {CONFIG_VERSION}", py_repr(v)));
    }
    if cfg.get("writtenBy").is_some_and(|v| !v.is_string()) {
        problems.push(format!("{at}.writtenBy: must be a release string"));
    }
    if cfg.get("steps").is_some_and(|v| !v.as_array().is_some_and(|a| a.iter().all(Value::is_string))) {
        problems.push(format!("{at}.steps: must be a list of step ids"));
    }
    problems.extend(validate_slots(cfg, at));
    problems
}

pub fn parse_user_config(path: &Path, raw: &[u8]) -> std::result::Result<Map<String, Value>, String> {
    match serde_json::from_slice::<Value>(raw) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(format!("{}: the top level must be a JSON object", path.display())),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

pub fn load_user_config(path: &Path) -> std::result::Result<Map<String, Value>, String> {
    let raw = match std::fs::read(path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    parse_user_config(path, &raw)
}

fn split_lines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let breaks =
            matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}');
        if !breaks {
            continue;
        }
        out.push(&text[start..i]);
        let mut next = i + c.len_utf8();
        if c == '\r' && chars.peek().is_some_and(|(_, n)| *n == '\n') {
            chars.next();
            next += 1;
        }
        start = next;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

const PY_STRIP: &[char] = &[
    ' ', '\t', '\n', '\r', '\x0b', '\x0c', '\x1c', '\x1d', '\x1e', '\x1f', '\u{85}', '\u{a0}', '\u{2028}', '\u{2029}',
];

fn strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || PY_STRIP.contains(&c))
}

fn first_name(value: &str) -> String {
    if let Some(c) = captures(&TICKED, value) {
        return strip(group(&c, 1)).to_string();
    }
    value.split_whitespace().next().map_or(String::new(), |w| w.trim_matches(['.', ',', ';', '(', ')']).to_string())
}

fn names(value: &str) -> Vec<String> {
    let ticked: Vec<String> =
        TICKED.captures_iter(value).filter_map(|c| c.ok()).map(|c| strip(group(&c, 1)).to_string()).collect();
    if !ticked.is_empty() {
        return ticked;
    }
    value.split(',').map(|w| w.trim_matches([' ', '.'])).filter(|w| !w.is_empty()).map(String::from).collect()
}

fn is_none(value: &str) -> bool {
    matches!(strip(value).trim_matches('`').to_lowercase().as_str(), "none" | "no" | "off" | "-")
}

pub fn agent_workflow_lines(text: &str) -> Vec<(String, String)> {
    let mut lines = Vec::new();
    let mut level: Option<usize> = None;
    for raw in split_lines(text) {
        if let Some(m) = captures(&HEADING, raw) {
            let depth = group(&m, 1).len();
            if level.is_some_and(|l| depth <= l) {
                break;
            }
            if level.is_none() && strip(group(&m, 2)).to_lowercase() == "agent workflow" {
                level = Some(depth);
            }
            continue;
        }
        if level.is_some()
            && let Some(b) = captures(&BULLET, raw)
        {
            lines.push((strip(group(&b, 1)).to_lowercase(), strip(group(&b, 2)).to_string()));
        }
    }
    lines
}

pub fn parse_agent_workflow(text: &str) -> (Map<String, Value>, Vec<String>) {
    let mut cfg = Map::new();
    let mut problems = Vec::new();
    let entry = |cfg: &mut Map<String, Value>, key: &str| -> Map<String, Value> {
        cfg.get(key).and_then(Value::as_object).cloned().unwrap_or_default()
    };
    for (key, value) in agent_workflow_lines(text) {
        match key.as_str() {
            "review" | "independent review" => {
                let v = if is_none(&value) { Value::Null } else { json!(first_name(&value)) };
                cfg.insert("review".into(), v);
            }
            "passes" | "worker passes" => {
                let v = if is_none(&value) { Vec::new() } else { names(&value) };
                cfg.insert("passes".into(), json!(v));
            }
            "credential renewal" | "credentials" => {
                let mut pairs = Vec::new();
                if !is_none(&value) {
                    for part in value.split(';') {
                        let part = strip(part);
                        let sides: Vec<&str> = match ARROW.find(part).ok().flatten() {
                            Some(m) => vec![&part[..m.start()], &part[m.end()..]],
                            None => vec![part],
                        };
                        if sides.len() != 2 {
                            problems.push(format!(
                                "AGENTS.md Credential renewal: {} is not `match` -> `skill`",
                                py_str(part)
                            ));
                            continue;
                        }
                        pairs.push(json!({"match": strip(sides[0]).trim_matches('`'), "skill": first_name(sides[1])}));
                    }
                }
                cfg.insert("credentials".into(), json!(pairs));
            }
            "delivery" => {
                let mut d = entry(&mut cfg, "delivery");
                d.insert("mode".into(), json!(first_name(&value)));
                cfg.insert("delivery".into(), Value::Object(d));
            }
            "deploy skill" => {
                let mut d = entry(&mut cfg, "delivery");
                d.insert("skill".into(), json!(first_name(&value)));
                cfg.insert("delivery".into(), Value::Object(d));
            }
            "ticket model" | "worker model" => {
                let role = key.split_whitespace().next().unwrap_or_default().to_string();
                let mut models = entry(&mut cfg, "models");
                if let Some(p) = captures(&POOL, &value) {
                    models.insert(role, json!({"pool": first_name(group(&p, 1))}));
                } else {
                    let model = first_name(&value);
                    let mut spec = Map::new();
                    spec.insert("model".into(), json!(model));
                    let rest = value.replacen(&model, "", 1);
                    if let Some(e) = captures(&EFFORT_WORD, &rest) {
                        spec.insert("effort".into(), json!(group(&e, 1).to_lowercase()));
                    }
                    models.insert(role, Value::Object(spec));
                }
                cfg.insert("models".into(), Value::Object(models));
            }
            _ => {}
        }
    }
    problems.extend(validate_slots(&cfg, "AGENTS.md").into_iter().map(|p| p.replace("AGENTS.md.", "AGENTS.md ")));
    (cfg, problems)
}

pub fn repo_override(root: &Path) -> (Map<String, Value>, Vec<String>) {
    let path = root.join("AGENTS.md");
    match std::fs::read(&path) {
        Ok(raw) => parse_agent_workflow(&String::from_utf8_lossy(&raw)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Map::new(), Vec::new()),
        Err(e) => (Map::new(), vec![format!("{}: {e}", path.display())]),
    }
}

fn frontmatter_from_bytes(raw: &[u8]) -> HashMap<String, String> {
    let all = String::from_utf8_lossy(raw);
    let head: String = all.chars().take(8192).collect();
    let Some(m) = captures(&FRONTMATTER, &head) else { return HashMap::new() };
    let mut meta = HashMap::new();
    for line in split_lines(group(&m, 1)) {
        if let Some(kv) = captures(&META_LINE, line) {
            meta.insert(group(&kv, 1).to_string(), strip(group(&kv, 2)).trim_matches(['"', '\'']).to_string());
        }
    }
    meta
}

fn frontmatter(path: &Path) -> HashMap<String, String> {
    let Ok(raw) = std::fs::read(path) else { return HashMap::new() };
    frontmatter_from_bytes(&raw)
}

fn is_inline(meta: &HashMap<String, String>) -> bool {
    meta.get("disable-model-invocation").is_some_and(|v| matches!(v.to_lowercase().as_str(), "true" | "yes" | "1"))
}

fn sorted_names(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut names: Vec<String> =
        rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

fn skill_files(base: &Path) -> Vec<PathBuf> {
    sorted_names(base).into_iter().map(|n| base.join(n).join("SKILL.md")).filter(|p| p.is_file()).collect()
}

fn version_key(v: &str) -> Vec<i128> {
    v.split(['.', '+', '-'])
        .map(
            |x| {
                if !x.is_empty() && x.chars().all(|c| c.is_ascii_digit()) { x.parse().unwrap_or(i128::MAX) } else { -1 }
            },
        )
        .collect()
}

fn newest_cached(cache: &Path) -> Vec<(String, String, PathBuf)> {
    let mut roots = Vec::new();
    if !cache.is_dir() {
        return roots;
    }
    for market in sorted_names(cache) {
        let mdir = cache.join(&market);
        if !mdir.is_dir() {
            continue;
        }
        for plugin in sorted_names(&mdir) {
            let pdir = mdir.join(&plugin);
            let versions: Vec<String> = sorted_names(&pdir).into_iter().filter(|v| pdir.join(v).is_dir()).collect();
            let mut best: Option<&String> = None;
            for v in &versions {
                if best.is_none_or(|b| version_key(v) > version_key(b)) {
                    best = Some(v);
                }
            }
            if let Some(b) = best {
                roots.push((plugin.clone(), format!("{plugin}@{market}"), pdir.join(b).join("skills")));
            }
        }
    }
    roots
}

pub fn realpath(p: &Path) -> PathBuf {
    if let Ok(r) = std::fs::canonicalize(p) {
        return r;
    }
    match (p.parent(), p.file_name()) {
        (Some(dir), Some(name)) if !dir.as_os_str().is_empty() => realpath(dir).join(name),
        _ => std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()),
    }
}

#[derive(Clone, Debug, Default)]
pub struct Enabled(Option<HashMap<String, bool>>);

impl Enabled {
    fn from_raw_files(raws: &[Option<&[u8]>]) -> Enabled {
        let mut merged: Option<HashMap<String, bool>> = None;
        for raw in raws.iter().flatten() {
            let Ok(v) = serde_json::from_slice::<Value>(raw) else { continue };
            if let Some(m) = v.get("enabledPlugins").and_then(Value::as_object) {
                let into = merged.get_or_insert_with(HashMap::new);
                for (k, v) in m {
                    into.insert(k.clone(), v == &Value::Bool(true));
                }
            }
        }
        Enabled(merged)
    }

    pub fn load(claude: &Path, root: &Path) -> Enabled {
        let paths = [
            claude.join("settings.json"),
            root.join(".claude").join("settings.json"),
            root.join(".claude").join("settings.local.json"),
        ];
        let bufs: Vec<Option<Vec<u8>>> = paths.iter().map(|p| std::fs::read(p).ok()).collect();
        let refs: Vec<Option<&[u8]>> = bufs.iter().map(|b| b.as_deref()).collect();
        Enabled::from_raw_files(&refs)
    }

    pub fn all() -> Enabled {
        Enabled(None)
    }

    fn allows(&self, key: &str) -> bool {
        self.0.as_ref().is_none_or(|m| m.get(key).copied().unwrap_or(false))
    }

    pub fn disabled(&self) -> Vec<String> {
        let mut out: Vec<String> = self.0.iter().flatten().filter(|(_, on)| !**on).map(|(k, _)| k.clone()).collect();
        out.sort();
        out
    }
}

fn installed_plugin_roots_from_bytes(raw: &[u8], root: &Path) -> Option<Vec<(String, String, PathBuf)>> {
    let v: Value = serde_json::from_slice(raw).ok()?;
    let top = v.as_object()?;
    let Some(installed) = top.get("plugins") else { return Some(Vec::new()) };
    let installed = installed.as_object()?;
    let want = realpath(root);
    let mut roots = Vec::new();
    for (key, entries) in installed {
        let name = key.split('@').next().unwrap_or_default().to_string();
        let list = match entries {
            Value::Array(a) => a.clone(),
            other => vec![other.clone()],
        };
        for e in list {
            let Some(e) = e.as_object() else {
                tracing::warn!("skipping malformed plugin record for {key} in installed_plugins.json");
                continue;
            };
            let project_scoped = matches!(e.get("scope").and_then(Value::as_str), Some("project" | "local"));
            match e.get("projectPath") {
                None | Some(Value::Null) => {
                    if project_scoped {
                        tracing::warn!("skipping project-scoped plugin record for {key} with missing projectPath");
                        continue;
                    }
                }
                Some(Value::String(s)) if s.is_empty() => {
                    if project_scoped {
                        tracing::warn!("skipping project-scoped plugin record for {key} with empty projectPath");
                        continue;
                    }
                }
                Some(Value::String(scoped)) => {
                    if realpath(Path::new(scoped)) != want {
                        continue;
                    }
                }
                Some(_) => {
                    tracing::warn!("skipping plugin record for {key} with non-string projectPath");
                    continue;
                }
            }
            match e.get("installPath") {
                None | Some(Value::Null) => {}
                Some(Value::String(s)) if s.is_empty() => {}
                Some(Value::String(ip)) => roots.push((name.clone(), key.clone(), Path::new(ip).join("skills"))),
                Some(_) => tracing::warn!("skipping plugin record for {key} with non-string installPath"),
            }
        }
    }
    Some(roots)
}

fn installed_plugin_roots(claude: &Path, root: &Path) -> Option<Vec<(String, String, PathBuf)>> {
    let raw = std::fs::read(claude.join("plugins").join("installed_plugins.json")).ok()?;
    installed_plugin_roots_from_bytes(&raw, root)
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn claude_plugin_roots_with(
    claude: &Path,
    root: &Path,
    installed_raw: Option<&[u8]>,
) -> Vec<(String, String, PathBuf)> {
    installed_raw
        .and_then(|raw| installed_plugin_roots_from_bytes(raw, root))
        .unwrap_or_else(|| newest_cached(&claude.join("plugins").join("cache")))
}

fn claude_plugin_roots(claude: &Path, root: &Path) -> Vec<(String, String, PathBuf)> {
    installed_plugin_roots(claude, root).unwrap_or_else(|| newest_cached(&claude.join("plugins").join("cache")))
}

#[derive(Clone, Debug)]
pub struct SearchRoot {
    pub label: String,
    pub namespace: Option<String>,
    pub dir: PathBuf,
    pub inline: bool,
}

fn search_roots_with(f: &Folders, root: &Path, enabled: &Enabled, installed_raw: Option<&[u8]>) -> Vec<SearchRoot> {
    let plain = |label: &str, dir: PathBuf| SearchRoot { label: label.into(), namespace: None, dir, inline: false };
    let mut roots = vec![
        plain("~/.claude/skills", f.claude.join("skills")),
        plain("~/.agents/skills", f.home.join(".agents").join("skills")),
        plain("project .claude/skills", root.join(".claude").join("skills")),
        plain("~/.codex/skills", f.codex.join("skills")),
        SearchRoot {
            label: "~/.gemini/config/skills".into(),
            namespace: None,
            dir: f.gemini.join("config").join("skills"),
            inline: true,
        },
    ];
    let claude =
        claude_plugin_roots_with(&f.claude, root, installed_raw).into_iter().filter(|(_, key, _)| enabled.allows(key));
    let codex = newest_cached(&f.codex.join("plugins").join("cache"));
    for (plugin, _, dir) in claude.chain(codex) {
        roots.push(SearchRoot { label: "plugin cache".into(), namespace: Some(plugin), dir, inline: false });
    }
    roots
}

pub fn search_roots(f: &Folders, root: &Path, enabled: &Enabled) -> Vec<SearchRoot> {
    let plain = |label: &str, dir: PathBuf| SearchRoot { label: label.into(), namespace: None, dir, inline: false };
    let mut roots = vec![
        plain("~/.claude/skills", f.claude.join("skills")),
        plain("~/.agents/skills", f.home.join(".agents").join("skills")),
        plain("project .claude/skills", root.join(".claude").join("skills")),
        plain("~/.codex/skills", f.codex.join("skills")),
        SearchRoot {
            label: "~/.gemini/config/skills".into(),
            namespace: None,
            dir: f.gemini.join("config").join("skills"),
            inline: true,
        },
    ];
    let claude = claude_plugin_roots(&f.claude, root).into_iter().filter(|(_, key, _)| enabled.allows(key));
    let codex = newest_cached(&f.codex.join("plugins").join("cache"));
    for (plugin, _, dir) in claude.chain(codex) {
        roots.push(SearchRoot { label: "plugin cache".into(), namespace: Some(plugin), dir, inline: false });
    }
    roots
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub bare: String,
    pub dir: Option<String>,
    pub path: Option<String>,
    pub inline: bool,
    pub at: String,
}

impl Skill {
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("name".into(), json!(self.name));
        m.insert("bare".into(), json!(self.bare));
        if let Some(d) = &self.dir {
            m.insert("dir".into(), json!(d));
        }
        m.insert("path".into(), json!(self.path));
        m.insert("inline".into(), json!(self.inline));
        m.insert("where".into(), json!(self.at));
        Value::Object(m)
    }
}

pub fn installed_skills(roots: &[SearchRoot]) -> Vec<Skill> {
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    for r in roots {
        for path in skill_files(&r.dir) {
            let meta = frontmatter(&path);
            let dir =
                path.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let bare = meta.get("name").filter(|n| !n.is_empty()).cloned().unwrap_or_else(|| dir.clone());
            let name = match &r.namespace {
                Some(ns) => format!("{ns}:{bare}"),
                None => bare.clone(),
            };
            if !seen.insert((realpath(&path), name.clone())) {
                continue;
            }
            found.push(Skill {
                name,
                bare,
                dir: Some(dir),
                path: Some(text(&path)),
                inline: r.inline || is_inline(&meta),
                at: r.label.clone(),
            });
        }
    }
    found
}

pub fn bare(reference: &str) -> &str {
    reference.rsplit(':').next().unwrap_or(reference)
}

pub struct Resolver {
    listed: Vec<String>,
    disk: Vec<Skill>,
}

impl Resolver {
    pub fn new(roots: &[SearchRoot], listed: &[String]) -> Resolver {
        Resolver::from_disk(installed_skills(roots), listed)
    }

    pub fn from_disk(disk: Vec<Skill>, listed: &[String]) -> Resolver {
        let listed =
            listed.iter().map(|s| s.trim().trim_start_matches('/').to_string()).filter(|s| !s.is_empty()).collect();
        Resolver { listed, disk }
    }

    pub fn candidates(&self, reference: &str) -> Vec<Skill> {
        let want = bare(reference);
        let mut out = Vec::new();
        let mut used = HashSet::new();
        let mut once = HashSet::new();
        for name in self.listed.iter().filter(|n| bare(n) == want) {
            if !once.insert(name.clone()) {
                continue;
            }
            match self.disk.iter().find(|s| &s.name == name) {
                Some(d) => {
                    if let Some(p) = &d.path {
                        used.insert(p.clone());
                    }
                    out.push(Skill { at: "session".into(), ..d.clone() });
                }
                None => out.push(Skill {
                    name: name.clone(),
                    bare: want.to_string(),
                    dir: None,
                    path: None,
                    inline: false,
                    at: "session".into(),
                }),
            }
        }
        for s in &self.disk {
            let hit = s.bare == want || s.dir.as_deref() == Some(want);
            if hit && !s.path.as_ref().is_some_and(|p| used.contains(p)) {
                out.push(s.clone());
            }
        }
        out
    }

    pub fn resolve(&self, reference: &str) -> Value {
        let cands = self.candidates(reference);
        let mut names: Vec<String> = Vec::new();
        for c in &cands {
            if !names.contains(&c.name) {
                names.push(c.name.clone());
            }
        }
        let exact: Vec<&Skill> = cands.iter().filter(|c| c.name == reference).collect();
        let hit: Vec<&Skill> = if !exact.is_empty() {
            exact.clone()
        } else if reference.contains(':') {
            Vec::new()
        } else {
            cands.iter().collect()
        };
        let Some(first) = hit.first() else {
            return json!({"ref": reference, "status": "missing", "others": names});
        };
        let others: Vec<String> = names.iter().filter(|n| **n != first.name).cloned().collect();
        let mut e = json!({
            "ref": reference,
            "name": first.name,
            "path": first.path,
            "where": first.at,
            "invoke": if first.inline { "inline" } else { "skill" },
        });
        if !others.is_empty() && exact.is_empty() {
            e["status"] = json!("ambiguous");
            e["others"] = json!(others);
        } else {
            e["status"] = json!("ok");
        }
        e
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelSlot {
    pub role: String,
    pub spec: Option<Map<String, Value>>,
    pub source: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resolved {
    pub repo: String,
    pub config: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_config: Option<String>,
    pub review: Value,
    pub passes: Value,
    pub credentials: Value,
    pub delivery: Value,
    pub models: Vec<ModelSlot>,
    pub config_problems: Vec<String>,
    pub repo_problems: Vec<String>,
}

pub struct Inputs<'a> {
    pub folders: &'a Folders,
    pub root: &'a Path,
    pub listed: &'a [String],
    pub enabled: &'a Enabled,
}

struct Eff {
    review: (Value, String),
    passes: (Vec<String>, String),
    credentials: (Vec<(String, String)>, String),
    delivery: (Map<String, Value>, String),
    models: BTreeMap<String, (Option<Map<String, Value>>, String)>,
    review_set: bool,
}

fn merge_delivery_layer(prev: &Map<String, Value>, next_layer: &Map<String, Value>) -> Option<Map<String, Value>> {
    if next_layer.is_empty() {
        return None;
    }
    if next_layer.contains_key("steps") {
        return Some(next_layer.clone());
    }
    if let Some(mode_val) = next_layer.get("mode") {
        let mut next = Map::new();
        let mode = mode_val.as_str().unwrap_or_default();
        next.insert("mode".into(), mode_val.clone());
        if let Some(skill) = next_layer.get("skill") {
            next.insert("skill".into(), skill.clone());
        } else if mode == "tag-deploy"
            && !prev.contains_key("steps")
            && let Some(prev_skill) = prev.get("skill")
        {
            next.insert("skill".into(), prev_skill.clone());
        }
        return Some(next);
    }
    if let Some(skill) = next_layer.get("skill") {
        if prev.contains_key("steps") {
            let mut next = prev.clone();
            if let Some(steps) = next.get_mut("steps").and_then(Value::as_array_mut) {
                for st in steps {
                    if st.get("id").and_then(Value::as_str) == Some("deploy") && st.get("skill").is_some() {
                        st["skill"] = skill.clone();
                    }
                }
            }
            return Some(next);
        }
        let mut next = prev.clone();
        next.insert("skill".into(), skill.clone());
        return Some(next);
    }
    if let Some(close_after) = next_layer.get("closeAfter") {
        let mut next = prev.clone();
        next.insert("closeAfter".into(), close_after.clone());
        return Some(next);
    }
    None
}

fn merged(user: &Map<String, Value>, block: &Map<String, Value>, repo_json: &Map<String, Value>) -> Eff {
    let mut eff = Eff {
        review: (Value::Null, "default".into()),
        passes: (Vec::new(), "default".into()),
        credentials: (Vec::new(), "default".into()),
        delivery: ([("mode".to_string(), json!("merge"))].into_iter().collect(), "default".into()),
        models: MODEL_ROLES.iter().map(|r| (r.to_string(), (None, "default".to_string()))).collect(),
        review_set: false,
    };
    for (src, cfg) in [("user", user), ("AGENTS.md", block), (REPO_CONFIG_FILE, repo_json)] {
        if let Some(r) = cfg.get("review") {
            eff.review = (r.clone(), src.into());
            eff.review_set = true;
        }
        if let Some(p) = cfg.get("passes") {
            let refs = p.as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from).collect();
            eff.passes = (refs, src.into());
        }
        if let Some(c) = cfg.get("credentials") {
            let pairs = c
                .as_array()
                .into_iter()
                .flatten()
                .map(|p| {
                    let s = |k: &str| p[k].as_str().unwrap_or_default().to_string();
                    (s("match"), s("skill"))
                })
                .collect();
            eff.credentials = (pairs, src.into());
        }
        if let Some(d) = cfg.get("delivery").and_then(Value::as_object)
            && let Some(next) = merge_delivery_layer(&eff.delivery.0, d)
        {
            eff.delivery = (next, src.into());
        }
        if let Some(m) = cfg.get("models").and_then(Value::as_object) {
            for (role, spec) in m {
                let (Some(slot), Some(spec)) = (eff.models.get_mut(role), spec.as_object()) else { continue };
                let prev = slot.0.clone().unwrap_or_default();
                let same = spec.get("model").filter(|m| !m.is_null()).is_none_or(|m| prev.get("model") == Some(m));
                let mut base =
                    if same && !spec.contains_key("pool") && !prev.contains_key("pool") { prev } else { Map::new() };
                base.extend(spec.clone());
                *slot = (Some(base), src.into());
            }
        }
    }
    eff
}

pub const NO_SKILL_TOOL_DRIVERS: &[&str] = &["antigravity"];

pub fn has_skill_tool(driver: Option<&str>) -> bool {
    !driver.is_some_and(|d| NO_SKILL_TOOL_DRIVERS.contains(&d))
}

pub fn in_skill_tool_harness() -> bool {
    std::env::var_os("ANTIGRAVITY_AGENT").is_none_or(|v| v.is_empty())
}

fn inline_paths(v: &mut Value) {
    match v {
        Value::Object(m) => {
            if m.get("invoke").and_then(Value::as_str) == Some("skill") && m.get("path").is_some_and(Value::is_string) {
                m.insert("invoke".into(), json!("inline"));
            }
            m.values_mut().for_each(inline_paths);
        }
        Value::Array(a) => a.iter_mut().for_each(inline_paths),
        _ => {}
    }
}

fn builtin_merge_step() -> Value {
    json!({"id": "merge", "kind": "builtin", "gate": "always", "scope": "pr"})
}

fn is_default_merge_step(step: &Value) -> bool {
    step.get("id").and_then(Value::as_str) == Some("merge")
        && step.get("kind").and_then(Value::as_str) == Some("builtin")
        && step.get("gate").and_then(Value::as_str) == Some("always")
        && step.get("scope").and_then(Value::as_str) == Some("pr")
        && step.get("paths").is_none()
        && step.get("watch").and_then(Value::as_bool) != Some(true)
}

fn apply_legacy_delivery_compat(delivery: &mut Value) {
    if delivery.get("problem").is_some() {
        return;
    }
    let close_after = delivery["closeAfter"].as_str().unwrap_or(DEFAULT_CLOSE_AFTER).to_string();
    let steps = delivery["steps"].as_array().cloned();
    let Some(o) = delivery.as_object_mut() else { return };
    o.remove("mode");
    o.remove("skill");
    if close_after != DEFAULT_CLOSE_AFTER {
        return;
    }
    let Some(steps) = steps else { return };
    if steps.len() == 1 && is_default_merge_step(&steps[0]) {
        o.insert("mode".into(), json!("merge"));
    } else if steps.len() == 2
        && is_default_merge_step(&steps[0])
        && steps[1].get("id").and_then(Value::as_str) == Some("deploy")
        && steps[1].get("kind").and_then(Value::as_str) == Some("skill")
        && steps[1].get("gate").and_then(Value::as_str) == Some("chain")
        && steps[1].get("scope").and_then(Value::as_str) == Some("pr")
        && steps[1].get("watch").and_then(Value::as_bool) == Some(true)
        && steps[1].get("paths").is_none()
    {
        o.insert("mode".into(), json!("tag-deploy"));
        o.insert("skill".into(), steps[1]["skill"].clone());
    }
}

fn resolve_step(raw: &Map<String, Value>, resolver: &Resolver) -> Value {
    let id = raw.get("id").and_then(Value::as_str).unwrap_or_default();
    let kind = raw
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_else(|| if raw.contains_key("skill") { "skill" } else { "builtin" });
    let gate = raw.get("gate").and_then(Value::as_str).unwrap_or("always");
    let scope = raw.get("scope").and_then(Value::as_str).unwrap_or("pr");
    let mut step = Map::new();
    step.insert("id".into(), json!(id));
    step.insert("kind".into(), json!(kind));
    if kind == "skill" {
        let skill_ref = raw.get("skill").and_then(Value::as_str).unwrap_or_default();
        step.insert("skill".into(), resolver.resolve(skill_ref));
    } else if kind == "manual"
        && let Some(prompt) = raw.get("prompt").and_then(Value::as_str)
    {
        step.insert("prompt".into(), json!(prompt));
    }
    step.insert("gate".into(), json!(gate));
    step.insert("scope".into(), json!(scope));
    if let Some(paths) = raw.get("paths").and_then(Value::as_array) {
        step.insert("paths".into(), Value::Array(paths.clone()));
    }
    if raw.get("watch").and_then(Value::as_bool) == Some(true) {
        step.insert("watch".into(), json!(true));
    }
    Value::Object(step)
}

fn resolve_delivery(d: &Map<String, Value>, source: &str, resolver: &Resolver) -> Value {
    let close_after = d
        .get("closeAfter")
        .and_then(Value::as_str)
        .filter(|c| DELIVERY_CLOSE_AFTER.contains(c))
        .unwrap_or(DEFAULT_CLOSE_AFTER);
    if let Some(raw_steps) = d.get("steps").and_then(Value::as_array) {
        let steps: Vec<Value> =
            raw_steps.iter().filter_map(Value::as_object).map(|st| resolve_step(st, resolver)).collect();
        let mut delivery = json!({
            "slot": "delivery",
            "source": source,
            "steps": steps,
            "closeAfter": close_after,
        });
        apply_legacy_delivery_compat(&mut delivery);
        return delivery;
    }
    let mode = d.get("mode").and_then(Value::as_str).unwrap_or("merge");
    if mode == "tag-deploy" {
        match d.get("skill").filter(|s| truthy(s)).and_then(Value::as_str) {
            Some(skill_ref) => {
                let resolved_skill = resolver.resolve(skill_ref);
                let steps = vec![
                    builtin_merge_step(),
                    json!({
                        "id": "deploy",
                        "kind": "skill",
                        "skill": resolved_skill,
                        "gate": "chain",
                        "scope": "pr",
                        "watch": true,
                    }),
                ];
                let mut delivery = json!({
                    "slot": "delivery",
                    "source": source,
                    "steps": steps,
                    "closeAfter": close_after,
                });
                apply_legacy_delivery_compat(&mut delivery);
                delivery
            }
            None => json!({
                "slot": "delivery",
                "source": source,
                "steps": [builtin_merge_step()],
                "closeAfter": close_after,
                "mode": "tag-deploy",
                "skill": Value::Null,
                "problem": "tag-deploy names no deploy skill",
            }),
        }
    } else {
        let mut delivery = json!({
            "slot": "delivery",
            "source": source,
            "steps": [builtin_merge_step()],
            "closeAfter": close_after,
        });
        apply_legacy_delivery_compat(&mut delivery);
        delivery
    }
}

pub fn filter_delivery_steps(delivery: &mut Value, files: &[String]) {
    let Some(steps) = delivery.get("steps").and_then(Value::as_array).cloned() else { return };
    let mut kept = Vec::new();
    let mut skipped = Vec::new();
    for step in steps {
        let Some(paths) = step.get("paths").and_then(Value::as_array) else {
            kept.push(step);
            continue;
        };
        let globs: Vec<&str> = paths.iter().filter_map(Value::as_str).collect();
        let matched = files.iter().any(|f| globs.iter().any(|g| glob_matches(g, f).unwrap_or(false)));
        if matched {
            kept.push(step);
        } else {
            skipped.push(json!({
                "id": step["id"],
                "reason": "no files matched paths",
                "paths": globs,
            }));
        }
    }
    delivery["steps"] = Value::Array(kept);
    delivery["skipped"] = Value::Array(skipped);
    apply_legacy_delivery_compat(delivery);
}

impl Resolved {
    pub fn build(i: &Inputs) -> Resolved {
        Snapshot::capture_with(i.folders, i.root, i.listed, Some(i.enabled)).build()
    }

    pub fn build_user_only(folders: &Folders) -> Resolved {
        let config_raw = read_optional(&folders.config);
        let config_exists = !matches!(config_raw, Ok(None));
        let snap = Snapshot {
            fingerprint: String::new(),
            folders: folders.clone(),
            root: PathBuf::new(),
            listed: Vec::new(),
            config_exists,
            config_raw,
            agents_md_raw: Ok(None),
            repo_config_exists: false,
            repo_config_raw: Ok(None),
            skills: Vec::new(),
        };
        Resolved::build_from_snapshot(&snap)
    }

    fn build_from_snapshot(s: &Snapshot) -> Resolved {
        let config_file = &s.folders.config;
        let mut config_problems = Vec::new();
        let mut user = match &s.config_raw {
            Ok(Some(raw)) => match parse_user_config(config_file, raw) {
                Ok(c) => c,
                Err(e) => {
                    config_problems.push(e);
                    Map::new()
                }
            },
            Ok(None) => Map::new(),
            Err(e) => {
                config_problems.push(e.clone());
                Map::new()
            }
        };
        if !user.is_empty() {
            let base = config_file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            config_problems.extend(validate_config(&user, &base));
        }
        if !config_problems.is_empty() {
            user = Map::new();
        }
        let (block, mut repo_problems) = match &s.agents_md_raw {
            Ok(Some(raw)) => parse_agent_workflow(&String::from_utf8_lossy(raw)),
            Ok(None) => (Map::new(), Vec::new()),
            Err(e) => (Map::new(), vec![e.clone()]),
        };
        let repo_config_path = s.root.join(REPO_CONFIG_FILE);
        let mut repo_json_problems = Vec::new();
        let mut repo_json = match &s.repo_config_raw {
            Ok(Some(raw)) => match parse_user_config(&repo_config_path, raw) {
                Ok(c) => c,
                Err(e) => {
                    repo_json_problems.push(e);
                    Map::new()
                }
            },
            Ok(None) => Map::new(),
            Err(e) => {
                repo_json_problems.push(e.clone());
                Map::new()
            }
        };
        if !repo_json.is_empty() {
            repo_json_problems.extend(validate_config(&repo_json, REPO_CONFIG_FILE));
        }
        if !repo_json_problems.is_empty() {
            repo_json = Map::new();
        }
        repo_problems.extend(repo_json_problems);
        let eff = merged(&user, &block, &repo_json);
        let resolver = Resolver::from_disk(s.skills.clone(), &s.listed);
        let review = if !eff.review_set {
            let hit = resolver.resolve(DEFAULT_REVIEW);
            if hit["status"] == "missing" {
                json!({"slot": "review", "source": "default", "skill": null,
                       "note": format!("no reviewer installed; suggest {REVIEW_SUGGESTION}")})
            } else {
                json!({"slot": "review", "source": "default", "skill": hit})
            }
        } else {
            match eff.review.0.as_str() {
                None => json!({"slot": "review", "source": eff.review.1, "skill": null, "note": "none configured"}),
                Some(r) => json!({"slot": "review", "source": eff.review.1, "skill": resolver.resolve(r)}),
            }
        };
        let passes = json!({"slot": "passes", "source": eff.passes.1,
                            "skills": eff.passes.0.iter().map(|r| resolver.resolve(r)).collect::<Vec<_>>()});
        let renewals: Vec<Value> =
            eff.credentials.0.iter().map(|(m, s)| json!({"match": m, "skill": resolver.resolve(s)})).collect();
        let credentials = json!({"slot": "credentials", "source": eff.credentials.1, "renewals": renewals});
        let delivery = resolve_delivery(&eff.delivery.0, &eff.delivery.1, &resolver);
        let mut models: Vec<ModelSlot> =
            eff.models.into_iter().map(|(role, (spec, source))| ModelSlot { role, spec, source }).collect();
        models.sort_by_key(|m| MODEL_ROLES.iter().position(|r| *r == m.role));
        Resolved {
            repo: text(&s.root),
            config: s.config_exists.then(|| text(config_file)),
            repo_config: s.repo_config_exists.then(|| text(&repo_config_path)),
            review,
            passes,
            credentials,
            delivery,
            models,
            config_problems,
            repo_problems,
        }
    }

    pub fn without_skill_tool(mut self) -> Resolved {
        for v in [&mut self.review, &mut self.passes, &mut self.credentials, &mut self.delivery] {
            inline_paths(v);
        }
        self
    }

    pub fn problems(&self) -> Vec<String> {
        self.config_problems.iter().chain(&self.repo_problems).cloned().collect()
    }

    pub fn model_searches(&self) -> Vec<String> {
        let mut out = Vec::new();
        for m in &self.models {
            let search = match &m.spec {
                None => Some(DEFAULT_MODEL.to_string()),
                Some(s) if s.contains_key("pool") => None,
                Some(s) => s.get("model").and_then(Value::as_str).map(String::from),
            };
            if let Some(s) = search.filter(|s| !out.contains(s)) {
                out.push(s);
            }
        }
        out
    }

    pub fn models_json(&self, caps: &Caps) -> Value {
        let mut out = Map::new();
        for m in &self.models {
            out.insert(m.role.clone(), model_entry(m, caps));
        }
        Value::Object(out)
    }

    pub fn all(&self, caps: &Caps) -> Value {
        let mut out = json!({
            "repo": self.repo,
            "config": self.config,
            "review": self.review,
            "passes": self.passes,
            "credentials": self.credentials,
            "delivery": self.delivery,
            "models": self.models_json(caps),
        });
        if let Some(rc) = &self.repo_config {
            out["repoConfig"] = json!(rc);
        }
        out
    }
}

#[derive(Clone, Debug)]
pub enum Caps {
    Unchecked,
    Failed(String),
    Got(Value),
}

impl Caps {
    fn get(&self) -> Option<&Value> {
        match self {
            Caps::Got(v) => Some(v),
            _ => None,
        }
    }

    fn why(&self) -> String {
        match self {
            Caps::Failed(e) => format!("T3 unreachable ({e})"),
            _ => "T3 not checked".into(),
        }
    }
}

fn disabled(p: &Value) -> bool {
    p["constraints"].as_array().is_some_and(|c| {
        c.iter().any(|x| {
            let s = match x {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            s.to_lowercase().contains("disabled")
        })
    })
}

pub fn model_offers(caps: &Value, model: &str, provider: Option<&str>) -> Vec<(String, Value)> {
    let mut offers: Vec<(String, Value)> = Vec::new();
    for p in caps["providers"].as_array().into_iter().flatten() {
        if disabled(p) {
            continue;
        }
        let pid = p["providerInstanceId"].as_str().unwrap_or_default().to_string();
        if let Some(want) = provider
            && want != pid
            && p["driverKind"].as_str() != Some(want)
        {
            continue;
        }
        for m in p["models"].as_array().into_iter().flatten() {
            if m["id"].as_str() == Some(model) {
                match offers.iter_mut().find(|(k, _)| *k == pid) {
                    Some(slot) => slot.1 = m.clone(),
                    None => offers.push((pid.clone(), m.clone())),
                }
            }
        }
    }
    offers
}

pub fn merge_caps(base: &mut Value, extra: &Value) {
    let Some(extra_providers) = extra["providers"].as_array() else { return };
    if !base["providers"].is_array() {
        base["providers"] = json!([]);
    }
    let Some(base_providers) = base["providers"].as_array_mut() else { return };
    for ep in extra_providers {
        let Some(pid) = ep["providerInstanceId"].as_str() else { continue };
        match base_providers.iter_mut().find(|bp| bp["providerInstanceId"].as_str() == Some(pid)) {
            Some(bp) => {
                let extra_models = ep["models"].as_array().into_iter().flatten();
                if !bp["models"].is_array() {
                    bp["models"] = json!([]);
                }
                if let Some(bm) = bp["models"].as_array_mut() {
                    for em in extra_models {
                        let id = em["id"].as_str();
                        if !bm.iter().any(|m| m["id"].as_str() == id) {
                            bm.push(em.clone());
                        }
                    }
                }
            }
            None => base_providers.push(ep.clone()),
        }
    }
}

fn effort_values(entry: &Value) -> Option<Vec<String>> {
    for o in entry["options"].as_array().into_iter().flatten() {
        if o["id"].as_str().is_some_and(|i| EFFORT_OPTIONS.contains(&i)) {
            return Some(
                o["values"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string()))
                    .collect(),
            );
        }
    }
    None
}

pub fn check_model(spec: &Map<String, Value>, caps: &Value) -> Option<String> {
    let model = spec.get("model").and_then(Value::as_str).unwrap_or_default();
    let provider = spec.get("provider").and_then(Value::as_str);
    let offers = model_offers(caps, model, provider);
    let on = provider.map_or(String::new(), |p| format!(" on {p}"));
    if offers.is_empty() {
        return Some(format!("T3 offers no enabled provider with model {model}{on}"));
    }
    if let Some(effort) = spec.get("effort").and_then(Value::as_str).filter(|e| !e.is_empty()) {
        let allowed: Vec<Option<Vec<String>>> = offers.iter().map(|(_, m)| effort_values(m)).collect();
        if allowed.iter().all(|a| a.as_ref().is_some_and(|a| !a.iter().any(|v| v == effort))) {
            let first = allowed[0].clone().unwrap_or_default().join(", ");
            return Some(format!("effort {effort} is not offered for {model}{on} ({first})"));
        }
    }
    None
}

pub fn default_model_spec() -> Map<String, Value> {
    [
        ("provider".to_string(), json!(DEFAULT_PROVIDER)),
        ("model".to_string(), json!(DEFAULT_MODEL)),
        ("effort".to_string(), json!(DEFAULT_EFFORT)),
    ]
    .into_iter()
    .collect()
}

pub fn check_default_model(caps: &Value) -> Option<String> {
    let offers = model_offers(caps, DEFAULT_MODEL, Some(DEFAULT_PROVIDER));
    if offers.is_empty() {
        return Some(format!("T3 offers no enabled provider with model {DEFAULT_MODEL} on {DEFAULT_PROVIDER}"));
    }
    let allowed: Vec<Option<Vec<String>>> = offers.iter().map(|(_, m)| effort_values(m)).collect();
    if allowed.iter().any(|a| a.as_ref().is_some_and(|v| v.iter().any(|x| x == DEFAULT_EFFORT))) {
        return None;
    }
    match allowed.iter().flatten().next() {
        Some(first) => Some(format!(
            "effort {DEFAULT_EFFORT} is not offered for {DEFAULT_MODEL} on {DEFAULT_PROVIDER} ({})",
            first.join(", ")
        )),
        None => Some(format!(
            "effort {DEFAULT_EFFORT} is not offered for {DEFAULT_MODEL} on {DEFAULT_PROVIDER} (no effort options)"
        )),
    }
}

pub fn model_needs_search(spec: &Map<String, Value>, caps: &Value) -> bool {
    check_model(spec, caps).is_some()
}

pub fn default_model_needs_search(caps: &Value) -> bool {
    check_default_model(caps).is_some()
}

fn model_entry(m: &ModelSlot, caps: &Caps) -> Value {
    let slot = format!("model.{}", m.role);
    let Some(spec) = &m.spec else {
        let mut out = json!({"slot": slot, "source": "default", "spec": Value::Null, "note": Value::Null});
        match caps.get() {
            None => out["note"] = Value::String(format!("{}, so Claude is unconfirmed", caps.why())),
            Some(c) => {
                if model_offers(c, DEFAULT_MODEL, Some(DEFAULT_PROVIDER)).is_empty() {
                    out["note"] = json!("T3 offers no Claude");
                } else {
                    match check_default_model(c) {
                        None => out["spec"] = Value::Object(default_model_spec()),
                        Some(problem) => out["problem"] = json!(problem),
                    }
                }
            }
        }
        return out;
    };
    let mut out = json!({"slot": slot, "source": m.source, "spec": spec});
    if spec.contains_key("pool") {
        out["note"] = json!(POOL_NOTE);
        return out;
    }
    if !spec.contains_key("model") {
        out["problem"] = json!("names no model");
        return out;
    }
    match caps.get() {
        None => out["note"] = json!(format!("unverified: {}", caps.why())),
        Some(c) => {
            if let Some(p) = check_model(spec, c) {
                out["problem"] = json!(p);
            }
        }
    }
    out
}

pub fn skill_text(e: &Value) -> String {
    if e.is_null() {
        return "none".into();
    }
    let reference = e["ref"].as_str().unwrap_or_default();
    if e["status"] == "missing" {
        let others = strings(&e["others"]);
        let alt = if others.is_empty() { String::new() } else { format!(" (installed: {})", others.join(", ")) };
        return format!("SKIP {reference}: no longer resolves{alt}");
    }
    let how = if e["invoke"] == "inline" {
        format!("inline: read {} and apply it", e["path"].as_str().unwrap_or_default())
    } else {
        "Skill tool".into()
    };
    let mut t = format!("{} ({how})", e["name"].as_str().unwrap_or_default());
    if e["status"] == "ambiguous" {
        t.push_str(&format!(" [also: {}]", strings(&e["others"]).join(", ")));
    }
    t
}

pub fn skill_how(e: &Value) -> String {
    if e["status"] == "missing" {
        return skill_text(e).replacen("SKIP ", "skip: ", 1);
    }
    if e["invoke"] == "inline" {
        format!("inline: read {} and apply it", e["path"].as_str().unwrap_or_default())
    } else {
        "Skill tool".into()
    }
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from).collect()
}

fn model_text(m: &Value) -> String {
    let s = &m["spec"];
    let mut t = if s.is_null() {
        "provider default".to_string()
    } else if let Some(p) = s.get("pool") {
        format!("pool {}", p.as_str().unwrap_or_default())
    } else {
        let mut t = s["model"].as_str().unwrap_or("?").to_string();
        if let Some(e) = s["effort"].as_str().filter(|e| !e.is_empty()) {
            t.push_str(&format!(" effort {e}"));
        }
        if let Some(p) = s["provider"].as_str().filter(|p| !p.is_empty()) {
            t.push_str(&format!(" on {p}"));
        }
        t
    };
    if let Some(p) = m["problem"].as_str().filter(|p| !p.is_empty()) {
        t.push_str(&format!(" - PROBLEM: {p}"));
    } else if let Some(n) = m["note"].as_str().filter(|n| !n.is_empty()) {
        t.push_str(&format!(" ({n})"));
    }
    t
}

fn step_text(step: &Value) -> String {
    let id = step["id"].as_str().unwrap_or("?");
    let kind = step["kind"].as_str().unwrap_or("skill");
    let gate = step["gate"].as_str().unwrap_or("always");
    let scope = step["scope"].as_str().unwrap_or("pr");
    let watch = step["watch"].as_bool().unwrap_or(false);
    let paths = strings(&step["paths"]);
    let mut head = match kind {
        "builtin" => id.to_string(),
        "manual" => format!("{id} (manual)"),
        _ => format!("{id} via {}", skill_text(&step["skill"])),
    };
    let mut meta = Vec::new();
    if !(kind == "builtin" && gate == "always" && scope == "pr" && !watch && paths.is_empty()) {
        meta.push(format!("{gate}/{scope}"));
        if watch {
            meta.push("watch".into());
        }
        if !paths.is_empty() {
            meta.push(format!("paths: {}", paths.join(", ")));
        }
    }
    if !meta.is_empty() {
        head.push_str(&format!(" ({})", meta.join(", ")));
    }
    head
}

pub fn delivery_line(d: &Value) -> String {
    let src = d["source"].as_str().unwrap_or_default();
    if let Some(p) = d["problem"].as_str() {
        let mode = d["mode"].as_str().unwrap_or("tag-deploy");
        return format!("delivery: {mode} via ? - PROBLEM: {p} [{src}]");
    }
    let steps = d["steps"].as_array();
    let mut text = match steps {
        Some(list) if !list.is_empty() => list.iter().map(step_text).collect::<Vec<_>>().join(" -> "),
        _ => "none".into(),
    };
    let close_after = d["closeAfter"].as_str().unwrap_or(DEFAULT_CLOSE_AFTER);
    if close_after != DEFAULT_CLOSE_AFTER {
        text.push_str(&format!("; closeAfter {close_after}"));
    }
    let skipped: Vec<&str> = d["skipped"].as_array().into_iter().flatten().filter_map(|s| s["id"].as_str()).collect();
    if !skipped.is_empty() {
        text.push_str(&format!("; skipped: {}", skipped.join(", ")));
    }
    format!("delivery: {text} [{src}]")
}

pub fn slot_line(eff: &Value, slot: &str) -> Vec<String> {
    let src = |v: &Value| v["source"].as_str().unwrap_or_default().to_string();
    match slot {
        "review" => {
            let r = &eff["review"];
            let note = r["note"].as_str().map_or(String::new(), |n| format!(" ({n})"));
            vec![format!("review: {}{note} [{}]", skill_text(&r["skill"]), src(r))]
        }
        "passes" => {
            let p = &eff["passes"];
            let list: Vec<String> = p["skills"].as_array().into_iter().flatten().map(skill_text).collect();
            let text = if list.is_empty() { "none".into() } else { list.join("; ") };
            vec![format!("passes: {text} [{}]", src(p))]
        }
        "credentials" => {
            let c = &eff["credentials"];
            let list: Vec<String> = c["renewals"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|x| format!("/{}/ -> {}", x["match"].as_str().unwrap_or_default(), skill_text(&x["skill"])))
                .collect();
            let text = if list.is_empty() { "none".into() } else { list.join("; ") };
            vec![format!("credentials: {text} [{}]", src(c))]
        }
        "delivery" => vec![delivery_line(&eff["delivery"])],
        "models" => eff["models"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(role, m)| format!("model {role}: {} [{}]", model_text(m), src(m)))
            .collect(),
        _ => Vec::new(),
    }
}

pub fn report_lines(eff: &Value) -> Vec<String> {
    let config = eff["config"].as_str().unwrap_or("none (defaults)");
    let mut header = format!("repo: {}; config: {config}", eff["repo"].as_str().unwrap_or_default());
    if let Some(rc) = eff["repoConfig"].as_str() {
        header.push_str(&format!("; repo config: {rc}"));
    }
    let mut lines = vec![header];
    for slot in SLOTS {
        lines.extend(slot_line(eff, slot));
    }
    lines
}

pub fn configured_skills(eff: &Value) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    if !eff["review"]["skill"].is_null() && eff["review"]["source"] != "default" {
        out.push(("review".to_string(), eff["review"]["skill"].clone()));
    }
    for (i, s) in eff["passes"]["skills"].as_array().into_iter().flatten().enumerate() {
        out.push((format!("passes[{i}]"), s.clone()));
    }
    for x in eff["credentials"]["renewals"].as_array().into_iter().flatten() {
        out.push((format!("credentials /{}/", x["match"].as_str().unwrap_or_default()), x["skill"].clone()));
    }
    if let Some(steps) = eff["delivery"]["steps"].as_array() {
        for (i, step) in steps.iter().enumerate() {
            if step["skill"].is_object() {
                let label = match step["id"].as_str().filter(|id| !id.is_empty()) {
                    Some(id) => format!("delivery.{id}"),
                    None => format!("delivery.steps[{i}]"),
                };
                out.push((label, step["skill"].clone()));
            }
        }
    } else if truthy(&eff["delivery"]["skill"]) {
        out.push(("delivery".to_string(), eff["delivery"]["skill"].clone()));
    }
    out
}

pub fn skill_problems(eff: &Value) -> (Vec<String>, Vec<String>) {
    let (mut problems, mut warnings) = (Vec::new(), Vec::new());
    for (at, e) in configured_skills(eff) {
        let reference = e["ref"].as_str().unwrap_or_default();
        if e["status"] == "missing" {
            let others = strings(&e["others"]);
            let alt = if others.is_empty() { String::new() } else { format!("; installed: {}", others.join(", ")) };
            problems.push(format!("{at}: skill {reference} no longer resolves{alt}"));
        } else if e["status"] == "ambiguous" {
            warnings.push(format!(
                "{at}: {reference} matches {} and {}; record the full name",
                e["name"].as_str().unwrap_or_default(),
                strings(&e["others"]).join(", ")
            ));
        }
    }
    (problems, warnings)
}

pub fn doctor(r: &Resolved, caps: &Caps) -> (Vec<String>, Vec<String>, Value) {
    let eff = r.all(caps);
    let (problems, warnings) = doctor_eff(r.problems(), &eff);
    (problems, warnings, eff)
}

pub fn doctor_eff(mut problems: Vec<String>, eff: &Value) -> (Vec<String>, Vec<String>) {
    let (skill_problems, mut warnings) = skill_problems(eff);
    problems.extend(skill_problems);
    if let Some(p) = eff["delivery"]["problem"].as_str() {
        problems.push(format!("delivery: {p}"));
    }
    for (role, m) in eff["models"].as_object().into_iter().flatten() {
        if let Some(p) = m["problem"].as_str() {
            problems.push(format!("model {role}: {p}"));
        } else if m["source"] != "default"
            && let Some(n) = m["note"].as_str().filter(|n| !n.is_empty())
            && m["spec"].get("pool").is_none()
        {
            warnings.push(format!("model {role}: {n}"));
        }
    }
    if eff["review"]["skill"].is_null() && eff["review"]["source"] == "default" {
        warnings.push(format!("review: {}", eff["review"]["note"].as_str().unwrap_or_default()));
    }
    (problems, warnings)
}

pub fn renewal(r: &Resolved, output: &str) -> (Value, Vec<String>) {
    let mut warnings = Vec::new();
    for x in r.credentials["renewals"].as_array().into_iter().flatten() {
        let pattern = x["match"].as_str().unwrap_or_default();
        match search(pattern, output) {
            Ok(false) => {}
            Ok(true) => {
                let skill = &x["skill"];
                let skipped = skill["status"] == "missing";
                let mut hit = json!({"match": pattern, "skill": skill, "how": skill_how(skill), "skipped": skipped});
                if skipped {
                    hit["reason"] = json!(format!(
                        "the renewal skill {} no longer resolves, so carry on as if no renewal were configured",
                        skill["ref"].as_str().unwrap_or_default()
                    ));
                }
                return (hit, warnings);
            }
            Err(e) => warnings.push(format!("credentials /{pattern}/: t3cm cannot compile it ({e}); skipped")),
        }
    }
    (Value::Null, warnings)
}

fn stamp_bytes(h: &mut Sha256, p: &Path, read: &std::result::Result<Option<Vec<u8>>, String>) {
    h.update(text(p).as_bytes());
    match read {
        Ok(Some(raw)) => {
            h.update(b"|");
            h.update(Sha256::digest(raw));
            h.update(b"\n");
        }
        Ok(None) => h.update(b"|missing\n"),
        Err(e) => {
            h.update(b"|err:");
            h.update(e.as_bytes());
            h.update(b"\n");
        }
    }
}

fn read_optional(p: &Path) -> std::result::Result<Option<Vec<u8>>, String> {
    match std::fs::read(p) {
        Ok(raw) => Ok(Some(raw)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", p.display())),
    }
}

fn stamp_root(h: &mut Sha256, r: &SearchRoot) {
    h.update(r.label.as_bytes());
    h.update(b"|");
    h.update(r.namespace.as_deref().unwrap_or_default().as_bytes());
    h.update(if r.inline { "|inline|" } else { "|skill|" }.as_bytes());
    stamp(h, &r.dir);
}

fn stamp(h: &mut Sha256, p: &Path) {
    h.update(text(p).as_bytes());
    h.update(text(&realpath(p)).as_bytes());
    match std::fs::metadata(p) {
        Ok(m) => {
            let mtime = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
            h.update(format!("|{}|{}|{:?}\n", m.is_dir(), m.len(), mtime).as_bytes());
        }
        Err(_) => h.update(b"|missing\n"),
    }
}

fn read_skill_file(p: &Path) -> Option<(std::fs::Metadata, Vec<u8>)> {
    use std::io::Read;
    let mut file = std::fs::File::open(p).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    let mut raw = Vec::new();
    file.read_to_end(&mut raw).ok()?;
    Some((meta, raw))
}

pub struct Snapshot {
    pub fingerprint: String,
    folders: Folders,
    root: PathBuf,
    listed: Vec<String>,
    config_exists: bool,
    config_raw: std::result::Result<Option<Vec<u8>>, String>,
    agents_md_raw: std::result::Result<Option<Vec<u8>>, String>,
    repo_config_exists: bool,
    repo_config_raw: std::result::Result<Option<Vec<u8>>, String>,
    skills: Vec<Skill>,
}

impl Snapshot {
    pub fn capture(folders: &Folders, root: &Path, listed: &[String]) -> Snapshot {
        Snapshot::capture_with(folders, root, listed, None)
    }

    pub fn capture_with(
        folders: &Folders,
        root: &Path,
        listed: &[String],
        enabled_override: Option<&Enabled>,
    ) -> Snapshot {
        let mut h = Sha256::new();
        h.update(env!("CARGO_PKG_VERSION").as_bytes());
        h.update(b"|slots-schema:2\n");
        for s in listed {
            h.update(s.as_bytes());
            h.update(b",");
        }
        let config_raw = read_optional(&folders.config);
        let config_exists = !matches!(config_raw, Ok(None));
        stamp_bytes(&mut h, &folders.config, &config_raw);

        let agents_path = root.join("AGENTS.md");
        let agents_md_raw = read_optional(&agents_path);
        stamp_bytes(&mut h, &agents_path, &agents_md_raw);

        let repo_config_path = root.join(REPO_CONFIG_FILE);
        let repo_config_raw = read_optional(&repo_config_path);
        let repo_config_exists = !matches!(repo_config_raw, Ok(None));
        stamp_bytes(&mut h, &repo_config_path, &repo_config_raw);

        let installed_path = folders.claude.join("plugins").join("installed_plugins.json");
        let installed_raw = read_optional(&installed_path);
        stamp_bytes(&mut h, &installed_path, &installed_raw);

        let settings_paths = [
            folders.claude.join("settings.json"),
            root.join(".claude").join("settings.json"),
            root.join(".claude").join("settings.local.json"),
        ];
        let mut settings_bufs = Vec::with_capacity(settings_paths.len());
        for p in &settings_paths {
            let r = read_optional(p);
            stamp_bytes(&mut h, p, &r);
            settings_bufs.push(r.ok().flatten());
        }
        let manifest_path = folders.manifest();
        let manifest_raw = read_optional(&manifest_path);
        stamp_bytes(&mut h, &manifest_path, &manifest_raw);

        let owned_enabled;
        let enabled = match enabled_override {
            Some(e) => e,
            None => {
                let refs: Vec<Option<&[u8]>> = settings_bufs.iter().map(|b| b.as_deref()).collect();
                owned_enabled = Enabled::from_raw_files(&refs);
                &owned_enabled
            }
        };
        let installed_bytes = installed_raw.as_ref().ok().and_then(|o| o.as_deref());
        let roots = search_roots_with(folders, root, enabled, installed_bytes);

        let mut seen = HashSet::new();
        let mut skills = Vec::new();
        for r in &roots {
            stamp_root(&mut h, r);
            for n in sorted_names(&r.dir) {
                let path = r.dir.join(&n).join("SKILL.md");
                let real = realpath(&path);
                h.update(text(&path).as_bytes());
                h.update(text(&real).as_bytes());
                match read_skill_file(&path) {
                    Some((m, raw)) => {
                        let mtime = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
                        h.update(format!("|{}|{}|{:?}|", m.is_dir(), m.len(), mtime).as_bytes());
                        h.update(Sha256::digest(&raw));
                        h.update(b"\n");
                        let meta = frontmatter_from_bytes(&raw);
                        let bare = meta.get("name").filter(|x| !x.is_empty()).cloned().unwrap_or_else(|| n.clone());
                        let name = match &r.namespace {
                            Some(ns) => format!("{ns}:{bare}"),
                            None => bare.clone(),
                        };
                        if seen.insert((real, name.clone())) {
                            skills.push(Skill {
                                name,
                                bare,
                                dir: Some(n),
                                path: Some(text(&path)),
                                inline: r.inline || is_inline(&meta),
                                at: r.label.clone(),
                            });
                        }
                    }
                    None => h.update(b"|missing\n"),
                }
            }
        }
        let fingerprint = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        Snapshot {
            fingerprint,
            folders: folders.clone(),
            root: root.to_path_buf(),
            listed: listed.to_vec(),
            config_exists,
            config_raw,
            agents_md_raw,
            repo_config_exists,
            repo_config_raw,
            skills,
        }
    }

    pub fn build(&self) -> Resolved {
        Resolved::build_from_snapshot(self)
    }
}

pub fn fingerprint(i: &Inputs) -> String {
    Snapshot::capture_with(i.folders, i.root, i.listed, Some(i.enabled)).fingerprint
}

pub fn git_root(start: &Path) -> PathBuf {
    let start = std::path::absolute(start).unwrap_or_else(|_| start.to_path_buf());
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&start)
        .args(["rev-parse", "--show-toplevel"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim_end_matches(['\r', '\n']).to_string();
            if s.is_empty() { start } else { PathBuf::from(s) }
        }
        _ => start,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_repr_matches_for_the_shapes_messages_use() {
        assert_eq!(py_repr(&json!("ship")), "'ship'");
        assert_eq!(py_repr(&json!("it's")), "\"it's\"");
        assert_eq!(py_repr(&json!(["a", "b"])), "['a', 'b']");
        assert_eq!(py_repr(&json!(null)), "None");
        assert_eq!(py_repr(&json!(2)), "2");
    }

    #[test]
    fn skill_refs_follow_the_python_pattern() {
        for ok in ["pr", "cbrip:sol-loop", "a/b:c.d", "x_y-z"] {
            assert!(skill_ref_ok(ok), "{ok}");
        }
        for bad in ["", "a b", "a:b:c", ":x", "x:", "a/b"] {
            assert!(!skill_ref_ok(bad), "{bad}");
        }
    }

    #[test]
    fn the_block_reads_every_slot() {
        let (cfg, problems) = parse_agent_workflow(
            "# R\n\n## Agent workflow\n\n- Review: none\n- Passes: `a`, `b`\n\
             - Credential renewal: `X|Y` -> `renew`; 401 → gh-login\n- Delivery: tag-deploy (see below)\n\
             - Deploy skill: `deploy` (`content/v*`)\n- Ticket model: pool `opus`\n\
             - Worker model: claude-opus-5-5, max effort\n\n## Other\n\n- Review: ignored\n",
        );
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(
            Value::Object(cfg),
            json!({"review": null, "passes": ["a", "b"],
                   "credentials": [{"match": "X|Y", "skill": "renew"}, {"match": "401", "skill": "gh-login"}],
                   "delivery": {"mode": "tag-deploy", "skill": "deploy"},
                   "models": {"ticket": {"pool": "opus"}, "worker": {"model": "claude-opus-5-5", "effort": "max"}}})
        );
    }

    #[test]
    fn python_only_regex_syntax_compiles() {
        assert!(search(r"(?<=token )expired", "token expired").unwrap());
        assert!(search(r"(a)\1", "aa").unwrap());
        assert!(compile("(").is_err());
        assert!(search("(?a)^token expired$", "token expired").unwrap());
        assert!(search("(?ai)^TOKEN", "token").unwrap());
        assert!(search("(?a:x)y", "xy").unwrap());
        assert!(search("[(?a)X]", "a").unwrap());
        assert!(search("[(?a)X]", "(").unwrap());
        assert!(search("[(?a)X]", "X").unwrap());
        assert!(!search("(?a)^\\w+$", "café").unwrap());
        assert!(search("(?a)^\\w+$", "cafe_1").unwrap());
        assert!(search("(?a)\\W", "é").unwrap());
        assert!(!search("(?a)\\d", "\u{0662}").unwrap());
        assert!(search("(?a)\\D", "\u{0662}").unwrap());
        assert!(!search("(?a)\\s", "\u{00a0}").unwrap());
        assert!(search("(?a)\\S", "\u{00a0}").unwrap());
        assert!(search("(?a)\\ba", "éa").unwrap());
        assert!(!search("(?a)\\Ba", "éa").unwrap());
        assert!(!search("(?ai)k", "\u{212a}").unwrap());
        assert!(!search("(?ai)é", "É").unwrap());
        assert!(!search("(?a)[\\w]", "é").unwrap());
        assert!(search("(?a)[\\W]", "é").unwrap());
        assert!(search(r"\\(?a:\w)", r"\a").unwrap());
        assert!(!search(r"\\(?a:\w)", r"\é").unwrap());
        assert!(search("(?x) # (?a)\n\\w", "é").unwrap());
        assert!(search("(?a)\\w(?u:\\w)", "aé").unwrap());
        assert!(!search("(?a)\\w(?u:\\w)", "éa").unwrap());
        assert!(compile("(?au)x").is_err());
        assert!(compile("(?-a:x)").is_err());
    }

    #[test]
    fn a_roots_inline_rule_is_part_of_the_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let root = |inline| SearchRoot {
            label: "~/.gemini/config/skills".into(),
            namespace: None,
            dir: dir.path().to_path_buf(),
            inline,
        };
        let digest = |r: &SearchRoot| {
            let mut h = Sha256::new();
            stamp_root(&mut h, r);
            h.finalize()
        };
        assert_ne!(digest(&root(true)), digest(&root(false)));
    }

    #[test]
    fn snapshot_resolves_the_exact_bytes_it_fingerprinted() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let skill_dir = home.join(".claude/skills/pr");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let md = skill_dir.join("SKILL.md");
        std::fs::write(&md, "---\nname: pr\ndescription: test\n---\n\nBody.\n").unwrap();
        let folders = Folders::under(&home, None);
        std::fs::create_dir_all(folders.config.parent().unwrap()).unwrap();
        std::fs::write(&folders.config, json!({"passes": ["pr"]}).to_string()).unwrap();

        let snap = Snapshot::capture(&folders, &repo, &[]);
        // Mutate on disk AFTER capture; building from the snapshot must still reflect captured bytes.
        std::fs::write(&md, "---\nname: pr\ndisable-model-invocation: true\n---\n\nBody.\n").unwrap();
        let resolved = snap.build();
        assert_eq!(resolved.passes["skills"][0]["invoke"], "skill");

        let next = Snapshot::capture(&folders, &repo, &[]);
        assert_ne!(snap.fingerprint, next.fingerprint);
        assert_eq!(next.build().passes["skills"][0]["invoke"], "inline");
    }

    #[cfg(unix)]
    #[test]
    fn git_root_preserves_trailing_space_in_worktree_path() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo ");
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        let ok = std::process::Command::new("git").arg("init").arg("-q").arg(&repo).status().is_ok_and(|s| s.success());
        if !ok {
            return;
        }
        assert_eq!(realpath(&git_root(&repo.join("sub"))), realpath(&repo));
    }

    #[test]
    fn glob_matching_and_validation_cover_repo_patterns() {
        assert!(glob_matches("training/**", "training/course-1/lesson.md").unwrap());
        assert!(glob_matches("training/**", "./training/README.md").unwrap());
        assert!(glob_matches("training/**", r"training\sub\file.md").unwrap());
        assert!(!glob_matches("training/**", "training-extra/file.md").unwrap());
        assert!(!glob_matches("training/**", "src/app.ts").unwrap());
        assert!(glob_matches("training/", "training/a/b.md").unwrap());

        assert!(glob_matches("**/Dockerfile", "Dockerfile").unwrap());
        assert!(glob_matches("**/Dockerfile", "deploy/docker/Dockerfile").unwrap());
        assert!(glob_matches("a/**/b.rs", "a/b.rs").unwrap());
        assert!(glob_matches("a/**/b.rs", "a/x/y/b.rs").unwrap());
        assert!(!glob_matches("a/**/b.rs", "a/x/c.rs").unwrap());

        assert!(glob_matches("docs/*.md", "docs/guide.md").unwrap());
        assert!(!glob_matches("docs/*.md", "docs/sub/guide.md").unwrap());
        assert!(glob_matches("src/**/*.{ts,tsx}", "src/index.ts").unwrap());
        assert!(glob_matches("src/**/*.{ts,tsx}", "src/ui/App.tsx").unwrap());
        assert!(!glob_matches("src/**/*.{ts,tsx}", "src/ui/App.js").unwrap());
        assert!(glob_matches("notes/v?.md", "notes/v2.md").unwrap());
        assert!(!glob_matches("notes/v?.md", "notes/v10.md").unwrap());
        assert!(glob_matches("data/[a-z].json", "data/k.json").unwrap());
        assert!(!glob_matches("data/[a-z].json", "data/1.json").unwrap());
        assert!(glob_matches("data/[!0-9].json", "data/k.json").unwrap());
        assert!(!glob_matches("data/[!0-9].json", "data/1.json").unwrap());
        assert!(!glob_matches("data/[!0-9].json", "data//.json").unwrap());

        for bad in ["", "   ", "/", "a/[unclosed", "a/[]", "a/{x,y", "a/x}", "a/{}", r"a\"] {
            assert!(validate_glob(bad).is_err(), "expected invalid glob: {bad:?}");
        }
        let deep = format!("{}a{}", "{".repeat(MAX_GLOB_BRACE_DEPTH + 1), "}".repeat(MAX_GLOB_BRACE_DEPTH + 1));
        assert!(validate_glob(&deep).unwrap_err().contains("maximum depth"));
    }

    #[test]
    fn validate_slots_checks_every_delivery_steps_invariant() {
        let check = |delivery: Value| -> Vec<String> {
            let mut cfg = Map::new();
            cfg.insert("delivery".into(), delivery);
            validate_slots(&cfg, ".t3-workflow.json")
        };

        // Valid lms-shaped 3-stage config
        assert!(
            check(json!({
                "steps": [
                    {"id": "merge", "kind": "builtin", "gate": "always", "scope": "pr"},
                    {"id": "deploy", "skill": "deploy", "gate": "chain", "scope": "ticket", "watch": true},
                    {"id": "publish", "skill": "lms-publish", "gate": "always", "scope": "post-handoff", "paths": ["training/**"]}
                ],
                "closeAfter": "ticket"
            }))
            .is_empty()
        );

        // Valid manual + never gate + non-merge start
        assert!(
            check(json!({
                "steps": [
                    {"id": "verify", "skill": "verify", "gate": "never", "scope": "pr"},
                    {"id": "announce", "kind": "manual", "prompt": "Post release notes", "gate": "always", "scope": "post-handoff"}
                ],
                "closeAfter": "all"
            }))
            .is_empty()
        );

        // Empty steps
        assert!(check(json!({"steps": []})).iter().any(|p| p.contains("at least one step")));
        // Mixing legacy and steps
        assert!(
            check(json!({"mode": "merge", "steps": [{"id": "merge", "kind": "builtin", "gate": "always"}]}))
                .iter()
                .any(|p| p.contains("cannot mix"))
        );
        // Merge not first
        assert!(
            check(json!({"steps": [
                {"id": "deploy", "skill": "deploy", "gate": "always"},
                {"id": "merge", "kind": "builtin", "gate": "always"}
            ]}))
            .iter()
            .any(|p| p.contains("must be first"))
        );
        // Duplicate id (including duplicate merge)
        assert!(
            check(json!({"steps": [
                {"id": "merge", "kind": "builtin", "gate": "always"},
                {"id": "merge", "kind": "builtin", "gate": "always"}
            ]}))
            .iter()
            .any(|p| p.contains("duplicate step id"))
        );
        // Builtin with non-merge id
        assert!(
            check(json!({"steps": [{"id": "deploy", "kind": "builtin", "gate": "always"}]}))
                .iter()
                .any(|p| p.contains("only supported for id 'merge'"))
        );
        // Builtin merge with non-pr scope
        assert!(
            check(json!({"steps": [{"id": "merge", "kind": "builtin", "gate": "always", "scope": "ticket"}]}))
                .iter()
                .any(|p| p.contains("must have scope 'pr'"))
        );
        // Chain without preceding always
        assert!(
            check(json!({"steps": [{"id": "deploy", "skill": "deploy", "gate": "chain"}]}))
                .iter()
                .any(|p| p.contains("'chain' requires a preceding step with gate 'always'"))
        );
        // Decreasing scope order
        assert!(
            check(json!({"steps": [
                {"id": "deploy", "skill": "deploy", "gate": "always", "scope": "ticket"},
                {"id": "verify", "skill": "verify", "gate": "never", "scope": "pr"}
            ]}))
            .iter()
            .any(|p| p.contains("cannot follow a broader scope"))
        );
        // Manual missing prompt
        assert!(
            check(json!({"steps": [{"id": "announce", "kind": "manual", "gate": "always"}]}))
                .iter()
                .any(|p| p.contains("requires a non-empty prompt"))
        );
        // Bad glob in paths
        assert!(
            check(json!({"steps": [{"id": "publish", "skill": "pub", "gate": "always", "paths": ["training/[bad"]}]}))
                .iter()
                .any(|p| p.contains("invalid glob"))
        );
        // Bad closeAfter
        assert!(
            check(json!({
                "steps": [{"id": "merge", "kind": "builtin", "gate": "always"}],
                "closeAfter": "never"
            }))
            .iter()
            .any(|p| p.contains("closeAfter"))
        );
        // Non-boolean watch
        assert!(
            check(json!({"steps": [{"id": "deploy", "skill": "deploy", "gate": "always", "watch": "yes"}]}))
                .iter()
                .any(|p| p.contains("watch: must be a boolean"))
        );
    }
}
