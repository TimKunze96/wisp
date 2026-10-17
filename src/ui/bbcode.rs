//! TeamSpeak's BBCode (channel descriptions, chat) as styled, wrapping text
//! with clickable links. Supported: b, i, u, s, color, url, img (as a link),
//! list items; everything else is dropped. Bare http(s) links are linked too.

use std::ops::Range;

use gpui_kit::{
    ElementId, FontStyle, FontWeight, HighlightStyle, InteractiveText, StrikethroughStyle, StyledText,
    UnderlineStyle, px, rgb,
};

use super::widgets::ACCENT;

#[derive(Clone, Default, PartialEq)]
struct Style {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    color: Option<u32>,
    link: Option<String>,
}

pub struct Rich {
    pub text: String,
    spans: Vec<(Range<usize>, Style)>,
    links: Vec<(Range<usize>, String)>,
}

fn named_color(s: &str) -> Option<u32> {
    let s = s.trim().trim_matches('"').to_lowercase();
    if let Some(hex) = s.strip_prefix('#') {
        return match hex.len() {
            6 => u32::from_str_radix(hex, 16).ok(),
            3 => {
                let v = u32::from_str_radix(hex, 16).ok()?;
                let (r, g, b) = ((v >> 8) & 0xf, (v >> 4) & 0xf, v & 0xf);
                Some((r * 17) << 16 | (g * 17) << 8 | (b * 17))
            }
            _ => None,
        };
    }
    Some(match s.as_str() {
        "red" => 0xe06c75,
        "green" => 0x5fc98a,
        "blue" => 0x6ca0e0,
        "yellow" => 0xe5c07b,
        "orange" => 0xe0a060,
        "purple" | "violet" => 0xc678dd,
        "cyan" | "aqua" => 0x56b6c2,
        "white" => 0xffffff,
        "black" => 0x8b9098, // unreadable on our background
        "gray" | "grey" => 0x8b9098,
        "pink" => 0xe88bb4,
        "lime" => 0x9ad86a,
        "gold" => 0xd8b26a,
        "maroon" | "darkred" => 0xc05050,
        "navy" | "darkblue" => 0x6c8ad0,
        "teal" => 0x4fb0a0,
        "brown" => 0xb08060,
        _ => return None,
    })
}

/// Tags we understand: (name, argument).
fn tag(raw: &str) -> Option<(bool, String, String)> {
    let raw = raw.trim();
    let (closing, raw) = match raw.strip_prefix('/') {
        Some(r) => (true, r),
        None => (false, raw),
    };
    let (name, arg) = raw.split_once('=').map_or((raw, ""), |(n, a)| (n, a));
    let name = name.trim().to_lowercase();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '*') {
        return None;
    }
    Some((closing, name, arg.trim().trim_matches('"').to_string()))
}

fn push(text: &mut String, spans: &mut Vec<(Range<usize>, Style)>, s: &str, style: &Style) {
    if s.is_empty() {
        return;
    }
    let start = text.len();
    text.push_str(s);
    spans.push((start..text.len(), style.clone()));
}

pub fn parse(input: &str) -> Rich {
    let mut text = String::with_capacity(input.len());
    let mut spans: Vec<(Range<usize>, Style)> = Vec::new();
    let mut stack: Vec<(String, Style)> = Vec::new();
    let mut style = Style::default();
    // a [url] without argument: the link is the text inside it
    let mut url_text_start: Option<usize> = None;
    let mut rest = input;
    while !rest.is_empty() {
        let Some(open) = rest.find('[') else {
            push(&mut text, &mut spans, rest, &style);
            break;
        };
        push(&mut text, &mut spans, &rest[..open], &style);
        rest = &rest[open..];
        let Some(close) = rest.find(']') else {
            push(&mut text, &mut spans, rest, &style);
            break;
        };
        let inner = &rest[1..close];
        let Some((closing, name, arg)) = tag(inner) else {
            push(&mut text, &mut spans, &rest[..=close], &style);
            rest = &rest[close + 1..];
            continue;
        };
        rest = &rest[close + 1..];
        if closing {
            if let Some(pos) = stack.iter().rposition(|(n, _)| *n == name) {
                if name == "url" {
                    if let Some(start) = url_text_start.take() {
                        let link = text[start..].trim().to_string();
                        for (_, s) in spans.iter_mut().filter(|(r, _)| r.start >= start) {
                            s.link = Some(link.clone());
                        }
                    }
                }
                style = stack[pos].1.clone();
                stack.truncate(pos);
            }
            continue;
        }
        let saved = style.clone();
        let mut opened = true;
        match name.as_str() {
            "b" => style.bold = true,
            "i" => style.italic = true,
            "u" => style.underline = true,
            "s" => style.strike = true,
            "color" => style.color = named_color(&arg).or(style.color),
            "url" => {
                if arg.is_empty() {
                    url_text_start = Some(text.len());
                    style.link = Some(String::new());
                } else {
                    style.link = Some(arg.clone());
                }
            }
            "img" => {
                // show the address as a link instead of loading the image
                let end = rest.to_lowercase().find("[/img]").unwrap_or(rest.len());
                let url = rest[..end].trim().to_string();
                rest = &rest[(end + 6).min(rest.len())..];
                let mut s = style.clone();
                s.link = Some(url.clone());
                push(&mut text, &mut spans, &url, &s);
                opened = false;
            }
            "*" => {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str("• ");
                opened = false;
            }
            "hr" => {
                text.push_str("\n――――――――\n");
                opened = false;
            }
            _ => {} // size, center, left, right, list, table...: keep the text only
        }
        if opened {
            stack.push((name, saved));
        }
    }
    let mut rich = Rich { text, spans, links: Vec::new() };
    rich.autolink();
    rich.collect_links();
    rich
}

