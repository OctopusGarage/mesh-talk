//! Local, approximate language ratios for direct messages. Models are bundled;
//! evaluation never performs network I/O. Ratios count alphabetic graphemes,
//! not detector confidence. Ambiguous or unsupported script spans count as other.
use lingua::{Language, LanguageDetector, LanguageDetectorBuilder};
use serde::Serialize;
use std::sync::LazyLock;
use unicode_script::{Script, UnicodeScript};
use unicode_segmentation::UnicodeSegmentation;

pub const MIN_TARGET_PERCENT: u8 = 80;
pub const SUPPORTED_LANGUAGES: &[&str] =
    &["en", "zh", "ja", "es", "fr", "de", "ko", "pt", "ru", "ar"];
// Reject the complete message on any resource bound; never classify a prefix.
const MAX_BYTES: usize = 4096;
const MAX_RUNS: usize = 64;
const MAX_WORDS: usize = 256;

static DETECTOR: LazyLock<LanguageDetector> = LazyLock::new(|| {
    LanguageDetectorBuilder::from_languages(&[
        Language::English,
        Language::Chinese,
        Language::Japanese,
        Language::Spanish,
        Language::French,
        Language::German,
        Language::Korean,
        Language::Portuguese,
        Language::Russian,
        Language::Arabic,
    ])
    .with_minimum_relative_distance(0.1)
    .build()
});

#[derive(Debug, Clone, Serialize)]
pub struct LanguageVerdict {
    pub allowed: bool,
    pub target_percent: u8,
    pub language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
}

pub fn is_supported(language: &str) -> bool {
    SUPPORTED_LANGUAGES.contains(&language)
}

pub fn evaluate(language: &str, text: &str) -> LanguageVerdict {
    let rejected = || LanguageVerdict {
        reason: Some("resource-limit"),
        ..verdict(language, 0, 1)
    };
    if !is_supported(language) {
        return LanguageVerdict {
            reason: Some("unsupported-language"),
            ..verdict(language, 0, 1)
        };
    }
    if text.len() > MAX_BYTES {
        return rejected();
    }
    let text = without_exemptions(text);
    if text.unicode_words().take(MAX_WORDS + 1).count() > MAX_WORDS {
        return rejected();
    }
    let acknowledgement = text
        .trim()
        .trim_end_matches(['!', '.', '?'])
        .to_ascii_lowercase();
    if matches!(acknowledgement.as_str(), "ok" | "okay") {
        return verdict(language, 1, 1);
    }
    let total = letters(&text);
    if total == 0 {
        return verdict(language, 0, 0);
    }
    let mut runs: Vec<(u8, String)> = Vec::new();
    for grapheme in text.graphemes(true) {
        let kind = grapheme
            .chars()
            .find(|ch| ch.is_alphabetic())
            .map(script_kind);
        if let Some(kind) = kind
            && runs.last().is_none_or(|(previous, _)| *previous != kind)
        {
            if runs.len() == MAX_RUNS {
                return rejected();
            }
            runs.push((kind, String::new()));
        }
        if let Some((_, run)) = runs.last_mut() {
            run.push_str(grapheme);
        }
    }
    let mut target = 0;
    for (kind, run) in runs {
        if kind == 0 {
            continue;
        }
        if kind == 1 && run.unicode_words().count() > 3 {
            // Lingua segments languages sharing the Latin script. Its byte ranges
            // identify text spans only; percentages always come from our counts.
            for span in DETECTOR.detect_multiple_languages_of(run.clone()) {
                if span
                    .language()
                    .iso_code_639_1()
                    .to_string()
                    .eq_ignore_ascii_case(language)
                {
                    target += letters(&run[span.start_index()..span.end_index()]);
                }
            }
        } else if DETECTOR
            .detect_language_of(run.clone())
            .is_some_and(|detected| {
                detected
                    .iso_code_639_1()
                    .to_string()
                    .eq_ignore_ascii_case(language)
            })
        {
            target += letters(&run);
        }
    }
    verdict(language, target, total)
}

fn verdict(language: &str, target: usize, total: usize) -> LanguageVerdict {
    let target_percent = (target * 100).checked_div(total).unwrap_or(100) as u8;
    LanguageVerdict {
        allowed: total == 0 || target * 100 >= total * usize::from(MIN_TARGET_PERCENT),
        target_percent,
        language: language.to_owned(),
        reason: None,
    }
}

fn letters(text: &str) -> usize {
    text.graphemes(true)
        .filter(|g| g.chars().any(char::is_alphabetic))
        .count()
}

fn script_kind(ch: char) -> u8 {
    match ch.script() {
        Script::Latin => 1,
        Script::Han | Script::Hiragana | Script::Katakana => 2,
        Script::Hangul => 3,
        Script::Cyrillic => 4,
        Script::Arabic => 5,
        _ => 0,
    }
}

