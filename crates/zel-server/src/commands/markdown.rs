// Copyright 2026 The Bazel Authors. All rights reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//    http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! A CommonMark renderer for `Markdown.renderToHtml` (commonmark-java `Parser.builder().build()` and
//! `HtmlRenderer.builder().escapeHtml(true)`), as used by `help everything-as-html` for option help
//! texts: paragraphs, ATX/setext headings, fenced/indented code, block quotes, bullet/ordered lists
//! (tight/loose), thematic breaks; inline code spans, emphasis, links (inline, reference, auto),
//! backslash escapes and hard/soft line breaks. Raw HTML is escaped (`escapeHtml(true)`), which for
//! tags and text coincides with escaping `<`, `>`, `&` and `"`.

use std::collections::HashMap;

/// `Markdown.renderToHtml`: renders and trims.
pub fn render_to_html(md: &str) -> String {
    let lines: Vec<String> = md.replace("\r\n", "\n").replace('\r', "\n").split('\n').map(expand_tabs).collect();
    let (blocks, _) = parse_blocks(&lines);
    let refs = collect_refs(&lines);
    let mut out = String::new();
    render_blocks(&blocks, &mut out, false, &refs);
    out.trim().to_string()
}

fn expand_tabs(l: &str) -> String {
    if !l.contains('\t') {
        return l.to_string();
    }
    let mut out = String::new();
    let mut col = 0;
    for c in l.chars() {
        if c == '\t' {
            let n = 4 - col % 4;
            out.push_str(&" ".repeat(n));
            col += n;
        } else {
            out.push(c);
            col += 1;
        }
    }
    out
}

pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

#[derive(Debug, Clone)]
enum Block {
    Para(String),
    Heading(usize, String),
    Code(Option<String>, String),
    Quote(Vec<Block>),
    List { ordered: bool, start: u32, tight: bool, items: Vec<Vec<Block>> },
    Hr,
}

fn indent_of(l: &str) -> usize {
    l.len() - l.trim_start_matches(' ').len()
}

fn is_blank(l: &str) -> bool {
    l.trim().is_empty()
}

fn is_hr(l: &str) -> bool {
    if indent_of(l) > 3 {
        return false;
    }
    let t: String = l.chars().filter(|c| *c != ' ').collect();
    t.len() >= 3 && ["-", "*", "_"].iter().any(|m| t.chars().all(|c| m.starts_with(c)))
}

