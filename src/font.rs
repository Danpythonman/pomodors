//! A tiny 3x5 pixel font for rendering the countdown in large block digits.

const HEIGHT: usize = 5;

fn glyph(c: char) -> [&'static str; HEIGHT] {
    match c {
        '0' => ["###", "# #", "# #", "# #", "###"],
        '1' => [" # ", "## ", " # ", " # ", "###"],
        '2' => ["###", "  #", "###", "#  ", "###"],
        '3' => ["###", "  #", "###", "  #", "###"],
        '4' => ["# #", "# #", "###", "  #", "  #"],
        '5' => ["###", "#  ", "###", "  #", "###"],
        '6' => ["###", "#  ", "###", "# #", "###"],
        '7' => ["###", "  #", "  #", "  #", "  #"],
        '8' => ["###", "# #", "###", "# #", "###"],
        '9' => ["###", "# #", "###", "  #", "###"],
        ':' => [" ", "#", " ", "#", " "],
        _ => [" ", " ", " ", " ", " "],
    }
}

/// Render `text` as rows of block characters. Each pixel is two cells wide so
/// the digits look roughly square in a terminal.
pub fn render(text: &str) -> Vec<String> {
    (0..HEIGHT)
        .map(|row| {
            text.chars()
                .map(|c| {
                    glyph(c)[row]
                        .chars()
                        .map(|p| if p == '#' { "██" } else { "  " })
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("  ")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_have_equal_width() {
        let rows = render("12:34");
        assert_eq!(rows.len(), HEIGHT);
        let width = rows[0].chars().count();
        assert_eq!(width, 4 * 6 + 2 + 4 * 2);
        assert!(rows.iter().all(|r| r.chars().count() == width));
    }
}
