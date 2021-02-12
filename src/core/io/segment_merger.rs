use std::cmp::{max, min};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;

use itertools::Itertools;

use crate::core::codegen::Segment;
use crate::errors::{MosError, MosResult};

/// A segment of data that will be emitted to an output file.
pub struct TargetSegment<'a> {
    /// The data contained in the segment
    data: [u8; 65536],
    /// Which of the data is actually valid
    range: Option<Range<u16>>,
    /// Which segments are the source of the data in this target segment?
    sources: HashMap<&'a str, &'a Segment>,
}

impl<'a> TargetSegment<'a> {
    pub fn range(&self) -> &Option<Range<u16>> {
        &self.range
    }

    pub fn range_data(&self) -> &[u8] {
        match &self.range {
            Some(range) => &self.data[self.range_usize(range)],
            None => &[],
        }
    }

    fn merge(&mut self, segment_name: &'a str, segment: &'a Segment) {
        let target_range = self.range_usize(&segment.target_range().unwrap());
        self.sources.insert(segment_name, segment);
        self.data[target_range.clone()].copy_from_slice(segment.range_data());

        match &mut self.range {
            Some(br) => {
                br.start = min(br.start, target_range.start as u16);
                br.end = max(br.end, target_range.end as u16);
            }
            None => self.range = Some(target_range.start as u16..target_range.end as u16),
        }
    }

    fn range_usize(&self, range: &Range<u16>) -> Range<usize> {
        Range {
            start: range.start as usize,
            end: range.end as usize,
        }
    }

    #[allow(clippy::suspicious_operation_groupings)]
    fn overlaps_with_sources(&self, new_range: &Range<u16>) -> Vec<(&&'a str, &&'a Segment)> {
        self.sources
            .iter()
            .filter_map(|(segment_name, segment)| {
                let sr = segment.range().as_ref().unwrap();
                if (new_range.start >= sr.start && new_range.start <= sr.end)
                    || (new_range.end >= sr.start && new_range.end <= sr.end)
                {
                    Some((segment_name, segment))
                } else {
                    None
                }
            })
            .collect()
    }
}

/// SegmentMerger contains information about which segments should go to which output target
pub struct SegmentMerger<'a> {
    targets: HashMap<PathBuf, TargetSegment<'a>>,
    default_target: PathBuf,
    errors: Vec<MosError>,
}

impl<'a> SegmentMerger<'a> {
    /// Creates a new merger with a single default target
    pub fn new(default_target: PathBuf) -> Self {
        Self {
            targets: HashMap::new(),
            default_target,
            errors: vec![],
        }
    }

    /// Which targets are available?
    pub fn targets(&self) -> &HashMap<PathBuf, TargetSegment<'a>> {
        &self.targets
    }

    /// Have there been any errors during merging?
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    /// The errors that occurred during merging
    pub fn errors(self) -> Vec<MosError> {
        self.errors
    }

    /// Merge a segment into the existing merged segments, taking care to see it doesn't overlap with already present segments
    pub fn merge(&mut self, segment_name: &'a str, segment: &'a Segment) -> MosResult<()> {
        if let Some(seg_range) = segment.target_range() {
            let target_name = &self.default_target;
            let target = match self.targets.entry(target_name.clone()) {
                Entry::Occupied(o) => o.into_mut(),
                Entry::Vacant(e) => e.insert(TargetSegment {
                    data: [0; 65536],
                    range: None,
                    sources: HashMap::new(),
                }),
            };

            let overlaps = target.overlaps_with_sources(&seg_range);
            if !overlaps.is_empty() {
                let overlaps = overlaps
                    .into_iter()
                    .map(|(name, segment)| {
                        let sr = segment.range().as_ref().unwrap();
                        format!("segment '{}' (${:04x} - ${:04x})", name, sr.start, sr.end)
                    })
                    .join(", ");
                self.errors.push(MosError::BuildError(format!(
                    "in target '{}': segment '{}' (${:04x} - ${:04x}) overlaps with: {}",
                    target_name.to_string_lossy(),
                    segment_name,
                    seg_range.start,
                    seg_range.end,
                    overlaps
                )));
            }

            target.merge(segment_name, segment);
        }

        Ok(())
    }
}