fn atx_heading(l: &str) -> Option<(usize, String)> {
    if indent_of(l) > 3 {
        return None;
    }
    let t = l.trim_start();
    let n = t.chars().take_while(|c| *c == '#').count();
    if n == 0 || n > 6 {
        return None;
    }
    let rest = &t[n..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    let mut c = rest.trim().to_string();
    let stripped = c.trim_end_matches('#');
    if stripped.len() != c.len() && (stripped.is_empty() || stripped.ends_with(' ')) {
        c = stripped.trim().to_string();
    }
    Some((n, c))
}

fn fence_start(l: &str) -> Option<(char, usize, usize, String)> {
    let ind = indent_of(l);
    if ind > 3 {
        return None;
    }
    let t = &l[ind..];
    let c = t.chars().next()?;
    if c != '`' && c != '~' {
        return None;
    }
    let n = t.chars().take_while(|x| *x == c).count();
    if n < 3 {
        return None;
    }
    let info = t[n..].trim().to_string();
    if c == '`' && info.contains('`') {
        return None;
    }
    Some((c, n, ind, info))
}

/// `(ordered, bullet char / delimiter, start number, content offset, rest-of-line)`.
fn list_marker(l: &str) -> Option<(bool, char, u32, usize, String)> {
    let ind = indent_of(l);
    if ind > 3 {
        return None;
    }
    let t = &l[ind..];
    let mut chars = t.chars();
    let c = chars.next()?;
    let (ordered, delim, start, mlen) = if matches!(c, '-' | '+' | '*') {
        (false, c, 0, 1)
    } else if c.is_ascii_digit() {
        let digits: String = t.chars().take_while(|x| x.is_ascii_digit()).collect();
        if digits.len() > 9 {
            return None;
        }
        let d = t[digits.len()..].chars().next()?;
        if d != '.' && d != ')' {
            return None;
        }
        (true, d, digits.parse().ok()?, digits.len() + 1)
    } else {
        return None;
    };
    let after = &t[mlen..];
    if !after.is_empty() && !after.starts_with(' ') {
        return None;
    }
    let spaces = after.len() - after.trim_start_matches(' ').len();
    let rest = after.trim_start_matches(' ');
    let (offset, content) = if rest.is_empty() {
        (ind + mlen + 1, String::new())
    } else if spaces > 4 {
        (ind + mlen + 1, after[1..].to_string())
    } else {
        (ind + mlen + spaces, rest.to_string())
    };
    Some((ordered, delim, start, offset, content))
}

fn can_interrupt_paragraph(l: &str) -> bool {
    if is_blank(l) {
        return true;
    }
    if indent_of(l) > 3 {
        return false;
    }
    if fence_start(l).is_some() || atx_heading(l).is_some() || is_hr(l) || l.trim_start().starts_with('>') {
        return true;
    }
    if let Some((ordered, _, start, _, content)) = list_marker(l) {
        return !content.is_empty() && (!ordered || start == 1);
    }
    false
}

fn setext_level(l: &str) -> Option<usize> {
    if indent_of(l) > 3 {
        return None;
    }
    let t = l.trim();
    if !t.is_empty() && t.chars().all(|c| c == '=') {
        Some(1)
    } else if !t.is_empty() && t.chars().all(|c| c == '-') {
        Some(2)
    } else {
        None
    }
}

/// Parses `lines` into blocks; the second result flags a blank-line gap after each block.
fn parse_blocks(lines: &[String]) -> (Vec<Block>, Vec<bool>) {
    let mut blocks = Vec::new();
    let mut gaps: Vec<bool> = Vec::new();
    let mut i = 0;
    let mut pending_blank = false;
    while i < lines.len() {
        let l = &lines[i];
        if is_blank(l) {
            pending_blank = true;
            i += 1;
            continue;
        }
        if pending_blank && !gaps.is_empty() {
            *gaps.last_mut().unwrap() = true;
        }
        pending_blank = false;
        let block;
        if indent_of(l) >= 4 {
            let mut code: Vec<String> = Vec::new();
            while i < lines.len() && (is_blank(&lines[i]) || indent_of(&lines[i]) >= 4) {
                code.push(if lines[i].len() >= 4 { lines[i][4..].to_string() } else { String::new() });
                i += 1;
            }
            while code.last().is_some_and(|c| c.trim().is_empty()) {
                code.pop();
                // blank lines after the code belong to the gap
            }
            let mut text = code.join("\n");
            text.push('\n');
            block = Block::Code(None, text);
            pending_blank = lines[i - 1..i].iter().any(|x| is_blank(x));
        } else if let Some((fc, fnn, find, info)) = fence_start(l) {
            i += 1;
            let mut code = String::new();
            while i < lines.len() {
                let t = lines[i].trim_start_matches(' ');
                let closing = indent_of(&lines[i]) <= 3
                    && t.starts_with(fc)
                    && t.chars().take_while(|x| *x == fc).count() >= fnn
                    && t[t.chars().take_while(|x| *x == fc).count()..].trim().is_empty();
                if closing {
                    i += 1;
                    break;
                }
                let strip = indent_of(&lines[i]).min(find);
                code.push_str(&lines[i][strip..]);
                code.push('\n');
                i += 1;
            }
            let lang = info.split_whitespace().next().map(|s| s.to_string());
            block = Block::Code(lang, code);
        } else if let Some((n, text)) = atx_heading(l) {
            i += 1;
            block = Block::Heading(n, text);
        } else if is_hr(l) {
            i += 1;
            block = Block::Hr;
        } else if l.trim_start().starts_with('>') && indent_of(l) <= 3 {
            let mut inner: Vec<String> = Vec::new();
            while i < lines.len() {
                let t = &lines[i];
                if indent_of(t) <= 3 && t.trim_start().starts_with('>') {
                    let r = &t.trim_start()[1..];
                    inner.push(r.strip_prefix(' ').unwrap_or(r).to_string());
                    i += 1;
                } else if !is_blank(t) && !inner.is_empty() && !is_blank(inner.last().unwrap()) && !can_interrupt_paragraph(t) {
                    inner.push(t.trim_start().to_string());
                    i += 1;
                } else {
                    break;
                }
            }
            block = Block::Quote(parse_blocks(&inner).0);
        } else if let Some((ordered, delim, start, _, _)) = list_marker(l) {
            let mut items: Vec<Vec<Block>> = Vec::new();
            let mut item_loose = false;
            let mut between_blank = false;
            loop {
                let Some((o, d, _, offset, first)) = list_marker(&lines[i]) else { break };
                if o != ordered || d != delim {
                    break;
                }
                let mut item_lines: Vec<String> = vec![first];
                i += 1;
                let mut in_fence: Option<(char, usize)> = None;
                if let Some((fc, fnn, _, _)) = fence_start(&item_lines[0]) {
                    in_fence = Some((fc, fnn));
                }
                while i < lines.len() {
                    let t = &lines[i];
                    if is_blank(t) {
                        item_lines.push(String::new());
                        i += 1;
                    } else if indent_of(t) >= offset {
                        let s = &t[offset..];
                        if let Some((fc, fnn)) = in_fence {
                            if s.trim_start().starts_with(fc) && s.trim_start().chars().take_while(|x| *x == fc).count() >= fnn {
                                in_fence = None;
                            }
                        } else if let Some((fc, fnn, _, _)) = fence_start(s) {
                            in_fence = Some((fc, fnn));
                        }
                        item_lines.push(s.to_string());
                        i += 1;
                    } else if item_lines.last().is_some_and(|x| !is_blank(x)) && !can_interrupt_paragraph(t) && list_marker(t).is_none() {
                        item_lines.push(t.trim_start().to_string());
                        i += 1;
                    } else {
                        break;
                    }
                }
                let trailing_blank = item_lines.last().is_some_and(|x| x.is_empty()) && i < lines.len();
                while item_lines.last().is_some_and(|x| x.is_empty()) {
                    item_lines.pop();
                }
                let (sub, gaps) = parse_blocks(&item_lines);
                if gaps.iter().take(gaps.len().saturating_sub(1)).any(|g| *g) {
                    item_loose = true;
                }
                items.push(sub);
                if trailing_blank && i < lines.len() && list_marker(&lines[i]).is_some_and(|(o2, d2, ..)| o2 == ordered && d2 == delim) {
                    between_blank = true;
                }
                if i >= lines.len() {
                    break;
                }
            }
            block = Block::List { ordered, start, tight: !(item_loose || between_blank), items };
        } else if !collect_refs(std::slice::from_ref(l)).is_empty() {
            // Link reference definitions produce no output.
            i += 1;
            continue;
        } else {
            let mut para: Vec<String> = vec![l.trim().to_string()];
            i += 1;
            let mut heading = None;
            while i < lines.len() {
                if let Some(level) = setext_level(&lines[i]) {
                    heading = Some(level);
                    i += 1;
                    break;
                }
                if can_interrupt_paragraph(&lines[i]) {
                    break;
                }
                para.push(lines[i].trim_start().to_string());
                i += 1;
            }
            block = match heading {
                Some(level) => Block::Heading(level, para.join("\n")),
                None => Block::Para(para.join("\n")),
            };
        }
        blocks.push(block);
        gaps.push(false);
    }
    (blocks, gaps)
}

fn render_blocks(blocks: &[Block], out: &mut String, tight: bool, refs: &HashMap<String, (String, Option<String>)>) {
    for b in blocks {
        match b {
            Block::Para(t) => {
                if tight {
                    out.push_str(&render_inline(t, refs));
                } else {
                    out.push_str("<p>");
                    out.push_str(&render_inline(t, refs));
                    out.push_str("</p>\n");
                }
            }
            Block::Heading(n, t) => out.push_str(&format!("<h{n}>{}</h{n}>\n", render_inline(t, refs))),
            Block::Code(lang, text) => {
                match lang {
                    Some(l) => out.push_str(&format!("<pre><code class=\"language-{}\">", escape(l))),
                    None => out.push_str("<pre><code>"),
                }
                out.push_str(&escape(text));
                out.push_str("</code></pre>\n");
            }
            Block::Quote(inner) => {
                out.push_str("<blockquote>\n");
                render_blocks(inner, out, false, refs);
                out.push_str("</blockquote>\n");
            }
            Block::Hr => out.push_str("<hr />\n"),
            Block::List { ordered, start, tight, items } => {
                let tag = if *ordered { "ol" } else { "ul" };
                if *ordered && *start != 1 {
                    out.push_str(&format!("<ol start=\"{start}\">\n"));
                } else {
                    out.push_str(&format!("<{tag}>\n"));
                }
                for item in items {
                    out.push_str("<li>");
                    if !*tight && !item.is_empty() {
                        out.push('\n');
                    }
                    let mut inner = String::new();
                    render_blocks(item, &mut inner, *tight, refs);
                    // Tight items: consecutive paragraphs/blocks are separated by a newline.
                    out.push_str(&inner);
                    if *tight && !item.is_empty() && !matches!(item.last(), Some(Block::Para(_))) {
                        // block-level child already ends with a newline
                    }
                    out.push_str("</li>\n");
                }
                out.push_str(&format!("</{tag}>\n"));
            }
        }
    }
}

fn collect_refs(lines: &[String]) -> HashMap<String, (String, Option<String>)> {
    let mut refs = HashMap::new();
    for l in lines {
        let t = l.trim_start();
        if indent_of(l) > 3 || !t.starts_with('[') {
            continue;
        }
        let Some(close) = t.find("]:") else { continue };
        let label = normalize_label(&t[1..close]);
        if label.is_empty() {
            continue;
        }
        let rest = t[close + 2..].trim();
        let (dest, title) = match rest.split_once(char::is_whitespace) {
            Some((d, tl)) => {
                let tl = tl.trim();
                let unq = tl
                    .strip_prefix('"')
                    .and_then(|x| x.strip_suffix('"'))
                    .or_else(|| tl.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')))
                    .or_else(|| tl.strip_prefix('(').and_then(|x| x.strip_suffix(')')));
                match unq {
                    Some(u) => (d, Some(u.to_string())),
                    None => continue,
                }
            }
            None => (rest, None),
        };
        if dest.is_empty() {
            continue;
        }
        let dest = dest.trim_start_matches('<').trim_end_matches('>');
        refs.entry(label).or_insert((dest.to_string(), title));
    }
    refs
}

fn normalize_label(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

// ------------------------------------------------------------------ inline

#[derive(Debug, Clone)]
enum Node {
    Text(String),
    Raw(String),
    Code(String),
    Soft,
    Hard,
    Delim { ch: char, count: usize, can_open: bool, can_close: bool, orig: usize },
    Em(Vec<Node>),
    Strong(Vec<Node>),
    Link { children: Vec<Node>, dest: String, title: Option<String> },
    Open(char),
}

fn is_punct(c: char) -> bool {
    c.is_ascii_punctuation() || (!c.is_alphanumeric() && !c.is_whitespace() && !c.is_control())
}

fn render_inline(text: &str, refs: &HashMap<String, (String, Option<String>)>) -> String {
    let nodes = parse_inline(text, refs);
    let mut out = String::new();
    emit(&nodes, &mut out);
    out
}

fn parse_inline(text: &str, refs: &HashMap<String, (String, Option<String>)>) -> Vec<Node> {
    let chars: Vec<char> = text.chars().collect();
    let mut nodes: Vec<Node> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;
    macro_rules! flush {
        () => {
            if !buf.is_empty() {
                nodes.push(Node::Text(std::mem::take(&mut buf)));
            }
        };
    }
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' if i + 1 < chars.len() && chars[i + 1] == '\n' => {
                flush!();
                nodes.push(Node::Hard);
                i += 2;
            }
            '\\' if i + 1 < chars.len() && chars[i + 1].is_ascii_punctuation() => {
                buf.push(chars[i + 1]);
                i += 2;
            }
            '\n' => {
                let trailing = buf.len() - buf.trim_end_matches(' ').len();
                let hard = trailing >= 2;
                while buf.ends_with(' ') {
                    buf.pop();
                }
                flush!();
                nodes.push(if hard { Node::Hard } else { Node::Soft });
                i += 1;
                while i < chars.len() && chars[i] == ' ' {
                    i += 1;
                }
            }
            '`' => {
                let n = chars[i..].iter().take_while(|x| **x == '`').count();
                let mut j = i + n;
                let mut found = None;
                while j < chars.len() {
                    if chars[j] == '`' {
                        let m = chars[j..].iter().take_while(|x| **x == '`').count();
                        if m == n {
                            found = Some(j);
                            break;
                        }
                        j += m;
                    } else {
                        j += 1;
                    }
                }
                match found {
                    Some(end) => {
                        flush!();
                        let mut content: String = chars[i + n..end].iter().collect::<String>().replace('\n', " ");
                        if content.starts_with(' ') && content.ends_with(' ') && !content.trim().is_empty() {
                            content = content[1..content.len() - 1].to_string();
                        }
                        nodes.push(Node::Code(content));
                        i = end + n;
                    }
                    None => {
                        buf.extend(std::iter::repeat('`').take(n));
                        i += n;
                    }
                }
            }
            '<' => {
                let rest: String = chars[i..].iter().collect();
                if let Some(len) = autolink_len(&rest) {
                    flush!();
                    let url: String = chars[i + 1..i + len - 1].iter().collect();
                    nodes.push(Node::Link { children: vec![Node::Text(url.clone())], dest: url, title: None });
                    i += len;
                } else if let Some(len) = html_inline_len(&rest) {
                    flush!();
                    nodes.push(Node::Raw(chars[i..i + len].iter().collect()));
                    i += len;
                } else {
                    buf.push('<');
                    i += 1;
                }
            }
            '*' | '_' => {
                let n = chars[i..].iter().take_while(|x| **x == c).count();
                let before = if i == 0 { ' ' } else { chars[i - 1] };
                let after = chars.get(i + n).copied().unwrap_or(' ');
                let left = !after.is_whitespace() && (!is_punct(after) || before.is_whitespace() || is_punct(before));
                let right = !before.is_whitespace() && (!is_punct(before) || after.is_whitespace() || is_punct(after));
                let (can_open, can_close) = if c == '*' {
                    (left, right)
                } else {
                    (left && (!right || is_punct(before)), right && (!left || is_punct(after)))
                };
                flush!();
                nodes.push(Node::Delim { ch: c, count: n, can_open, can_close, orig: n });
                i += n;
            }
            '[' => {
                flush!();
                nodes.push(Node::Open('['));
                i += 1;
            }
            ']' => {
                flush!();
                // find the nearest unresolved opener
                let opener = nodes.iter().rposition(|n| matches!(n, Node::Open('[')));
                match opener {
                    None => {
                        buf.push(']');
                        i += 1;
                    }
                    Some(oi) => {
                        let label_text: String = chars[..i].iter().collect::<String>();
                        let _ = label_text;
                        let rest: Vec<char> = chars[i + 1..].to_vec();
                        let mut consumed = 0usize;
                        let mut dest = None;
                        if rest.first() == Some(&'(') {
                            if let Some((d, t, len)) = parse_inline_dest(&rest) {
                                dest = Some((d, t));
                                consumed = len;
                            }
                        }
                        if dest.is_none() {
                            // reference link: [text][label], [text][] or [text]
                            let inner_src = source_of(&nodes[oi + 1..]);
                            let (label, len) = if rest.first() == Some(&'[') {
                                match rest.iter().position(|x| *x == ']') {
                                    Some(e) => {
                                        let l: String = rest[1..e].iter().collect();
                                        (if l.trim().is_empty() { inner_src.clone() } else { l }, e + 1)
                                    }
                                    None => (inner_src.clone(), 0),
                                }
                            } else {
                                (inner_src.clone(), 0)
                            };
                            if let Some((d, t)) = refs.get(&normalize_label(&label)) {
                                dest = Some((d.clone(), t.clone()));
                                consumed = len;
                            }
                        }
                        match dest {
                            Some((d, t)) => {
                                let children: Vec<Node> = nodes.drain(oi + 1..).collect();
                                nodes.pop(); // the opener
                                let children = process_emphasis(children);
                                nodes.push(Node::Link { children, dest: d, title: t });
                                i += 1 + consumed;
                            }
                            None => {
                                nodes[oi] = Node::Text("[".to_string());
                                buf.push(']');
                                i += 1;
                            }
                        }
                    }
                }
            }
            _ => {
                buf.push(c);
                i += 1;
            }
        }
    }
    flush!();
    for n in nodes.iter_mut() {
        if let Node::Open(_) = n {
            *n = Node::Text("[".to_string());
        }
    }
    process_emphasis(nodes)
}

