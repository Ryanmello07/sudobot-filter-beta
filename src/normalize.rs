//! Text normalization and URL extraction.

use regex::Regex;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;
use unicode_normalization::UnicodeNormalization;

fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}' | '\u{00AD}'
    )
}

/// Common Cyrillic/Greek lookalikes mapped to ASCII.
fn homoglyph(c: char) -> char {
    match c {
        'а' | 'α' => 'a',
        'в' => 'b',
        'с' | 'ϲ' => 'c',
        'е' | 'ε' => 'e',
        'һ' => 'h',
        'і' | 'ι' | 'ı' => 'i',
        'ј' => 'j',
        'к' | 'κ' => 'k',
        'м' => 'm',
        'н' => 'h',
        'о' | 'ο' => 'o',
        'р' | 'ρ' => 'p',
        'ѕ' => 's',
        'т' | 'τ' => 't',
        'х' | 'χ' => 'x',
        'у' | 'γ' => 'y',
        _ => c,
    }
}

/// NFKC, strip invisible characters, lowercase, map homoglyphs, collapse whitespace.
pub fn normalize(text: &str) -> String {
    let mapped: String = text
        .nfkc()
        .filter(|c| !is_invisible(*c))
        .flat_map(|c| c.to_lowercase())
        .map(homoglyph)
        .collect();
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn hash_text(normalized: &str) -> String {
    hex(&Sha256::digest(normalized.as_bytes()))
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn url_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?i)\b(?:https?://|www\.)[^\s<>()\[\]]+|\b[a-z0-9-]+(?:\.[a-z0-9-]+)*\.(?:com|net|org|io|gg|xyz|ru|top|info|co|me|app|link|click|live|shop|site|online|club|cc|ly|to)(?:/[^\s<>()\[\]]*)?\b")
            .unwrap()
    })
}

fn invite_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?i)(?:discord\.gg|discord(?:app)?\.com/invite|dsc\.gg)/([a-z0-9-]+)").unwrap()
    })
}

/// Lowercased host names of every URL-like token in `text`.
/// Text is normalized first so split or decorated links still match.
pub fn extract_domains(text: &str) -> Vec<String> {
    let n = normalize(text);
    let mut out = Vec::new();
    for m in url_re().find_iter(&n) {
        let raw = m.as_str();
        let with_scheme = if raw.contains("://") { raw.to_string() } else { format!("http://{raw}") };
        if let Ok(u) = url::Url::parse(&with_scheme) {
            if let Some(h) = u.host_str() {
                let h = h.trim_start_matches("www.").to_string();
                if !out.contains(&h) {
                    out.push(h);
                }
            }
        }
    }
    out
}

pub fn extract_invite_codes(text: &str) -> Vec<String> {
    let n = normalize(text);
    invite_re().captures_iter(&n).map(|c| c[1].to_string()).collect()
}

pub fn contains_url(text: &str) -> bool {
    !extract_domains(text).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_zero_width_and_homoglyphs() {
        assert_eq!(normalize("fr\u{200B}ee  nitro"), "free nitro");
        assert_eq!(normalize("frее nіtrо"), "free nitro");
    }

    #[test]
    fn nfkc_folds_fullwidth() {
        assert_eq!(normalize("ＦＲＥＥ ＮＩＴＲＯ"), "free nitro");
    }

    #[test]
    fn same_hash_for_evasions() {
        assert_eq!(hash_text(&normalize("Free  Nitro")), hash_text(&normalize("fr\u{200B}ee nitro")));
    }

    #[test]
    fn extracts_domains() {
        let d = extract_domains("go to https://Evil.example.com/x?y=1 or www.scam.xyz now");
        assert_eq!(d, vec!["evil.example.com".to_string(), "scam.xyz".to_string()]);
        assert!(extract_domains("hello world, nothing here").is_empty());
    }

    #[test]
    fn extracts_bare_domain() {
        assert_eq!(extract_domains("claim at free-nitro.gg/abc"), vec!["free-nitro.gg".to_string()]);
    }

    #[test]
    fn extracts_invites() {
        let c = extract_invite_codes("join discord.gg/AbC123 or https://discord.com/invite/xyz");
        assert_eq!(c, vec!["abc123".to_string(), "xyz".to_string()]);
    }
}
