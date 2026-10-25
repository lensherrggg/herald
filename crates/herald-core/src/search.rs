use std::sync::OnceLock;

use jieba_rs::Jieba;

fn jieba() -> &'static Jieba {
    static JIEBA: OnceLock<Jieba> = OnceLock::new();
    JIEBA.get_or_init(Jieba::new)
}

/// Pre-tokenise text with jieba. Tokens are separated by single spaces so the
/// FTS5 unicode61 tokenizer indexes Chinese words instead of whole runs.
pub fn tokenize(text: &str) -> String {
    let mut tokens = Vec::new();
    for token in jieba().cut(text, false) {
        let token = token.word.trim();
        if !token.is_empty() && token.chars().any(|c| c.is_alphanumeric()) {
            tokens.push(token);
        }
    }
    tokens.join(" ")
}

pub fn index_text(subject: Option<&str>, from: Option<&str>, body: Option<&str>) -> String {
    let mut parts = Vec::with_capacity(3);
    if let Some(subject) = subject {
        parts.push(subject);
    }
    if let Some(from) = from {
        parts.push(from);
    }
    if let Some(body) = body {
        parts.push(body);
    }
    tokenize(&parts.join("\n"))
}

/// Turn a user query into an FTS5 MATCH expression. Every token is quoted, so
/// user input cannot inject FTS operators.
pub fn fts_query(query: &str) -> String {
    let tokens = tokenize(query);
    if tokens.is_empty() {
        return String::new();
    }
    tokens
        .split(' ')
        .filter(|token| !token.is_empty())
        .map(|token| format!("\"{}\"", token.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Simple substring query used as a fallback when FTS tokenisation misses a
/// Chinese substring. Returns a LIKE pattern with escaped wildcards.
pub fn like_pattern(query: &str) -> String {
    let escaped = query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chinese_tokens_are_spaced() {
        let tokens = tokenize("关于项目进度的通知");
        assert!(tokens.contains("项目"));
        assert!(tokens.split(' ').count() > 1);
    }

    #[test]
    fn fts_query_is_quoted() {
        let query = fts_query("项目 \"进度\" OR");
        assert!(query.contains("\"项目\""));
        assert!(!query.to_ascii_uppercase().contains(" OR"));
    }

    #[test]
    fn like_pattern_escapes_wildcards() {
        assert_eq!(like_pattern("50%_x"), "%50\\%\\_x%");
    }
}