impl Rich {
    /// Link bare http(s) addresses that aren't links already.
    fn autolink(&mut self) {
        let mut found = Vec::new();
        let mut from = 0;
        while let Some(i) = self.text[from..].find("http").map(|i| i + from) {
            let rest = &self.text[i..];
            if !(rest.starts_with("http://") || rest.starts_with("https://")) {
                from = i + 4;
                continue;
            }
            let end = rest.find(|c: char| c.is_whitespace() || c == '"' || c == '<' || c == '>').unwrap_or(rest.len());
            let url = rest[..end].trim_end_matches(['.', ',', ')', '!', '?']);
            found.push((i..i + url.len(), url.to_string()));
            from = i + end.max(1);
        }
        for (range, url) in found {
            if self.spans.iter().any(|(r, s)| s.link.is_some() && r.start < range.end && range.start < r.end) {
                continue;
            }
            // split the spans so the link range has its own runs
            let mut out = Vec::with_capacity(self.spans.len() + 2);
            for (r, s) in self.spans.drain(..) {
                if r.end <= range.start || r.start >= range.end {
                    out.push((r, s));
                    continue;
                }
                if r.start < range.start {
                    out.push((r.start..range.start, s.clone()));
                }
                let mut l = s.clone();
                l.link = Some(url.clone());
                out.push((r.start.max(range.start)..r.end.min(range.end), l));
                if r.end > range.end {
                    out.push((range.end..r.end, s));
                }
            }
            self.spans = out;
        }
    }

    fn collect_links(&mut self) {
        for (r, s) in &self.spans {
            if let Some(url) = s.link.as_ref().filter(|u| !u.is_empty()) {
                match self.links.last_mut() {
                    Some((last, u)) if u == url && last.end == r.start => last.end = r.end,
                    _ => self.links.push((r.clone(), url.clone())),
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// Wrapping text element; links open in the browser.
    pub fn element(self, id: impl Into<ElementId>) -> InteractiveText {
        let highlights: Vec<(Range<usize>, HighlightStyle)> = self
            .spans
            .iter()
            .filter(|(_, s)| *s != Style::default())
            .map(|(r, s)| {
                let color = if s.link.is_some() { Some(ACCENT) } else { s.color };
                (
                    r.clone(),
                    HighlightStyle {
                        color: color.map(|c| rgb(c).into()),
                        font_weight: s.bold.then_some(FontWeight::SEMIBOLD),
                        font_style: s.italic.then_some(FontStyle::Italic),
                        underline: (s.underline || s.link.is_some())
                            .then(|| UnderlineStyle { thickness: px(1.), color: None, wavy: false }),
                        strikethrough: s.strike.then(|| StrikethroughStyle { thickness: px(1.), color: None }),
                        ..Default::default()
                    },
                )
            })
            .collect();
        let (ranges, urls): (Vec<_>, Vec<_>) = self.links.into_iter().unzip();
        InteractiveText::new(id, StyledText::new(self.text).with_highlights(highlights))
            .on_click(ranges, move |i, _, cx| cx.open_url(&urls[i]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_and_links() {
        let r = parse("[b]Hi[/b] [url=https://a.b]there[/url] [color=red]x[/color] see https://c.d/e.");
        assert_eq!(r.text, "Hi there x see https://c.d/e.");
        assert_eq!(r.links.len(), 2);
        assert_eq!(&r.text[r.links[0].0.clone()], "there");
        assert_eq!(r.links[1].1, "https://c.d/e");
    }

    #[test]
    fn bare_url_tag_and_unknown() {
        let r = parse("[center][url]https://x.y[/url][/center] [notatag");
        assert_eq!(r.text, "https://x.y [notatag");
        assert_eq!(r.links[0].1, "https://x.y");
    }

    #[test]
    fn list_items() {
        assert_eq!(parse("[list][*]a[*]b[/list]").text, "• a\n• b");
    }
}
