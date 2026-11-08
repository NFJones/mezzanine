//! Whole-unit input packing and temporary lossless UTF-8 fragmentation.

use super::*;

/// Admission uses the complete auxiliary request estimator. A conservative
/// estimator must be monotonic for prefixes; exact provider rejection can still
/// lower capacity later. Byte boundaries are always validated, never truncated.
pub(super) fn split_raw(
    source: &str,
    range: Range<usize>,
    capacity: usize,
    max_fragments: usize,
    measure: &impl Fn(&str) -> AgentContextResult<usize>,
) -> AgentContextResult<Vec<Range<usize>>> {
    let mut ranges = Vec::new();
    let mut start = range.start;
    while start < range.end {
        if ranges.len() >= max_fragments {
            return Err(invalid("compaction work-node memory bound exceeded"));
        }
        if measure(&source[start..range.end])? <= capacity {
            ranges.push(start..range.end);
            break;
        }
        let mut low = start;
        let mut high = range.end;
        while low < high {
            let midpoint = low + (high - low).div_ceil(2);
            let mut boundary = midpoint;
            while boundary > low && !source.is_char_boundary(boundary) {
                boundary -= 1;
            }
            if boundary == low {
                boundary = low + source[low..high].chars().next().map_or(0, char::len_utf8);
            }
            if measure(&source[start..boundary])? <= capacity {
                low = boundary;
            } else {
                high = boundary - 1;
                while high > start && !source.is_char_boundary(high) {
                    high -= 1;
                }
            }
        }
        if low == start {
            return Err(invalid("compactor input cannot fit one source character"));
        }
        ranges.push(start..low);
        start = low;
    }
    Ok(ranges)
}

impl EpochCompactionEngine {
    pub(super) fn node_text<'a>(&'a self, node: &'a Node) -> Option<&'a str> {
        match &node.material {
            Material::Raw(range) => Some(&self.source.text[range.clone()]),
            Material::Summary(text) => Some(text),
            Material::Claimed(_) => None,
        }
    }

    /// Finds the longest fitting adjacent run of the requested shape. A merge
    /// contains at least two summaries; a carry contains a summary and raw
    /// source. A lone completed summary is never shortened by a unary call.
    pub(super) fn candidate(
        &self,
        start: usize,
        carry: bool,
        merge: bool,
        measure: &impl Fn(&str) -> AgentContextResult<usize>,
    ) -> AgentContextResult<Option<(Vec<Node>, String)>> {
        // The common fitting-source path measures the complete request once,
        // rather than re-estimating every growing prefix of a long history.
        if !carry && !merge && self.work_count == 0 && start == 0 {
            let raw = self
                .nodes
                .range(start..)
                .map(|(_, node)| node)
                .take_while(|node| matches!(node.material, Material::Raw(_)))
                .collect::<Vec<_>>();
            if let Some(last) = raw.last() {
                let text = &self.source.text[start..last.coverage.end];
                if measure(text)? <= self.limits.input_tokens {
                    return Ok(Some((raw.into_iter().cloned().collect(), text.to_string())));
                }
            }
        }
        let mut nodes = Vec::new();
        let mut text = String::new();
        for node in self.nodes.range(start..).map(|(_, node)| node) {
            let raw = matches!(node.material, Material::Raw(_));
            if (merge && raw) || (!merge && !raw && !(carry && nodes.is_empty())) {
                break;
            }
            let Some(content) = self.node_text(node) else {
                break;
            };
            let old_len = text.len();
            if matches!(node.material, Material::Summary(_)) {
                text.push_str("[private compaction intermediate]\n");
                text.push_str(content);
                text.push_str("\n[/private compaction intermediate]\n");
            } else {
                text.push_str(content);
            }
            if measure(&text)? > self.limits.input_tokens {
                text.truncate(old_len);
                break;
            }
            nodes.push(node.clone());
        }
        let minimum = if carry || merge { 2 } else { 1 };
        Ok((nodes.len() >= minimum).then_some((nodes, text)))
    }
}
