//! Chinese or English for user-facing text, chosen once at startup.
//!
//! Text is written inline in both languages with [`tr!`](crate::tr):
//! `tr!("連線失敗：{e}", "Connection failed: {e}")`.

use std::sync::atomic::{AtomicBool, Ordering};

static ENGLISH: AtomicBool = AtomicBool::new(false);

pub fn set_english(english: bool) {
    ENGLISH.store(english, Ordering::Relaxed);
}

pub fn is_english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

/// Chinese when Windows' display language is Chinese, English otherwise.
pub fn system_prefers_english() -> bool {
    #[cfg(windows)]
    {
        const LANG_CHINESE: u16 = 0x04;
        let language = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
        language & 0x3FF != LANG_CHINESE
    }
    #[cfg(not(windows))]
    {
        !std::env::var("LANG").unwrap_or_default().starts_with("zh")
    }
}

/// Formats the Chinese or the English text, depending on [`is_english`].
#[macro_export]
macro_rules! tr {
    ($zh:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if $crate::i18n::is_english() { format!($en $(, $arg)*) } else { format!($zh $(, $arg)*) }
    };
}

#[cfg(test)]
mod tests {
    #[test]
    fn picks_the_language_and_captures_arguments() {
        let n = 3;
        super::set_english(true);
        assert_eq!(tr!("第 {n} 次", "attempt {n}"), "attempt 3");
        super::set_english(false);
        assert_eq!(tr!("第 {n} 次", "attempt {n}"), "第 3 次");
        assert_eq!(tr!("{} 與 {}", "{} and {}", 1, 2), "1 與 2");
    }
}
