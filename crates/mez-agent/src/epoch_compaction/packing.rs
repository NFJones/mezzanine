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
        let end = fitting_prefix(source, start..range.end, capacity, measure)?
            .ok_or_else(|| invalid("compactor input cannot fit one source character"))?;
        ranges.push(start..end);
        start = end;
    }
    Ok(ranges)
}

/// Finds a nonempty fitting UTF-8 prefix without losing or trimming its suffix.
/// None is an actual inability to fit one scalar, not a summary-size rejection.
fn fitting_prefix(
    source: &str,
    range: Range<usize>,
    capacity: usize,
    measure: &impl Fn(&str) -> AgentContextResult<usize>,
) -> AgentContextResult<Option<usize>> {
    if measure(&source[range.clone()])? <= capacity {
        return Ok(Some(range.end));
    }
    let mut low = range.start;
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
        if measure(&source[range.start..boundary])? <= capacity {
            low = boundary;
        } else {
            high = boundary - 1;
            while high > range.start && !source.is_char_boundary(high) {
                high -= 1;
            }
        }
    }
    Ok((low > range.start).then_some(low))
}

impl EpochCompactionEngine {
    /// Wait for already claimed siblings when one final merge may cover them
    /// all. An early merge is necessary only when known adjacent results already
    /// exceed input capacity, or the caller must relieve held-summary pressure.
    pub(super) fn merge_candidate(
        &self,
        wait_for_claimed: bool,
        measure: &impl Fn(&str) -> AgentContextResult<usize>,
    ) -> AgentContextResult<Option<PackedWork>> {
        for (&start, node) in &self.nodes {
            if !matches!(node.material, Material::Summary(_)) {
                continue;
            }
            let Some(candidate) = self.candidate(start, false, true, measure)? else {
                continue;
            };
            if wait_for_claimed {
                let known_adjacent = self
                    .nodes
                    .range(start..)
                    .take_while(|(_, node)| matches!(node.material, Material::Summary(_)))
                    .count();
                if candidate.children.len() == known_adjacent {
                    continue;
                }
            }
            return Ok(Some(candidate));
        }
        Ok(None)
    }

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
    ) -> AgentContextResult<Option<PackedWork>> {
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
                    return Ok(Some(PackedWork {
                        children: raw.into_iter().cloned().collect(),
                        input: text.to_string(),
                        remainder: None,
                    }));
                }
            }
        }
        let mut nodes = Vec::new();
        let mut text = String::new();
        let mut remainder = None;
        for node in self.nodes.range(start..).map(|(_, node)| node) {
            let raw = matches!(node.material, Material::Raw(_));
            if (merge && raw) || !(merge || raw || carry && nodes.is_empty()) {
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
                if carry
                    && !nodes.is_empty()
                    && let Material::Raw(range) = &node.material
                    && self.source.units.iter().any(|unit| {
                        let whole = unit.bytes();
                        whole.start <= range.start && whole.end >= range.end && whole != *range
                    })
                {
                    // Only a previously fragmented oversized unit can split
                    // further to share capacity with the carry. Fitting whole
                    // ownership units never split for a scheduling preference.
                    let end = fitting_prefix(
                        &self.source.text,
                        range.clone(),
                        self.limits.input_tokens,
                        &|fragment| measure(&format!("{text}{fragment}")),
                    )?;
                    if let Some(end) = end {
                        text.push_str(&self.source.text[range.start..end]);
                        nodes.push(Node {
                            coverage: range.start..end,
                            material: Material::Raw(range.start..end),
                            depth: 0,
                        });
                        if end < range.end {
                            remainder = Some(Node {
                                coverage: end..range.end,
                                material: Material::Raw(end..range.end),
                                depth: 0,
                            });
                        }
                    }
                }
                break;
            }
            nodes.push(node.clone());
        }
        let minimum = if carry || merge { 2 } else { 1 };
        Ok((nodes.len() >= minimum).then_some(PackedWork {
            children: nodes,
            input: text,
            remainder,
        }))
    }
}