/// Raw source approximation of nodes (for reference-link labels).
fn source_of(nodes: &[Node]) -> String {
    let mut s = String::new();
    for n in nodes {
        match n {
            Node::Text(t) | Node::Raw(t) => s.push_str(t),
            Node::Code(c) => {
                s.push('`');
                s.push_str(c);
                s.push('`');
            }
            Node::Soft => s.push('\n'),
            Node::Delim { ch, orig, .. } => s.extend(std::iter::repeat(*ch).take(*orig)),
            Node::Open(c) => s.push(*c),
            _ => {}
        }
    }
    s
}

fn autolink_len(s: &str) -> Option<usize> {
    let end = s.find('>')?;
    let inner = &s[1..end];
    if inner.is_empty() || inner.contains(char::is_whitespace) || inner.contains('<') {
        return None;
    }
    let colon = inner.find(':')?;
    let scheme = &inner[..colon];
    if scheme.len() < 2
        || scheme.len() > 32
        || !scheme.chars().next().unwrap().is_ascii_alphabetic()
        || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
    {
        return None;
    }
    Some(s[..=end].chars().count())
}

fn html_inline_len(s: &str) -> Option<usize> {
    let chars: Vec<char> = s.chars().collect();
    if s.starts_with("<!--") {
        let end = s.find("-->")?;
        return Some(s[..end + 3].chars().count());
    }
    let mut j = 1;
    if chars.get(j) == Some(&'/') {
        j += 1;
    }
    if !chars.get(j)?.is_ascii_alphabetic() {
        return None;
    }
    while j < chars.len() && chars[j] != '>' {
        if chars[j] == '<' {
            return None;
        }
        j += 1;
    }
    if j >= chars.len() {
        return None;
    }
    Some(j + 1)
}

