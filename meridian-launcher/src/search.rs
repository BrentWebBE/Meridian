//! Fuzzy matching: "ffx" finds "Firefox". Built on `nucleo`, the matcher
//! from the Helix editor, which scores how well a query fits a text.

use nucleo::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};

/// Fuzzy name matches scoring less than this share of the best one are
/// dropped: they are usually coincidences ("terra" in "Text Editor").
const WEAK_MATCH: f64 = 0.5;

pub struct Ranker {
    matcher: Matcher,
    /// Scratch space nucleo uses to view a `&str` as characters.
    buffer: Vec<char>,
}

impl Ranker {
    pub fn new() -> Self {
        Self {
            matcher: Matcher::new(Config::DEFAULT),
            buffer: Vec::new(),
        }
    }

    fn score(&mut self, pattern: &Pattern, text: &str) -> Option<u32> {
        pattern.score(Utf32Str::new(text, &mut self.buffer), &mut self.matcher)
    }

    /// The best `limit` items for `query`, best first. `texts` gives each
    /// item's name and its keywords (lowercase). Names are matched fuzzily;
    /// keywords must contain every word of the query as written, because
    /// fuzzy matching against long descriptions finds almost anything.
    /// An empty query returns the first `limit` items in their original order.
    pub fn rank<'a, T>(
        &mut self,
        items: impl IntoIterator<Item = &'a T>,
        query: &str,
        limit: usize,
        texts: impl Fn(&T) -> (&str, &str),
    ) -> Vec<&'a T> {
        let query = query.trim();
        if query.is_empty() {
            return items.into_iter().take(limit).collect();
        }
        // Ignore case and accents: "cafe" finds "Café".
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);

        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();

        let mut by_name: Vec<(u32, &T)> = Vec::new();
        let mut by_keyword: Vec<&T> = Vec::new();
        for item in items {
            let (name, keywords) = texts(item);
            if let Some(score) = self.score(&pattern, name) {
                by_name.push((score, item));
            } else if words.iter().all(|word| keywords.contains(word.as_str())) {
                by_keyword.push(item);
            }
        }

        // Highest score first; the sort is stable, so ties keep their order.
        by_name.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        let best = by_name.first().map_or(0, |(score, _)| *score) as f64;
        by_name
            .into_iter()
            .filter(|(score, _)| *score as f64 >= best * WEAK_MATCH)
            .map(|(_, item)| item)
            // Keyword matches come after all name matches.
            .chain(by_keyword)
            .take(limit)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APPS: [(&str, &str); 4] = [
        ("Text Editor", "edit text files gedit"),
        ("Firefox", "web browser internet"),
        ("Terminal", "command line shell"),
        ("Files", "file manager nautilus"),
    ];

    fn names(query: &str) -> Vec<&'static str> {
        Ranker::new()
            .rank(APPS.iter(), query, 10, |app| (app.0, app.1))
            .into_iter()
            .map(|app| app.0)
            .collect()
    }

    #[test]
    fn letters_may_be_skipped() {
        assert_eq!(names("ffx"), ["Firefox"]);
        assert_eq!(names("TRM"), ["Terminal"]);
    }

    #[test]
    fn a_name_match_beats_a_keyword_match() {
        // "file" is in the name of Files and only in the keywords of Text Editor.
        assert_eq!(names("file")[0], "Files");
        assert!(names("file").contains(&"Text Editor"));
    }

    #[test]
    fn keywords_are_searched_too() {
        assert_eq!(names("browser"), ["Firefox"]);
    }

    #[test]
    fn scattered_letters_in_keywords_do_not_match() {
        // "ter" is spread through "edit text files gedit" but is not a word in it.
        assert!(!names("xtf").contains(&"Text Editor"));
        // "nautilus" is a keyword of Files, written out in full.
        assert_eq!(names("nautilus"), ["Files"]);
    }

    #[test]
    fn empty_query_keeps_the_order() {
        assert_eq!(names("  "), ["Text Editor", "Firefox", "Terminal", "Files"]);
        assert!(names("zzzz").is_empty());
    }
}
