//! Local English intent grammar. Unrecognized speech remains dictation.
//!
//! This deliberately accepts a small set of whole-utterance imperatives with
//! an explicit reference to selected text. It never calls an inference service.
//! The caller must separately require a real captured selection and opt-in.
use once_cell::sync::Lazy;
use regex::Regex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditIntent {
    pub instruction: String,
}

const TARGET: &str = r"(?:this(?: text| selection)?|the (?:selected text|selection))";

static RULES: Lazy<Vec<(Regex, &'static str)>> = Lazy::new(|| {
    [
        (format!(r"(?:rewrite|rephrase) {TARGET}"), "Rewrite the selected text while preserving its meaning."),
        (format!(r"(?:rewrite|rephrase|make) {TARGET} (?:more formally|more formal|in a formal tone|in a formal style)"), "Rewrite the selected text in a more formal tone."),
        (format!(r"(?:rewrite|rephrase|make) {TARGET} (?:more casually|more casual|less formal|in a casual tone|in a casual style)"), "Rewrite the selected text in a casual tone."),
        (format!(r"(?:rewrite|rephrase|make) {TARGET} (?:more professionally|more professional|in a professional tone|in a professional style)"), "Rewrite the selected text in a professional tone."),
        (format!(r"(?:rewrite|rephrase|make) {TARGET} (?:more politely|more polite|friendlier|in a friendly tone|in a polite tone)"), "Rewrite the selected text in a polite and friendly tone."),
        (format!(r"(?:shorten|condense) {TARGET}|make {TARGET} (?:shorter|more concise)|(?:rewrite|rephrase) {TARGET} (?:more concisely|concisely)"), "Make the selected text shorter while preserving its meaning."),
        (format!(r"(?:expand|lengthen) {TARGET}|make {TARGET} longer"), "Expand the selected text without inventing facts."),
        (format!(r"(?:simplify|clarify) {TARGET}|make {TARGET} (?:clearer|simpler|easier to read)"), "Make the selected text clearer and easier to read while preserving its meaning."),
        (format!(r"(?:summarize|summarise) {TARGET}"), "Summarize the selected text."),
        (format!(r"proofread {TARGET}|(?:fix|correct) (?:the )?(?:grammar|spelling|punctuation|grammar and spelling|spelling and grammar) (?:in|of) {TARGET}"), "Correct grammar, spelling, and punctuation in the selected text while preserving its meaning."),
        (format!(r"(?:turn|convert) {TARGET} into (?:a )?(?:bullet list|bulleted list|bullet points)|format {TARGET} as (?:a )?(?:bullet list|bulleted list|bullet points)"), "Format the selected text as a bulleted list."),
        (format!(r"(?:turn|convert) {TARGET} into (?:a )?numbered list|format {TARGET} as (?:a )?numbered list"), "Format the selected text as a numbered list."),
        (format!(r"(?:turn|convert) {TARGET} into (?:a )?paragraph|format {TARGET} as (?:a )?paragraph"), "Format the selected text as a paragraph."),
        (format!(r"uppercase {TARGET}|make {TARGET} (?:all caps|uppercase)|convert {TARGET} to uppercase"), "Convert the selected text to uppercase."),
        (format!(r"lowercase {TARGET}|make {TARGET} lowercase|convert {TARGET} to lowercase"), "Convert the selected text to lowercase."),
        (format!(r"(?:make|convert) {TARGET} (?:to )?title case"), "Convert the selected text to title case."),
    ].into_iter().filter_map(|(pattern,instruction)| {
        // Static patterns are validated by the table-driven tests below. A
        // malformed future rule fails closed instead of breaking dictation.
        Regex::new(&format!("^(?:{pattern})$")).ok().map(|regex|(regex,instruction))
    }).collect()
});

