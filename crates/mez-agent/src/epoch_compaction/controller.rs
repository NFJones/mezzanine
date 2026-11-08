//! Private reduction graph and exact logical completion fences.

use super::*;

impl EpochCompactionEngine {
    /// Starts one immutable source reduction. `measure` estimates the COMPLETE
    /// auxiliary request, including its real instructions/schema/framing. It
    /// must be deterministic and conservative for prefix packing. Fixed replay
    /// costs supply only the initial scheduling lower bound. No model is called.
    pub fn new(
        source: FrozenEpochCompactionSource,
        limits: EpochCompactionLimits,
        measure: impl Fn(&str) -> AgentContextResult<usize>,
    ) -> AgentContextResult<Self> {
        limits.validate()?;
        if source.text.len() > limits.source_bytes {
            return Err(invalid("epoch compaction source memory bound exceeded"));
        }
        let schedule = CompactionScheduleEstimate::new(
            source.plan.input_estimates().source_input_tokens()?,
            limits,
        );
        let mut nodes = BTreeMap::new();
        // Keep unit boundaries even when the entire source fits. Greedy
        // dispatch then makes exactly one call, while a later genuine input
        // rejection can repack whole units before fragmenting oversized ones.
        for unit in &source.units {
            for range in packing::split_raw(
                &source.text,
                unit.bytes(),
                limits.input_tokens,
                limits.work_nodes.saturating_sub(nodes.len()),
                &measure,
            )? {
                nodes.insert(
                    range.start,
                    Node {
                        coverage: range.clone(),
                        material: Material::Raw(range),
                        depth: 0,
                    },
                );
            }
        }
        Ok(Self {
            source,
            limits,
            schedule,
            nodes,
            claims: BTreeMap::new(),
            next_id: 0,
            work_count: 0,
            buffered_summary_bytes: 0,
        })
    }

    /// Returns the cheap frozen scheduling comparison for diagnostics/metrics.
    pub fn schedule(&self) -> CompactionScheduleEstimate {
        self.schedule
    }

