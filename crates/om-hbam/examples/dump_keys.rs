//! Read-only dump of a local file's key tree, or of the subtrees under the
//! given paths: one line per key with its value in hex and, when the value
//! reads as text (XOR 0x5A, file-format.md 3.5), that text.
//!
//! Paths are slash-separated hex segments (`2/0d`, `3/fb/5`); segments are
//! printed the same way. Chunked values are shown reassembled on their node.
//!
//! `cargo run -p om-hbam --example dump_keys -- <file.fmp12> [path]...`
use om_hbam::{HbamFile, KeySegment, Node};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|x| format!("{x:02x}")).collect()
}

fn parse_path(text: &str) -> Vec<Vec<u8>> {
    text.split('/')
        .filter(|s| !s.is_empty())
        .map(|s| {
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..(i + 2).min(s.len())], 16).expect("hex path"))
                .collect()
        })
        .collect()
}

fn printable(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| !c.is_control() || c == '\n' || c == '\r' || c == '\t')
}

fn print_node(prefix: &str, node: &Node<'_>) {
    node.walk(&mut |path: &[KeySegment<'_>], n: &Node<'_>| {
        let mut key = prefix.to_string();
        for seg in path {
            key.push('/');
            key.push_str(&hex(seg.0));
        }
        let mut line = key;
        if let Some(value) = n.assembled() {
            if n.value.is_none() {
                line.push_str(" [chunked]");
            }
            let max = std::env::var("MAXV")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(usize::MAX);
            let shown = &value[..value.len().min(max)];
            line.push_str(&format!(
                " = {}{} ({})",
                hex(shown),
                if shown.len() < value.len() { ".." } else { "" },
                value.len()
            ));
            if let Some(Ok(text)) = n.text() {
                if printable(&text) && text.len() <= max {
                    line.push_str(&format!(" {text:?}"));
                }
            }
        }
        println!("{line}");
    });
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let file = HbamFile::open(args.next().ok_or("path required")?)?;
    let tree = file.key_tree()?;
    let paths: Vec<String> = args.collect();
    if paths.is_empty() {
        print_node("", &tree.root);
        return Ok(());
    }
    for path in paths {
        let segments = parse_path(&path);
        let mut node = Some(&tree.root);
        for seg in &segments {
            node = node.and_then(|n| n.child(seg));
        }
        match node {
            Some(n) => print_node(&path, n),
            None => println!("{path}: missing"),
        }
    }
    Ok(())
}