// Only complete, bounded code spans and ASCII URL/mention tokens are exempt.
// Unclosed delimiters and Unicode URL lookalikes retain their language weight.
fn without_exemptions(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        let rest = &text[index..];
        if rest.starts_with('`') {
            let delimiter = if rest.starts_with("```") { "```" } else { "`" };
            let content = &rest[delimiter.len()..];
            if let Some(end) = content.find(delimiter)
                && end <= 1024
            {
                output.push(' ');
                index += delimiter.len() * 2 + end;
                continue;
            }
        }
        let at_boundary = index == 0
            || text[..index]
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace);
        if at_boundary
            && (rest.starts_with("https://")
                || rest.starts_with("http://")
                || rest.starts_with('@'))
        {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let token = &rest[..end];
            let mention = token.starts_with('@');
            let valid = if mention {
                token.len() > 1
                    && token.len() <= 64
                    && token[1..]
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            } else {
                token.len() <= 512 && token.is_ascii() && token.contains('.')
            };
            if valid {
                output.push(' ');
                index += end;
                continue;
            }
        }
        let ch = rest
            .chars()
            .next()
            .expect("index remains on a character boundary");
        output.push(ch);
        index += ch.len_utf8();
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifies_target_and_foreign_prose() {
        for (lang, text) in [
            (
                "en",
                "Please send the updated document before our meeting tomorrow morning.",
            ),
            (
                "fr",
                "Nous allons préparer le dîner ensemble après notre promenade dans le jardin.",
            ),
            (
                "es",
                "Necesitamos terminar el trabajo antes de salir de la oficina esta tarde.",
            ),
            (
                "de",
                "Wir möchten morgen gemeinsam einen Spaziergang durch den schönen Garten machen.",
            ),
            (
                "pt",
                "Precisamos terminar o trabalho antes de sair do escritório esta tarde.",
            ),
            (
                "ru",
                "Мы собираемся обсудить новый проект завтра утром после завтрака.",
            ),
            ("ar", "نريد مناقشة المشروع الجديد غدا في الصباح بعد الإفطار."),
            (
                "ko",
                "우리는 내일 아침에 새로운 프로젝트에 대해 이야기할 것입니다.",
            ),
            (
                "zh",
                "我们明天早上一起讨论这个新的项目，然后准备下一次会议。",
            ),
            (
                "ja",
                "明日の朝、新しいプロジェクトについて一緒に話しましょう。",
            ),
        ] {
            assert!(
                evaluate(lang, text).allowed,
                "{lang}: {:?}",
                evaluate(lang, text)
            );
            if lang != "en" {
                assert!(!evaluate("en", text).allowed, "{lang}");
            }
        }
    }
    #[test]
    fn neutral_and_narrow_acknowledgements_pass() {
        for text in [
            "",
            "👍 123 !!!",
            "OK",
            "ok!",
            "Okay",
            "https://example.com/path",
            "`let x = 1`",
            "@alice",
        ] {
            assert!(evaluate("en", text).allowed, "{text}");
        }
    }
    #[test]
    fn foreign_text_cannot_be_diluted_with_neutral_padding() {
        let text = format!("这是中文消息 {}", "123 👍 ! ".repeat(100));
        assert!(!evaluate("en", &text).allowed);
        assert!(
            !evaluate(
                "en",
                "这是中文消息 https://example.com/english `hello world` @alice"
            )
            .allowed
        );
    }
    #[test]
    fn measures_mixed_scripts_by_letter_count() {
        let verdict = evaluate("en", "hello world你好");
        assert_eq!(verdict.target_percent, 83);
        assert!(verdict.allowed);
        assert!(!evaluate("en", "hello你好世界").allowed);
        assert!(evaluate("zh", "我们明天早上一起讨论这个新的项目然后准备会议 hello").allowed);
    }
    #[test]
    fn rejects_foreign_same_script_passages() {
        let text = "Please send the updated document before our meeting tomorrow morning. Nous allons préparer le dîner ensemble après notre promenade dans le jardin.";
        let verdict = evaluate("en", text);
        assert!(!verdict.allowed, "{verdict:?}");
        assert!(
            verdict.target_percent > 10 && verdict.target_percent < 80,
            "{verdict:?}"
        );
    }
    #[test]
    fn unknown_scripts_and_unclosed_exemptions_are_counted() {
        assert!(!evaluate("en", "αβγδεζηθικλμνξοπρστυφχψω").allowed);
        assert!(!evaluate("en", "`这是没有结束的代码消息").allowed);
        assert!(!evaluate("en", "https://这是中文消息").allowed);
    }
    #[test]
    fn uses_exact_threshold_and_counts_graphemes() {
        assert!(verdict("en", 80, 100).allowed);
        assert!(!verdict("en", 799, 1000).allowed);
        assert_eq!(verdict("en", 799, 1000).target_percent, 79);
        assert_eq!(verdict("en", 1, 3).target_percent, 33);
        assert_eq!(letters("e\u{301} 👩‍💻 42 !"), 1);
    }
    #[test]
    fn rejects_excessive_detector_work() {
        assert!(!evaluate("en", &"a ".repeat(MAX_WORDS + 1)).allowed);
        assert!(!evaluate("en", &"a中".repeat(MAX_RUNS)).allowed);
    }
    #[test]
    fn exemptions_are_bounded() {
        let foreign = "这是中文".repeat(100);
        assert!(!evaluate("en", &format!("`{foreign}`")).allowed);
        let url = format!("https://example.com/{}", "a".repeat(600));
        assert!(!without_exemptions(&url).trim().is_empty());
        assert!(is_supported("en"));
        assert!(!is_supported("EN"));
    }
    #[test]
    fn rejects_oversized_or_unsupported_input() {
        assert!(!evaluate("xx", "OK").allowed);
        assert!(!evaluate("en", &"hello world ".repeat(5000)).allowed);
        assert!(!evaluate("en", &format!("hello{}这是中文", " ".repeat(20000))).allowed);
    }
}