    /// Claims one necessary work node, bounded by the existing resource owner.
    /// Wait means previously claimed work may enable progress. Error means no
    /// remaining necessary source/merge can fit; it never schedules a unary
    /// shortening call. A final completed summary is not measured for input fit.
    pub fn next(
        &mut self,
        measure: impl Fn(&str) -> AgentContextResult<usize>,
    ) -> AgentContextResult<EpochCompactionStep> {
        if self.is_complete() {
            return Ok(EpochCompactionStep::Complete);
        }
        if self.claims.len() >= self.limits.in_flight {
            return Ok(EpochCompactionStep::Waiting);
        }
        if self.schedule.selected == CompactionSchedule::Rolling && !self.claims.is_empty() {
            return Ok(EpochCompactionStep::Waiting);
        }
        let mut chosen = None;
        if self.schedule.selected == CompactionSchedule::Rolling {
            for (&start, node) in &self.nodes {
                if matches!(node.material, Material::Summary(_)) {
                    chosen = self.candidate(start, true, false, &measure)?;
                    if chosen.is_some() {
                        break;
                    }
                }
            }
        }
        if chosen.is_none() {
            for (&start, node) in &self.nodes {
                if matches!(node.material, Material::Raw(_)) {
                    chosen = self.candidate(start, false, false, &measure)?;
                    if chosen.is_some() {
                        break;
                    }
                }
            }
        }
        if chosen.is_none() {
            for (&start, node) in &self.nodes {
                if matches!(node.material, Material::Summary(_)) {
                    chosen = self.candidate(start, false, true, &measure)?;
                    if chosen.is_some() {
                        break;
                    }
                }
            }
        }
        let Some((children, input)) = chosen else {
            return if self.claims.is_empty() {
                Err(invalid(
                    "remaining compaction source cannot fit a necessary merge",
                ))
            } else {
                Ok(EpochCompactionStep::Waiting)
            };
        };
        let id = EpochCompactionWorkId(self.next_id);
        let next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| invalid("compaction work identity exhausted"))?;
        let first = children
            .first()
            .ok_or_else(|| invalid("missing compaction work input"))?;
        let last = children
            .last()
            .ok_or_else(|| invalid("missing compaction work input"))?;
        let depth = children
            .iter()
            .map(|node| node.depth)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| invalid("compaction work depth exhausted"))?;
        let work = EpochCompactionWork {
            id,
            input: input.into(),
            coverage: first.coverage.start..last.coverage.end,
            raw_coverage: children
                .iter()
                .filter_map(|node| match &node.material {
                    Material::Raw(range) => Some(range.clone()),
                    _ => None,
                })
                .collect(),
            depth,
        };
        for node in &children {
            self.nodes.remove(&node.coverage.start);
        }
        self.nodes.insert(
            work.coverage.start,
            Node {
                coverage: work.coverage.clone(),
                material: Material::Claimed(id),
                depth,
            },
        );
        self.claims.insert(
            id,
            Claim {
                work: work.clone(),
                children,
            },
        );
        self.next_id = next_id;
        self.work_count = self.work_count.saturating_add(1);
        Ok(EpochCompactionStep::Work(work))
    }

    /// Accepts completed, decoded nonempty text within real parser/memory bounds.
    /// Empty/resource/stale errors leave the claim intact for the adapter's
    /// finite invalid-output repair or terminal failure policy. Transport retry
    /// needs no controller mutation and resends the same claimed work.
    pub fn complete(
        &mut self,
        id: EpochCompactionWorkId,
        summary: String,
    ) -> AgentContextResult<()> {
        let claim = self
            .claims
            .get(&id)
            .ok_or_else(|| invalid("stale compaction work completion"))?;
        if summary.trim().is_empty() {
            return Err(invalid("compaction work returned an empty summary"));
        }
        if summary.len() > self.limits.summary_bytes {
            return Err(invalid("compaction summary parser byte bound exceeded"));
        }
        let consumed = claim
            .children
            .iter()
            .filter_map(|node| match &node.material {
                Material::Summary(text) => Some(text.len()),
                _ => None,
            })
            .sum::<usize>();
        let buffered = self
            .buffered_summary_bytes
            .checked_sub(consumed)
            .and_then(|bytes| bytes.checked_add(summary.len()))
            .filter(|bytes| *bytes <= self.limits.buffered_summary_bytes)
            .ok_or_else(|| invalid("compaction intermediate memory bound exceeded"))?;
        let node = self
            .nodes
            .get_mut(&claim.work.coverage.start)
            .filter(|node| matches!(node.material, Material::Claimed(current) if current == id))
            .ok_or_else(|| invalid("compaction work ownership drifted"))?;
        node.material = Material::Summary(summary.into());
        self.buffered_summary_bytes = buffered;
        self.claims.remove(&id);
        Ok(())
    }

    /// Responds only to a genuine auxiliary INPUT rejection. Restore the exact
    /// children, lower actual admission capacity and fragment oversized RAW
    /// material losslessly. Accepted summaries are never split/shortened to
    /// meet a semantic target. Other in-flight claims retain their exact input.
    /// Failure leaves the controller unchanged; terminal errors cannot publish.
    pub fn reject_input(
        &mut self,
        id: EpochCompactionWorkId,
        input_tokens: usize,
        measure: impl Fn(&str) -> AgentContextResult<usize>,
    ) -> AgentContextResult<()> {
        if input_tokens >= self.limits.input_tokens || input_tokens <= self.limits.framing_tokens {
            return Err(invalid(
                "input rejection did not lower usable compactor capacity",
            ));
        }
        let claim = self
            .claims
            .get(&id)
            .ok_or_else(|| invalid("stale compaction input rejection"))?;
        let mut replacements = Vec::new();
        for node in self.nodes.values() {
            let children = if node.coverage.start == claim.work.coverage.start {
                claim.children.as_slice()
            } else {
                std::slice::from_ref(node)
            };
            for child in children {
                if let Material::Raw(range) = &child.material {
                    for fragment in packing::split_raw(
                        &self.source.text,
                        range.clone(),
                        input_tokens,
                        self.limits.work_nodes.saturating_sub(replacements.len()),
                        &measure,
                    )? {
                        replacements.push(Node {
                            coverage: fragment.clone(),
                            material: Material::Raw(fragment),
                            depth: 0,
                        });
                    }
                } else {
                    replacements.push(child.clone());
                }
                if replacements.len() > self.limits.work_nodes {
                    return Err(invalid("compaction work-node memory bound exceeded"));
                }
            }
        }
        self.nodes = replacements
            .into_iter()
            .map(|node| (node.coverage.start, node))
            .collect();
        self.claims.remove(&id);
        self.limits.input_tokens = input_tokens;
        Ok(())
    }

    /// Transfers one final and its frozen source witnesses. Intermediate results
    /// and in-flight work cannot be extracted through this publication boundary.
    pub fn into_result(self) -> AgentContextResult<AcceptedEpochCompaction> {
        if !self.is_complete() {
            return Err(invalid("epoch compaction has no complete final summary"));
        }
        let node = self
            .nodes
            .into_values()
            .next()
            .ok_or_else(|| invalid("missing final compaction node"))?;
        let Material::Summary(summary) = node.material else {
            return Err(invalid("missing completed final compaction summary"));
        };
        Ok(AcceptedEpochCompaction {
            plan: self.source.plan,
            summary: summary.to_string(),
            units: self.source.units,
            work_count: self.work_count,
            serial_depth: node.depth,
        })
    }

    fn is_complete(&self) -> bool {
        self.claims.is_empty()
            && self.nodes.len() == 1
            && self.nodes.values().next().is_some_and(|node| {
                node.coverage == (0..self.source.text.len())
                    && matches!(node.material, Material::Summary(_))
            })
    }
}
