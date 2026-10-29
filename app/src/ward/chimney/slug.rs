//! Task slugs for `claude --remote-control '<project>. <task-slug>'`: kebab-case ASCII from the
//! card title, Cyrillic transliterated, at most [`MAX_SLUG_CHARS`] characters.

pub const MAX_SLUG_CHARS: usize = 30;

fn transliterate(c: char) -> Option<&'static str> {
    let lower = c.to_lowercase().next().unwrap_or(c);
    Some(match lower {
        'а' => "a",
        'б' => "b",
        'в' => "v",
        'г' => "g",
        'д' => "d",
        'е' | 'ё' | 'э' => "e",
        'ж' => "zh",
        'з' => "z",
        'и' | 'й' => "i",
        'к' => "k",
        'л' => "l",
        'м' => "m",
        'н' => "n",
        'о' => "o",
        'п' => "p",
        'р' => "r",
        'с' => "s",
        'т' => "t",
        'у' => "u",
        'ф' => "f",
        'х' => "kh",
        'ц' => "ts",
        'ч' => "ch",
        'ш' => "sh",
        'щ' => "shch",
        'ъ' | 'ь' => "",
        'ы' => "y",
        'ю' => "yu",
        'я' => "ya",
        _ => return None,
    })
}

/// Kebab-case ASCII of `text`, cut at a word boundary to at most `max` characters. Empty when
/// nothing in the text can be written in ASCII.
pub fn slugify(text: &str, max: usize) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            current.push(c.to_ascii_lowercase());
        } else if let Some(latin) = transliterate(c) {
            current.push_str(latin);
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }

    let mut slug = String::new();
    for word in words {
        let needed = if slug.is_empty() { word.len() } else { word.len() + 1 };
        if slug.len() + needed > max {
            if slug.is_empty() {
                // A single word longer than the limit is cut.
                slug = word.chars().take(max).collect();
            }
            break;
        }
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(&word);
    }
    slug
}

/// The task part of the session name: the title slug, or the card id when the title has no
/// usable characters.
pub fn task_slug(title: &str, task_id: u64) -> String {
    let slug = slugify(title, MAX_SLUG_CHARS);
    if slug.is_empty() {
        format!("card-{task_id}")
    } else {
        slug
    }
}

/// `claude --remote-control '<project>. <task-slug>'`. Both parts are ASCII without quotes, so
/// the single-quoted argument works in PowerShell and POSIX shells alike.
pub fn claude_command(project: &str, task_slug: &str) -> String {
    format!("claude --remote-control '{project}. {task_slug}'")
}

#[cfg(test)]
#[path = "slug_tests.rs"]
mod tests;
