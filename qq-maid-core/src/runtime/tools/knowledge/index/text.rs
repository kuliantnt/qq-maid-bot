use std::collections::HashSet;

use sha2::{Digest, Sha256};

pub(super) fn hash_text(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

pub(super) fn build_index_text(text: &str) -> String {
    let mut tokens = lexical_tokens(text);
    tokens.extend(cjk_ngrams(text));
    tokens.extend(ascii_ngrams(text));
    tokens.sort();
    tokens.dedup();
    tokens.join(" ")
}

pub(super) fn build_search_query(text: &str, max_tokens: usize) -> String {
    let mut tokens = lexical_tokens(text);
    tokens.extend(cjk_ngrams(text));
    tokens.extend(ascii_ngrams(text));
    dedup_preserving_order(tokens)
        .into_iter()
        .take(max_tokens)
        .map(|token| escape_fts_token(&token))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn dedup_preserving_order(tokens: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::<String>::new();
    let mut unique = Vec::new();
    for token in tokens {
        // 检索 query 需要保留用户输入靠前的关键词；不能为了去重先排序，
        // 否则达到 token 上限时会按字典序丢掉真正的问题核心词。
        if seen.insert(token.clone()) {
            unique.push(token);
        }
    }
    unique
}

fn lexical_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
            token.push(ch.to_ascii_lowercase());
        } else if !token.is_empty() {
            tokens.push(std::mem::take(&mut token));
        }
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    tokens.into_iter().filter(|item| item.len() >= 2).collect()
}

pub(super) fn relevance_terms(text: &str) -> Vec<String> {
    let mut terms = lexical_tokens(text);
    terms.extend(cjk_ngrams(text));
    dedup_preserving_order(terms)
}

/// 识别通用编号/配置标识形态，只用于相关性信号，不绑定任何业务词表。
pub(super) fn identifier_terms(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut run = String::new();
    for ch in text.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
            run.push(ch);
            continue;
        }
        if run.len() >= 4
            && (run.bytes().any(|byte| byte.is_ascii_digit())
                || run.contains(['_', '-'])
                || run
                    .bytes()
                    .filter(|byte| byte.is_ascii_alphabetic())
                    .all(|byte| byte.is_ascii_uppercase()))
        {
            terms.push(run.to_ascii_lowercase());
        }
        run.clear();
    }
    dedup_preserving_order(terms)
}

fn cjk_ngrams(text: &str) -> Vec<String> {
    let chars = text.chars().filter(|ch| is_cjk(*ch)).collect::<Vec<_>>();
    if chars.is_empty() {
        return Vec::new();
    }

    let mut tokens = Vec::new();
    for ch in &chars {
        tokens.push(ch.to_string());
    }
    if chars.len() >= 2 {
        for window in chars.windows(2) {
            tokens.push(window.iter().collect::<String>());
        }
    }
    if chars.len() >= 3 {
        for window in chars.windows(3) {
            tokens.push(window.iter().collect::<String>());
        }
    }

    // 有 2-gram 或 3-gram 时去掉 1-gram 单字，减少噪声；
    // 否则保留 1-gram 作为短查询（如"D区""站"）的唯一检索信号。
    if tokens.iter().any(|t| t.chars().count() >= 2) {
        tokens.retain(|t| t.chars().count() >= 2);
    }

    tokens
}

fn ascii_ngrams(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut run = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            run.push(ch.to_ascii_lowercase());
        } else {
            push_ascii_ngrams(&mut tokens, &run);
            run.clear();
        }
    }
    push_ascii_ngrams(&mut tokens, &run);
    tokens
}

fn push_ascii_ngrams(tokens: &mut Vec<String>, run: &str) {
    // ASCII 只生成 3-gram：保留 RAG407 这类编号的模糊匹配，同时避免 hi/ok
    // 被拆成单字母后命中任意英文资料。
    const ASCII_NGRAM_SIZE: usize = 3;
    let chars = run.chars().collect::<Vec<_>>();
    if chars.len() < ASCII_NGRAM_SIZE {
        return;
    }
    for window in chars.windows(ASCII_NGRAM_SIZE) {
        tokens.push(window.iter().collect::<String>());
    }
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x4E00..=0x9FFF
            | 0x3400..=0x4DBF
            | 0x20000..=0x2A6DF
            | 0x2A700..=0x2B73F
            | 0x2B740..=0x2B81F
            | 0x2B820..=0x2CEAF
            | 0xF900..=0xFAFF
    )
}

fn escape_fts_token(token: &str) -> String {
    format!("\"{}\"", token.replace('"', "\"\""))
}