/// Parses `(dest "title")` at the start of `rest`; returns `(dest, title, consumed chars)`.
fn parse_inline_dest(rest: &[char]) -> Option<(String, Option<String>, usize)> {
    let mut j = 1;
    while j < rest.len() && rest[j].is_whitespace() {
        j += 1;
    }
    let mut dest = String::new();
    if rest.get(j) == Some(&'<') {
        j += 1;
        while j < rest.len() && rest[j] != '>' {
            dest.push(rest[j]);
            j += 1;
        }
        j += 1;
    } else {
        let mut depth = 0;
        while j < rest.len() && !rest[j].is_whitespace() {
            match rest[j] {
                '(' => depth += 1,
                ')' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                '\\' if j + 1 < rest.len() && rest[j + 1].is_ascii_punctuation() => {
                    j += 1;
                }
                _ => {}
            }
            dest.push(rest[j]);
            j += 1;
        }
    }
    while j < rest.len() && rest[j].is_whitespace() {
        j += 1;
    }
    let mut title = None;
    if matches!(rest.get(j), Some('"') | Some('\'') | Some('(')) {
        let close = if rest[j] == '(' { ')' } else { rest[j] };
        let mut t = String::new();
        j += 1;
        while j < rest.len() && rest[j] != close {
            t.push(rest[j]);
            j += 1;
        }
        j += 1;
        title = Some(t);
        while j < rest.len() && rest[j].is_whitespace() {
            j += 1;
        }
    }
    if rest.get(j) == Some(&')') {
        Some((dest, title, j + 1))
    } else {
        None
    }
}

