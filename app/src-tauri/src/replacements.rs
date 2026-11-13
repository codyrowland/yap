use serde::{Deserialize, Serialize};

pub const MAX_REPLACEMENTS: usize = 500;
pub const MAX_SAY_CHARS: usize = 100;
pub const MAX_TEXT_CHARS: usize = 10_000;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Replacement {
    pub say: String,
    pub text: String,
}

pub fn phrase_words(phrase: &str) -> Vec<String> {
    words(phrase)
        .into_iter()
        .map(|(start, end)| phrase[start..end].to_lowercase())
        .collect()
}

fn words(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    for (index, character) in text.char_indices() {
        match (character.is_alphanumeric(), start) {
            (true, None) => start = Some(index),
            (false, Some(begin)) => {
                spans.push((begin, index));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(begin) = start {
        spans.push((begin, text.len()));
    }

    spans
}

pub fn validate(replacements: &[Replacement]) -> Result<(), String> {
    if replacements.len() > MAX_REPLACEMENTS {
        return Err(format!(
            "You can have up to {MAX_REPLACEMENTS} replacements"
        ));
    }

    let mut seen = std::collections::HashSet::new();
    for replacement in replacements {
        let words = phrase_words(&replacement.say);
        if words.is_empty() {
            return Err("Each replacement needs a phrase to say".to_owned());
        }

        if replacement.say.chars().count() > MAX_SAY_CHARS {
            return Err(format!("Phrases can be up to {MAX_SAY_CHARS} characters"));
        }

        if replacement.text.trim().is_empty() {
            return Err(format!("“{}” needs text to insert", replacement.say.trim()));
        }

        if replacement.text.chars().count() > MAX_TEXT_CHARS {
            return Err(format!(
                "Replacement text can be up to {MAX_TEXT_CHARS} characters"
            ));
        }

        if !seen.insert(words) {
            return Err(format!("You already have “{}”", replacement.say.trim()));
        }
    }

    Ok(())
}

pub fn apply(text: &str, replacements: &[Replacement]) -> String {
    let mut phrases: Vec<(Vec<String>, &str)> = replacements
        .iter()
        .map(|replacement| (phrase_words(&replacement.say), replacement.text.as_str()))
        .filter(|(words, _)| !words.is_empty())
        .collect();
    if phrases.is_empty() {
        return text.to_owned();
    }

    phrases.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
    let spans = words(text);
    let lowered: Vec<String> = spans
        .iter()
        .map(|&(start, end)| text[start..end].to_lowercase())
        .collect();

    let mut output = String::with_capacity(text.len());
    let mut copied = 0;
    let mut index = 0;
    while index < spans.len() {
        let found = phrases.iter().find(|(words, _)| {
            lowered
                .get(index..index + words.len())
                .is_some_and(|candidate| candidate == words.as_slice())
        });
        let Some((words, insert)) = found else {
            index += 1;
            continue;
        };

        let end = index + words.len();
        if index == 0 && end == spans.len() {
            return (*insert).to_owned();
        }

        output.push_str(&text[copied..spans[index].0]);
        output.push_str(insert);
        copied = spans[end - 1].1;
        index = end;
    }
    output.push_str(&text[copied..]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replacement(say: &str, text: &str) -> Replacement {
        Replacement {
            say: say.to_owned(),
            text: text.to_owned(),
        }
    }

    #[test]
    fn replaces_a_phrase_anywhere_in_any_case() {
        let list = [replacement("my email", "cody@example.com")];
        assert_eq!(
            apply("Send it to My Email please.", &list),
            "Send it to cody@example.com please."
        );
        assert_eq!(
            apply("my email, then my email.", &list),
            "cody@example.com, then cody@example.com."
        );
    }

    #[test]
    fn ignores_punctuation_between_and_around_words() {
        let list = [replacement("sign off", "Thanks,\nCody")];
        assert_eq!(apply("Sign-off.", &list), "Thanks,\nCody");
        assert_eq!(
            apply("See you. Sign, off!", &list),
            "See you. Thanks,\nCody!"
        );
    }

    #[test]
    fn a_dictation_that_is_only_the_phrase_becomes_only_the_text() {
        let list = [replacement("my email", "cody@example.com")];
        assert_eq!(apply("My email.", &list), "cody@example.com");
        assert_eq!(apply("  my email ", &list), "cody@example.com");
    }

    #[test]
    fn matches_whole_words_only() {
        let list = [replacement("cat", "🐈")];
        assert_eq!(
            apply("A category of cats, not a cat.", &list),
            "A category of cats, not a 🐈."
        );
    }

    #[test]
    fn longest_phrase_wins_and_inserted_text_is_not_matched_again() {
        let list = [
            replacement("email", "EMAIL"),
            replacement("my work email", "cody@work.com email"),
        ];
        assert_eq!(
            apply("use my work email or email", &list),
            "use cody@work.com email or EMAIL"
        );
    }

    #[test]
    fn leaves_text_alone_without_matches() {
        assert_eq!(apply("Hello there.", &[]), "Hello there.");
        let list = [replacement("my email", "x")];
        assert_eq!(apply("my emails", &list), "my emails");
        assert_eq!(apply("", &list), "");
    }

    #[test]
    fn validation_rejects_empty_and_duplicate_phrases() {
        assert!(validate(&[replacement("my email", "x")]).is_ok());
        assert!(validate(&[replacement(" ... ", "x")]).is_err());
        assert!(validate(&[replacement("my email", "  ")]).is_err());
        assert!(validate(&[replacement("my email", "x"), replacement("My, Email", "y")]).is_err());
        assert!(validate(&[replacement(&"a".repeat(MAX_SAY_CHARS + 1), "x")]).is_err());
    }
}
