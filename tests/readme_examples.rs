//! Every `yuki …` example in the README must parse against the real clap
//! definitions, so the docs cannot drift from the CLI again.

use clap::Parser;
use yuki_cli::cli::Cli;

/// Split a shell line into words, honouring double quotes.
fn shell_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut has_word = false;
    for c in line.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                has_word = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if has_word {
                    words.push(std::mem::take(&mut current));
                    has_word = false;
                }
            }
            c => {
                current.push(c);
                has_word = true;
            }
        }
    }
    if has_word {
        words.push(current);
    }
    words
}

/// Turn a README example into argv: drop the trailing comment, fill `<placeholders>`,
/// and unwrap `[--optional]` flags.
fn example_args(example: &str) -> Vec<String> {
    let without_comment = match example.find(" #") {
        Some(i) => &example[..i],
        None => example,
    };
    shell_words(without_comment)
        .into_iter()
        .map(|w| {
            let w = w.trim_start_matches('[').trim_end_matches(']').to_string();
            if w.starts_with('<') && w.ends_with('>') {
                "placeholder".to_string()
            } else {
                w
            }
        })
        .collect()
}

/// Collect the `yuki …` lines from the README's `sh` code blocks, joining `\` continuations.
fn readme_examples() -> Vec<String> {
    let readme = include_str!("../README.md");
    let mut examples = Vec::new();
    let mut in_sh = false;
    let mut pending = String::new();
    for line in readme.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_sh = trimmed == "```sh";
            continue;
        }
        if !in_sh {
            continue;
        }
        if !pending.is_empty() {
            pending.push(' ');
            pending.push_str(trimmed);
        } else if trimmed.starts_with("yuki ") {
            pending.push_str(trimmed);
        } else {
            continue;
        }
        if let Some(stripped) = pending.strip_suffix('\\') {
            pending = stripped.trim_end().to_string();
        } else {
            examples.push(std::mem::take(&mut pending));
        }
    }
    examples
}

#[test]
fn readme_has_examples() {
    assert!(readme_examples().len() > 20, "README examples not found");
}

#[test]
fn every_readme_example_parses() {
    let failures: Vec<String> = readme_examples()
        .iter()
        .filter_map(|example| {
            Cli::try_parse_from(example_args(example))
                .err()
                .map(|e| format!("{example}\n  -> {}", e.kind()))
        })
        .collect();
    assert!(
        failures.is_empty(),
        "README examples that do not parse:\n{}",
        failures.join("\n")
    );
}