/// The CommonMark "process emphasis" algorithm over a flat node list.
fn process_emphasis(mut nodes: Vec<Node>) -> Vec<Node> {
    let mut ci = 0;
    while ci < nodes.len() {
        let (cch, ccount, can_open_c, corig) = match &nodes[ci] {
            Node::Delim { ch, count, can_open, can_close: true, orig } => (*ch, *count, *can_open, *orig),
            _ => {
                ci += 1;
                continue;
            }
        };
        let mut found = None;
        for o in (0..ci).rev() {
            if let Node::Delim { ch, count, can_open: true, can_close, orig } = &nodes[o] {
                if *ch == cch && *count > 0 {
                    let skip = (*can_close || can_open_c)
                        && (*orig + corig) % 3 == 0
                        && !(*orig % 3 == 0 && corig % 3 == 0);
                    if !skip {
                        found = Some(o);
                        break;
                    }
                }
            }
        }
        let Some(o) = found else {
            if !can_open_c {
                nodes[ci] = Node::Text(cch.to_string().repeat(ccount));
            }
            ci += 1;
            continue;
        };
        let ocount = match &nodes[o] {
            Node::Delim { count, .. } => *count,
            _ => 0,
        };
        let use_n = if ocount >= 2 && ccount >= 2 { 2 } else { 1 };
        let inner: Vec<Node> = nodes.drain(o + 1..ci).collect();
        let inner = process_emphasis_inner(inner);
        if let Node::Delim { count, .. } = &mut nodes[o] {
            *count -= use_n;
        }
        if let Node::Delim { count, .. } = &mut nodes[o + 1] {
            *count -= use_n;
        }
        let wrapped = if use_n == 2 { Node::Strong(inner) } else { Node::Em(inner) };
        nodes.insert(o + 1, wrapped);
        let mut wi = o + 1;
        let mut closer_at = o + 2;
        if matches!(&nodes[o], Node::Delim { count: 0, .. }) {
            nodes.remove(o);
            wi -= 1;
            closer_at -= 1;
        }
        if matches!(&nodes[closer_at], Node::Delim { count: 0, .. }) {
            nodes.remove(closer_at);
            ci = wi + 1;
        } else {
            ci = closer_at;
        }
    }
    nodes
        .into_iter()
        .map(|n| match n {
            Node::Delim { ch, count, .. } => Node::Text(ch.to_string().repeat(count)),
            other => other,
        })
        .collect()
}

