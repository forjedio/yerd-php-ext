//! Bounded, panic-safe rendering of a [`Zval`] to text + HTML for `dump` frames.
//!
//! Never recurses without a depth bound and never copies unbounded user data:
//! strings, array breadth, and nesting depth are all capped. Output is a
//! `print_r`-flavoured text plus an HTML-escaped `<pre>` block.

use ext_php_rs::types::Zval;

const MAX_DEPTH: usize = 4;
const MAX_ITEMS: usize = 100;
const MAX_STR: usize = 8 * 1024;

/// Render a value to `(value_html, value_text)`.
#[must_use]
pub fn render(z: &Zval) -> (String, String) {
    let mut text = String::new();
    render_into(z, 0, &mut text);
    let html = format!("<pre>{}</pre>", html_escape(&text));
    (html, text)
}

fn render_into(z: &Zval, depth: usize, out: &mut String) {
    let z = z.dereference();
    if z.is_null() {
        out.push_str("null");
    } else if let Some(b) = z.bool() {
        out.push_str(if b { "true" } else { "false" });
    } else if let Some(n) = z.long() {
        out.push_str(&n.to_string());
    } else if let Some(f) = z.double() {
        out.push_str(&f.to_string());
    } else if let Some(s) = z.str() {
        out.push('"');
        out.push_str(&crate::frame::truncate(s, MAX_STR));
        out.push('"');
    } else if let Some(arr) = z.array() {
        if depth >= MAX_DEPTH {
            out.push_str("array(…)");
            return;
        }
        out.push_str("[\n");
        let pad = "  ".repeat(depth + 1);
        for (i, (key, val)) in arr.iter().enumerate() {
            if i >= MAX_ITEMS {
                out.push_str(&pad);
                out.push_str("…\n");
                break;
            }
            out.push_str(&pad);
            out.push_str(&key.to_string());
            out.push_str(" => ");
            render_into(val, depth + 1, out);
            out.push('\n');
        }
        out.push_str(&"  ".repeat(depth));
        out.push(']');
    } else if let Some(obj) = z.object() {
        let class = obj.get_class_name().unwrap_or_else(|_| "object".to_owned());
        out.push_str(&class);
        out.push_str(" {…}");
    } else {
        out.push_str("(unknown)");
    }
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::html_escape;

    #[test]
    fn escapes_html_metacharacters() {
        assert_eq!(
            html_escape(r#"<a href="x">&'"#),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;"
        );
    }
}
