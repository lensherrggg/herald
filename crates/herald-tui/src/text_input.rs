//! Helpers for single-line text fields with a caret. All positions are byte
//! offsets kept on UTF-8 character boundaries.

fn clamp(text: &str, cursor: usize) -> usize {
    let mut at = cursor.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

pub fn prev_cursor(text: &str, cursor: usize) -> usize {
    let at = clamp(text, cursor);
    text[..at]
        .char_indices()
        .next_back()
        .map_or(0, |(index, _)| index)
}

pub fn next_cursor(text: &str, cursor: usize) -> usize {
    let at = clamp(text, cursor);
    text[at..]
        .chars()
        .next()
        .map_or(text.len(), |ch| at + ch.len_utf8())
}

pub fn insert_char(text: &mut String, cursor: &mut usize, ch: char) {
    let at = clamp(text, *cursor);
    text.insert(at, ch);
    *cursor = at + ch.len_utf8();
}

pub fn backspace(text: &mut String, cursor: &mut usize) {
    let at = clamp(text, *cursor);
    if at == 0 {
        *cursor = 0;
        return;
    }
    let prev = prev_cursor(text, at);
    text.replace_range(prev..at, "");
    *cursor = prev;
}

pub fn delete(text: &mut String, cursor: &mut usize) {
    let at = clamp(text, *cursor);
    if at >= text.len() {
        *cursor = at;
        return;
    }
    let next = next_cursor(text, at);
    text.replace_range(at..next, "");
    *cursor = at;
}

pub fn split_at(text: &str, cursor: usize) -> (&str, &str) {
    text.split_at(clamp(text, cursor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_and_edits_on_character_boundaries() {
        let mut text = "中文ab".to_string();
        let mut cursor = text.len();
        cursor = prev_cursor(&text, cursor);
        assert_eq!(&text[cursor..], "b");
        cursor = prev_cursor(&text, cursor);
        assert_eq!(&text[cursor..], "ab");
        cursor = next_cursor(&text, cursor);
        assert_eq!(&text[cursor..], "b");

        cursor = prev_cursor(&text, cursor);
        insert_char(&mut text, &mut cursor, '新');
        assert_eq!(text, "中文新ab");
        assert_eq!(&text[cursor..], "ab");

        backspace(&mut text, &mut cursor);
        assert_eq!(text, "中文ab");
        assert_eq!(&text[cursor..], "ab");

        delete(&mut text, &mut cursor);
        assert_eq!(text, "中文b");
        assert_eq!(cursor, 6);

        cursor = 0;
        backspace(&mut text, &mut cursor);
        assert_eq!(text, "中文b");
        assert_eq!(cursor, 0);
        delete(&mut text, &mut cursor);
        assert_eq!(text, "文b");
    }

    #[test]
    fn split_and_invalid_positions_are_safe() {
        let text = "你好ok";
        let (before, after) = split_at(text, 99);
        assert_eq!(before, text);
        assert_eq!(after, "");
        let (before, after) = split_at(text, 6);
        assert_eq!(before, "你好");
        assert_eq!(after, "ok");
        // Mid-character positions are pulled back to a boundary.
        let cursor = prev_cursor(text, 1);
        assert_eq!(cursor, 0);
    }
}