fn process_emphasis_inner(nodes: Vec<Node>) -> Vec<Node> {
    // Delimiters between an opener and closer that did not match become text.
    nodes
        .into_iter()
        .map(|n| match n {
            Node::Delim { ch, count, .. } => Node::Text(ch.to_string().repeat(count)),
            other => other,
        })
        .collect()
}

fn emit(nodes: &[Node], out: &mut String) {
    for n in nodes {
        match n {
            Node::Text(t) | Node::Raw(t) => out.push_str(&escape(t)),
            Node::Code(c) => {
                out.push_str("<code>");
                out.push_str(&escape(c));
                out.push_str("</code>");
            }
            Node::Soft => out.push('\n'),
            Node::Hard => out.push_str("<br />\n"),
            Node::Delim { ch, count, .. } => out.push_str(&ch.to_string().repeat(*count)),
            Node::Em(c) => {
                out.push_str("<em>");
                emit(c, out);
                out.push_str("</em>");
            }
            Node::Strong(c) => {
                out.push_str("<strong>");
                emit(c, out);
                out.push_str("</strong>");
            }
            Node::Link { children, dest, title } => {
                out.push_str(&format!("<a href=\"{}\"", escape(&percent_encode_url(dest))));
                if let Some(t) = title {
                    out.push_str(&format!(" title=\"{}\"", escape(t)));
                }
                out.push('>');
                emit(children, out);
                out.push_str("</a>");
            }
            Node::Open(c) => out.push(*c),
        }
    }
}

