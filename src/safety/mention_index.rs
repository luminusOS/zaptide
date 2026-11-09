use super::{MentionLabel, mention_token_char};

#[derive(Default)]
struct Node {
    first: Option<usize>,
    mention: Option<usize>,
}

struct Edge {
    byte: u8,
    next: Option<usize>,
    child: usize,
}

pub(super) struct MentionIndex<'a> {
    mentions: &'a [MentionLabel],
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

impl<'a> MentionIndex<'a> {
    pub(super) fn new(mentions: &'a [MentionLabel]) -> Self {
        let mut index = Self {
            mentions,
            nodes: Vec::new(),
            edges: Vec::new(),
        };
        if mentions.is_empty() {
            return index;
        }
        index.nodes.push(Node::default());
        for (number, mention) in mentions.iter().enumerate() {
            if mention.user.is_empty() {
                continue;
            }
            let mut node = 0;
            for byte in mention.user.bytes() {
                node = if let Some(child) = index.child(node, byte) {
                    child
                } else {
                    let child = index.nodes.len();
                    index.nodes.push(Node::default());
                    index.edges.push(Edge {
                        byte,
                        next: index.nodes[node].first,
                        child,
                    });
                    index.nodes[node].first = Some(index.edges.len() - 1);
                    child
                };
            }
            if index.nodes[node].mention.is_none() {
                index.nodes[node].mention = Some(number);
            }
        }
        index
    }

    fn child(&self, node: usize, byte: u8) -> Option<usize> {
        let mut edge = self.nodes[node].first;
        while let Some(position) = edge {
            #[cfg(test)]
            super::MENTION_STEPS.with(|steps| steps.set(steps.get() + 1));
            let current = &self.edges[position];
            if current.byte == byte {
                return Some(current.child);
            }
            edge = current.next;
        }
        None
    }

    pub(super) fn find(&self, rest: &str) -> Option<(usize, &'a MentionLabel)> {
        if self.nodes.is_empty() {
            return None;
        }
        let token = rest.strip_prefix('@')?;
        let end = token
            .find(|c: char| !mention_token_char(c))
            .unwrap_or(token.len());
        let mut node = 0;
        for byte in token[..end].bytes() {
            node = self.child(node, byte)?;
        }
        Some((end + 1, &self.mentions[self.nodes[node].mention?]))
    }
}