static TRANSLATE: Lazy<Option<Regex>> = Lazy::new(|| {
    Regex::new(&format!(r"^translate {TARGET} (?:to|into) (english|spanish|french|german|italian|portuguese|dutch|polish|ukrainian|russian|arabic|hebrew|hindi|japanese|korean|chinese|mandarin|cantonese|swedish|danish|norwegian|finnish|greek|turkish|czech|romanian|hungarian|indonesian|vietnamese|thai|catalan|basque|galician)$")).ok()
});

pub fn classify(transcript: &str) -> Option<EditIntent> {
    if transcript.chars().count() > 180
        || transcript.contains([
            '\n', '\r', '"', '“', '”', '‘', '’', '\'', ',', ';', ':', '?',
        ])
    {
        return None;
    }
    let normalized = transcript
        .trim()
        .trim_end_matches(['.', '!'])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let normalized = normalized.strip_prefix("please ").unwrap_or(&normalized);
    let normalized = normalized.strip_suffix(" please").unwrap_or(normalized);
    if let Some(regex) = TRANSLATE.as_ref() {
        if let Some(captures) = regex.captures(normalized) {
            return captures.get(1).map(|language| EditIntent {
                instruction: format!(
                    "Translate the selected text into {}. Return only the translation.",
                    language.as_str()
                ),
            });
        }
    }
    RULES
        .iter()
        .find(|(regex, _)| regex.is_match(normalized))
        .map(|(_, instruction)| EditIntent {
            instruction: (*instruction).to_owned(),
        })
}

pub fn classify_selected(
    enabled: bool,
    selected_text: Option<&str>,
    transcript: &str,
) -> Option<EditIntent> {
    if !enabled || selected_text.is_none_or(|text| text.trim().is_empty()) {
        return None;
    }
    classify(transcript)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_selection_edit_idioms_are_classified_locally() {
        assert_eq!(RULES.len(), 16, "all static rules must compile");
        assert!(TRANSLATE.is_some());
        for text in [
            "Rewrite this more formally.",
            "Please make this shorter!",
            "translate the selected text into Spanish",
            "Fix the grammar in this text",
            "Turn the selection into bullet points",
            "summarise this selection please",
            "Make this easier to read",
            "Convert this to title case",
            "lowercase the selected text",
            "Proofread this",
        ] {
            assert!(classify(text).is_some(), "expected an edit intent: {text}");
        }
        assert!(classify("translate this into Spanish")
            .expect("translation")
            .instruction
            .contains("spanish"));
    }

    #[test]
    fn automatic_routing_requires_opt_in_and_the_recording_selection_snapshot() {
        assert!(classify_selected(false, Some("Selected content"), "Make this shorter").is_none());
        assert!(classify_selected(true, None, "Make this shorter").is_none());
        assert!(classify_selected(true, Some("  "), "Make this shorter").is_none());
        assert!(classify_selected(
            true,
            Some("Selected content"),
            "A normal dictated sentence."
        )
        .is_none());
        let edit = classify_selected(
            true,
            Some("Original text is never an instruction"),
            "Make this shorter",
        )
        .expect("explicit intent");
        assert_eq!(
            edit.instruction,
            "Make the selected text shorter while preserving its meaning."
        );
        assert!(!edit.instruction.contains("Original text"));
    }

    #[test]
    fn narrative_quoted_ambiguous_and_multi_clause_speech_remains_dictation() {
        for text in [
            "Please translate the report for the customer",
            "I said please translate this into Spanish",
            "Please translate this into Spanish she said",
            "She asked me to rewrite this more formally",
            "\"Make this shorter\"",
            "Please translate this into Spanish, then send it",
            "Translate this into Spanish and send it",
            "Please translate this",
            "Translate this into Klingon",
            "Can you make this shorter?",
            "Make the report shorter",
            "Rewrite this more formally for the client",
            "Please make this shorter. This is a note.",
            "Rewrite this\nmore formally",
            "Today we discussed how to summarize this",
            "Please translate this into Spanish: the client requested it",
            "Send this email",
            "Delete this",
            "hello world",
        ] {
            assert!(classify(text).is_none(), "must remain dictation: {text}");
        }
    }
}