/// commonmark-java `Escaping.percentEncodeUrl` (keeps reserved characters and valid `%XX`).
fn percent_encode_url(s: &str) -> String {
    let mut out = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let keep = b.is_ascii_alphanumeric() || b"-_.!~*'();:@&=+$,/?#[]%".contains(&b);
        if b == b'%' && !(i + 2 < bytes.len() + 0 && bytes.get(i + 1).is_some_and(|x| x.is_ascii_hexdigit()) && bytes.get(i + 2).is_some_and(|x| x.is_ascii_hexdigit())) {
            out.push_str("%25");
        } else if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::render_to_html as r;

    #[test]
    fn paragraphs_and_code() {
        assert_eq!(r("a `b` c\n\nsecond"), "<p>a <code>b</code> c</p>\n<p>second</p>");
        assert_eq!(r("it's <b>x</b> & \"q\""), "<p>it's &lt;b&gt;x&lt;/b&gt; &amp; &quot;q&quot;</p>");
    }

    #[test]
    fn emphasis() {
        assert_eq!(r("${X}/_blaze_${U}"), "<p>${X}/<em>blaze</em>${U}</p>");
        assert_eq!(r("*a* **b** snake_case_name"), "<p><em>a</em> <strong>b</strong> snake_case_name</p>");
    }

    #[test]
    fn lists() {
        assert_eq!(r("x\n\n1. a\n2. b"), "<p>x</p>\n<ol>\n<li>a</li>\n<li>b</li>\n</ol>");
        assert_eq!(r("- a\n- b"), "<ul>\n<li>a</li>\n<li>b</li>\n</ul>");
        assert_eq!(r("- a\n\n- b"), "<ul>\n<li>\n<p>a</p>\n</li>\n<li>\n<p>b</p>\n</li>\n</ul>");
    }

    #[test]
    fn links() {
        assert_eq!(r("[t](http://x/y_z)"), "<p><a href=\"http://x/y_z\">t</a></p>");
        assert_eq!(r("[t]\n\n[t]: http://x"), "<p><a href=\"http://x\">t</a></p>");
    }
}
