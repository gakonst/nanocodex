use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::File as StdFile,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

use chromiumoxide::{
    Page,
    cdp::js_protocol::{
        heap_profiler::{
            CollectGarbageParams, EnableParams as HeapEnableParams, EventAddHeapSnapshotChunk,
            TakeHeapSnapshotParams,
        },
        profiler::{
            EnableParams as ProfilerEnableParams, Profile, ScriptCoverage,
            StartParams as CpuStartParams, StartPreciseCoverageParams, StopParams as CpuStopParams,
            StopPreciseCoverageParams, TakePreciseCoverageParams,
        },
    },
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::{io::AsyncWriteExt, sync::watch, task::JoinHandle, time::timeout};

use crate::{
    BrowserCoverage, BrowserCpuFunction, BrowserCpuProfile, BrowserHeapClass,
    BrowserHeapClassDelta, BrowserHeapComparison, BrowserHeapDuplicateString,
    BrowserHeapInspection, BrowserHeapNode, BrowserHeapRetainerNode, BrowserHeapRetainers,
    BrowserHeapSnapshot, BrowserScriptCoverage, trace_serialized,
};

use super::BrowserError;

const MAX_CPU_FUNCTIONS: usize = 50;
const MAX_HEAP_CLASSES: usize = 50;
const MAX_HEAP_BYTES: u64 = 1_073_741_824;
const HEAP_TIMEOUT: Duration = Duration::from_mins(1);

pub(super) async fn start_cpu(page: &Page) -> Result<(), BrowserError> {
    page.execute(ProfilerEnableParams::default()).await?;
    page.execute(CpuStartParams::default()).await?;
    Ok(())
}

pub(super) async fn stop_cpu(
    page: &Page,
    output_dir: &Path,
    sequence: u64,
) -> Result<BrowserCpuProfile, BrowserError> {
    let response = page.execute(CpuStopParams::default()).await?;
    let profile = &response.profile;
    let path = output_dir.join(format!("cpu-profile-{sequence}.json"));
    tokio::fs::write(&path, serde_json::to_vec(profile)?).await?;
    Ok(summarize_cpu(profile, path))
}

pub(super) async fn start_coverage(page: &Page) -> Result<(), BrowserError> {
    page.execute(ProfilerEnableParams::default()).await?;
    page.execute(
        StartPreciseCoverageParams::builder()
            .call_count(true)
            .detailed(true)
            .allow_triggered_updates(false)
            .build(),
    )
    .await?;
    Ok(())
}

pub(super) async fn stop_coverage(
    page: &Page,
    output_dir: &Path,
    sequence: u64,
) -> Result<BrowserCoverage, BrowserError> {
    let response = page.execute(TakePreciseCoverageParams::default()).await?;
    page.execute(StopPreciseCoverageParams::default()).await?;
    let coverage = &response.result;
    let path = output_dir.join(format!("coverage-{sequence}.json"));
    tokio::fs::write(
        &path,
        serde_json::to_vec(&CoverageArtifact {
            timestamp: coverage.timestamp,
            scripts: &coverage.result,
        })?,
    )
    .await?;
    Ok(summarize_coverage(&coverage.result, path))
}

pub(super) async fn capture_heap(
    page: &Page,
    output_dir: &Path,
    sequence: u64,
    collect_garbage: bool,
) -> Result<(BrowserHeapSnapshot, HeapAnalysis), BrowserError> {
    page.execute(HeapEnableParams::default()).await?;
    if collect_garbage {
        page.execute(CollectGarbageParams::default()).await?;
    }
    let path = output_dir.join(format!("heap-snapshot-{sequence}.heapsnapshot"));
    let chunks = page.event_listener::<EventAddHeapSnapshotChunk>().await?;
    let (stop_tx, stop_rx) = watch::channel(false);
    let collector = collect_heap_chunks(path.clone(), chunks, stop_rx);
    page.execute(
        TakeHeapSnapshotParams::builder()
            .report_progress(false)
            .capture_numeric_value(true)
            .build(),
    )
    .await?;
    let _ = stop_tx.send(true);
    let collected = timeout(HEAP_TIMEOUT, collector)
        .await
        .map_err(|_| BrowserError::HeapSnapshotTimeout)?;
    let collected = collected.map_err(BrowserError::HeapCollectorTask)?;
    let bytes = collected.map_err(BrowserError::Io)?;
    if bytes > MAX_HEAP_BYTES {
        return Err(BrowserError::HeapSnapshotTooLarge {
            bytes,
            maximum: MAX_HEAP_BYTES,
        });
    }
    let parse_path = path.clone();
    let analysis = tokio::task::spawn_blocking(move || analyze_heap(&parse_path))
        .await
        .map_err(BrowserError::HeapAnalysisTask)??;
    let artifact_id = format!("heap-{sequence}");
    let snapshot = analysis.public_snapshot(artifact_id);
    Ok((snapshot, analysis))
}

pub(super) async fn heap_retainers(
    artifact_id: String,
    path: PathBuf,
    node_id: u64,
    max_depth: u8,
    max_nodes: usize,
) -> Result<BrowserHeapRetainers, BrowserError> {
    tokio::task::spawn_blocking(move || {
        analyze_heap_retainers(
            StdFile::open(path)?,
            artifact_id,
            node_id,
            max_depth,
            max_nodes,
        )
    })
    .await
    .map_err(BrowserError::HeapAnalysisTask)?
}

pub(super) async fn inspect_heap(
    artifact_id: String,
    path: PathBuf,
    class_name: Option<String>,
    minimum_retained_size: u64,
    max_nodes: usize,
    include_duplicate_strings: bool,
) -> Result<BrowserHeapInspection, BrowserError> {
    tokio::task::spawn_blocking(move || {
        analyze_heap_details(
            &path,
            artifact_id,
            class_name.as_deref(),
            minimum_retained_size,
            max_nodes,
            include_duplicate_strings,
        )
    })
    .await
    .map_err(BrowserError::HeapAnalysisTask)?
}

pub(super) fn compare_heaps(
    before_id: &str,
    before: &HeapAnalysis,
    after_id: &str,
    after: &HeapAnalysis,
) -> BrowserHeapComparison {
    let mut names = before
        .classes
        .keys()
        .chain(after.classes.keys())
        .collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();
    let mut growing_classes = names
        .into_iter()
        .filter_map(|name| {
            let before = before.classes.get(name);
            let after = after.classes.get(name);
            let delta = BrowserHeapClassDelta {
                name: name.clone(),
                instance_count_delta: signed_delta_usize(
                    after.map_or(0, |class| class.instance_count),
                    before.map_or(0, |class| class.instance_count),
                ),
                self_size_delta: signed_delta(
                    after.map_or(0, |class| class.self_size),
                    before.map_or(0, |class| class.self_size),
                ),
                maximum_retained_size_delta: signed_delta(
                    after.map_or(0, |class| class.maximum_retained_size),
                    before.map_or(0, |class| class.maximum_retained_size),
                ),
            };
            (delta.maximum_retained_size_delta > 0 || delta.self_size_delta > 0).then_some(delta)
        })
        .collect::<Vec<_>>();
    growing_classes.sort_unstable_by(|left, right| {
        right
            .maximum_retained_size_delta
            .cmp(&left.maximum_retained_size_delta)
            .then_with(|| right.self_size_delta.cmp(&left.self_size_delta))
    });
    growing_classes.truncate(MAX_HEAP_CLASSES);
    BrowserHeapComparison {
        before_id: before_id.to_owned(),
        after_id: after_id.to_owned(),
        node_count_delta: signed_delta_usize(after.node_count, before.node_count),
        self_size_delta: signed_delta(after.total_self_size, before.total_self_size),
        growing_classes,
    }
}

#[derive(Clone)]
pub(super) struct HeapAnalysis {
    path: PathBuf,
    node_count: usize,
    total_self_size: u64,
    classes: BTreeMap<String, BrowserHeapClass>,
}

impl HeapAnalysis {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    fn public_snapshot(&self, artifact_id: String) -> BrowserHeapSnapshot {
        let mut classes = self.classes.values().cloned().collect::<Vec<_>>();
        classes.sort_unstable_by(|left, right| {
            right
                .maximum_retained_size
                .cmp(&left.maximum_retained_size)
                .then_with(|| right.self_size.cmp(&left.self_size))
        });
        classes.truncate(MAX_HEAP_CLASSES);
        BrowserHeapSnapshot {
            artifact_id,
            path: self.path.clone(),
            node_count: self.node_count,
            total_self_size: self.total_self_size,
            classes,
        }
    }
}

fn collect_heap_chunks(
    path: PathBuf,
    mut chunks: impl futures_util::Stream<Item = std::sync::Arc<EventAddHeapSnapshotChunk>>
    + Unpin
    + Send
    + 'static,
    mut stop: watch::Receiver<bool>,
) -> JoinHandle<Result<u64, std::io::Error>> {
    tokio::spawn(async move {
        let mut file = tokio::fs::File::create(path).await?;
        let mut bytes = 0_u64;
        loop {
            tokio::select! {
                biased;
                chunk = chunks.next() => {
                    let Some(chunk) = chunk else {
                        break;
                    };
                    trace_serialized(
                        "devtools.HeapProfiler.addHeapSnapshotChunk",
                        chunk.as_ref(),
                    );
                    file.write_all(chunk.chunk.as_bytes()).await?;
                    bytes = bytes.saturating_add(
                        u64::try_from(chunk.chunk.len()).unwrap_or(u64::MAX)
                    );
                    if bytes > MAX_HEAP_BYTES {
                        break;
                    }
                }
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        break;
                    }
                }
            }
        }
        if *stop.borrow() {
            while let Ok(Some(chunk)) = timeout(Duration::from_millis(50), chunks.next()).await {
                trace_serialized("devtools.HeapProfiler.addHeapSnapshotChunk", chunk.as_ref());
                file.write_all(chunk.chunk.as_bytes()).await?;
                bytes = bytes.saturating_add(u64::try_from(chunk.chunk.len()).unwrap_or(u64::MAX));
                if bytes > MAX_HEAP_BYTES {
                    break;
                }
            }
        }
        file.flush().await?;
        Ok(bytes)
    })
}

fn analyze_heap(path: &Path) -> Result<HeapAnalysis, BrowserError> {
    let graph = v8_heap_parser::decode_reader(StdFile::open(path)?)
        .map_err(BrowserError::HeapSnapshotParse)?;
    let node_count = graph.nodes().len();
    let total_self_size = graph.nodes().iter().map(|node| node.weight.self_size).sum();
    let classes = graph
        .get_class_groups(true)
        .iter()
        .map(|group| {
            let node = graph
                .get_node(group.index)
                .ok_or(BrowserError::HeapClassUnavailable { index: group.index })?;
            let (maximum_retained_node_index, maximum_retained_size) = group
                .nodes
                .iter()
                .map(|index| (*index, graph.retained_size(*index)))
                .max_by_key(|(_, retained_size)| *retained_size)
                .unwrap_or((group.index, 0));
            let maximum_retained_node = graph.get_node(maximum_retained_node_index).ok_or(
                BrowserError::HeapClassUnavailable {
                    index: maximum_retained_node_index,
                },
            )?;
            Ok((
                node.class_name().to_owned(),
                BrowserHeapClass {
                    name: node.class_name().to_owned(),
                    instance_count: group.nodes.len(),
                    self_size: group.self_size,
                    maximum_retained_size,
                    maximum_retained_node_id: u64::from(maximum_retained_node.id),
                },
            ))
        })
        .collect::<Result<_, BrowserError>>()?;
    Ok(HeapAnalysis {
        path: path.to_path_buf(),
        node_count,
        total_self_size,
        classes,
    })
}

fn analyze_heap_details(
    path: &Path,
    artifact_id: String,
    class_name: Option<&str>,
    minimum_retained_size: u64,
    max_nodes: usize,
    include_duplicate_strings: bool,
) -> Result<BrowserHeapInspection, BrowserError> {
    let graph = v8_heap_parser::decode_reader(StdFile::open(path)?)
        .map_err(BrowserError::HeapSnapshotParse)?;
    let mut nodes = graph
        .nodes()
        .iter()
        .enumerate()
        .filter_map(|(index, raw)| {
            let node = &raw.weight;
            if class_name.is_some_and(|expected| node.class_name() != expected) {
                return None;
            }
            let retained_size = graph.retained_size(index);
            if retained_size < minimum_retained_size {
                return None;
            }
            Some(BrowserHeapNode {
                node_id: u64::from(node.id),
                name: node.name().to_owned(),
                class_name: node.class_name().to_owned(),
                node_type: format!("{:?}", node.typ).to_ascii_lowercase(),
                self_size: node.self_size,
                retained_size,
                detached: node.detachedness != 0,
            })
        })
        .collect::<Vec<_>>();
    nodes.sort_unstable_by(|left, right| {
        right
            .retained_size
            .cmp(&left.retained_size)
            .then_with(|| right.self_size.cmp(&left.self_size))
            .then_with(|| left.node_id.cmp(&right.node_id))
    });
    let matching_node_count = nodes.len();
    nodes.truncate(max_nodes);

    let duplicate_strings = if include_duplicate_strings {
        let mut strings = HashMap::<String, (usize, u64)>::new();
        for raw in graph.nodes() {
            let node = &raw.weight;
            if !matches!(
                node.typ,
                v8_heap_parser::NodeType::String
                    | v8_heap_parser::NodeType::ConcatString
                    | v8_heap_parser::NodeType::SliceString
            ) {
                continue;
            }
            let entry = strings.entry(node.name().to_owned()).or_default();
            entry.0 = entry.0.saturating_add(1);
            entry.1 = entry.1.saturating_add(node.self_size);
        }
        let mut strings = strings
            .into_iter()
            .filter(|(_, (count, _))| *count > 1)
            .map(
                |(value, (instance_count, self_size))| BrowserHeapDuplicateString {
                    value: value.chars().take(512).collect(),
                    instance_count,
                    self_size,
                },
            )
            .collect::<Vec<_>>();
        strings.sort_unstable_by(|left, right| {
            right
                .self_size
                .cmp(&left.self_size)
                .then_with(|| right.instance_count.cmp(&left.instance_count))
        });
        strings.truncate(100);
        strings
    } else {
        Vec::new()
    };
    Ok(BrowserHeapInspection {
        artifact_id,
        matching_node_count,
        truncated: matching_node_count > nodes.len(),
        nodes,
        duplicate_strings,
    })
}

fn analyze_heap_retainers(
    reader: impl Read,
    artifact_id: String,
    target_node_id: u64,
    max_depth: u8,
    max_nodes: usize,
) -> Result<BrowserHeapRetainers, BrowserError> {
    let snapshot: RawHeapSnapshot =
        serde_json::from_reader(reader).map_err(BrowserError::HeapSnapshotParse)?;
    let layout = HeapLayout::new(&snapshot)?;
    let target_index = (0..layout.node_count)
        .find(|index| layout.node_id(&snapshot, *index) == Some(target_node_id))
        .ok_or_else(|| BrowserError::HeapNodeUnavailable {
            artifact_id: artifact_id.clone(),
            node_id: target_node_id,
        })?;
    let mut nodes = vec![layout.public_node(&snapshot, target_index, 0, None, None, None)?];
    let mut visited = HashSet::from([target_index]);
    let mut frontier = vec![target_index];
    let mut truncated = false;

    for distance in 1..=max_depth {
        if frontier.is_empty() {
            break;
        }
        let targets = frontier.iter().copied().collect::<HashSet<_>>();
        let mut next = Vec::new();
        let mut edge_offset = 0;
        for source_index in 0..layout.node_count {
            let edge_count = layout.node_edge_count(&snapshot, source_index)?;
            for _ in 0..edge_count {
                let edge = layout.edge(&snapshot, edge_offset)?;
                edge_offset = edge_offset.saturating_add(layout.edge_width);
                if !targets.contains(&edge.target_index)
                    || edge.edge_type == "weak"
                    || !visited.insert(source_index)
                {
                    continue;
                }
                if nodes.len() == max_nodes {
                    truncated = true;
                    return Ok(BrowserHeapRetainers {
                        artifact_id,
                        target_node_id,
                        nodes,
                        truncated,
                    });
                }
                let retained_node_id =
                    layout
                        .node_id(&snapshot, edge.target_index)
                        .ok_or_else(|| {
                            heap_format(format!(
                                "edge target {} has no node identifier",
                                edge.target_index
                            ))
                        })?;
                nodes.push(layout.public_node(
                    &snapshot,
                    source_index,
                    distance,
                    Some(retained_node_id),
                    Some(edge.edge_type),
                    Some(edge.edge_name),
                )?);
                if source_index != 0 {
                    next.push(source_index);
                }
            }
        }
        frontier = next;
    }
    if !frontier.is_empty() {
        truncated = true;
    }
    Ok(BrowserHeapRetainers {
        artifact_id,
        target_node_id,
        nodes,
        truncated,
    })
}

#[derive(Deserialize)]
struct RawHeapSnapshot {
    snapshot: RawHeapSnapshotHeader,
    nodes: Vec<u64>,
    edges: Vec<u64>,
    strings: Vec<String>,
}

#[derive(Deserialize)]
struct RawHeapSnapshotHeader {
    meta: RawHeapMeta,
}

#[derive(Deserialize)]
struct RawHeapMeta {
    node_fields: Vec<String>,
    node_types: Vec<RawHeapFieldType>,
    edge_fields: Vec<String>,
    edge_types: Vec<RawHeapFieldType>,
}

#[allow(
    dead_code,
    reason = "scalar metadata entries are required to decode V8's heterogeneous type table"
)]
#[derive(Deserialize)]
#[serde(untagged)]
enum RawHeapFieldType {
    Names(Vec<String>),
    Scalar(String),
}

struct HeapLayout {
    node_width: usize,
    node_count: usize,
    node_type: usize,
    node_name: usize,
    node_id: usize,
    node_self_size: usize,
    node_edge_count: usize,
    node_type_names: Vec<String>,
    edge_width: usize,
    edge_type: usize,
    edge_name: usize,
    edge_target: usize,
    edge_type_names: Vec<String>,
}

struct HeapEdge {
    target_index: usize,
    edge_type: String,
    edge_name: String,
}

impl HeapLayout {
    fn new(snapshot: &RawHeapSnapshot) -> Result<Self, BrowserError> {
        let meta = &snapshot.snapshot.meta;
        let node_width = meta.node_fields.len();
        let edge_width = meta.edge_fields.len();
        if node_width == 0 || !snapshot.nodes.len().is_multiple_of(node_width) {
            return Err(heap_format("node table has an invalid width"));
        }
        if edge_width == 0 || !snapshot.edges.len().is_multiple_of(edge_width) {
            return Err(heap_format("edge table has an invalid width"));
        }
        let node_type = field_index(&meta.node_fields, "type", "node")?;
        let edge_type = field_index(&meta.edge_fields, "type", "edge")?;
        Ok(Self {
            node_width,
            node_count: snapshot.nodes.len() / node_width,
            node_type,
            node_name: field_index(&meta.node_fields, "name", "node")?,
            node_id: field_index(&meta.node_fields, "id", "node")?,
            node_self_size: field_index(&meta.node_fields, "self_size", "node")?,
            node_edge_count: field_index(&meta.node_fields, "edge_count", "node")?,
            node_type_names: type_names(&meta.node_types, node_type, "node")?.to_vec(),
            edge_width,
            edge_type,
            edge_name: field_index(&meta.edge_fields, "name_or_index", "edge")?,
            edge_target: field_index(&meta.edge_fields, "to_node", "edge")?,
            edge_type_names: type_names(&meta.edge_types, edge_type, "edge")?.to_vec(),
        })
    }

    fn node_value(
        &self,
        snapshot: &RawHeapSnapshot,
        node_index: usize,
        field: usize,
    ) -> Option<u64> {
        snapshot
            .nodes
            .get(
                node_index
                    .checked_mul(self.node_width)?
                    .checked_add(field)?,
            )
            .copied()
    }

    fn node_id(&self, snapshot: &RawHeapSnapshot, node_index: usize) -> Option<u64> {
        self.node_value(snapshot, node_index, self.node_id)
    }

    fn node_edge_count(
        &self,
        snapshot: &RawHeapSnapshot,
        node_index: usize,
    ) -> Result<usize, BrowserError> {
        let value = self
            .node_value(snapshot, node_index, self.node_edge_count)
            .ok_or_else(|| heap_format(format!("node {node_index} has no edge count")))?;
        usize::try_from(value).map_err(|_| heap_format("node edge count does not fit usize"))
    }

    fn public_node(
        &self,
        snapshot: &RawHeapSnapshot,
        node_index: usize,
        distance: u8,
        retains_node_id: Option<u64>,
        edge_type: Option<String>,
        edge_name: Option<String>,
    ) -> Result<BrowserHeapRetainerNode, BrowserError> {
        let name_index = self
            .node_value(snapshot, node_index, self.node_name)
            .ok_or_else(|| heap_format(format!("node {node_index} has no name")))?;
        let name_index =
            usize::try_from(name_index).map_err(|_| heap_format("node name does not fit usize"))?;
        let name = snapshot
            .strings
            .get(name_index)
            .ok_or_else(|| heap_format(format!("node string {name_index} is unavailable")))?
            .clone();
        let node_type_index = self
            .node_value(snapshot, node_index, self.node_type)
            .ok_or_else(|| heap_format(format!("node {node_index} has no type")))?;
        let node_type_index = usize::try_from(node_type_index)
            .map_err(|_| heap_format("node type does not fit usize"))?;
        let node_type = self
            .node_type_names
            .get(node_type_index)
            .ok_or_else(|| heap_format(format!("node type {node_type_index} is unavailable")))?
            .clone();
        let class_name = match node_type.as_str() {
            "object" | "native" => name.clone(),
            "array" => "(array)".to_owned(),
            "string" => "(string)".to_owned(),
            "code" => "(compiled code)".to_owned(),
            "closure" => "(closure)".to_owned(),
            "regexp" => "(regexp)".to_owned(),
            "number" => "(number)".to_owned(),
            "synthetic" => "(synthetic)".to_owned(),
            "concatenated string" => "(concatenated string)".to_owned(),
            "sliced string" => "(sliced string)".to_owned(),
            "bigint" => "(bigint)".to_owned(),
            "hidden" => "(system)".to_owned(),
            _ => "(unknown)".to_owned(),
        };
        Ok(BrowserHeapRetainerNode {
            node_id: self
                .node_id(snapshot, node_index)
                .ok_or_else(|| heap_format(format!("node {node_index} has no identifier")))?,
            name,
            class_name,
            node_type,
            self_size: self
                .node_value(snapshot, node_index, self.node_self_size)
                .ok_or_else(|| heap_format(format!("node {node_index} has no self size")))?,
            distance,
            retains_node_id,
            edge_type,
            edge_name,
        })
    }

    fn edge(&self, snapshot: &RawHeapSnapshot, offset: usize) -> Result<HeapEdge, BrowserError> {
        let end = offset
            .checked_add(self.edge_width)
            .ok_or_else(|| heap_format("edge offset overflowed"))?;
        let fields = snapshot
            .edges
            .get(offset..end)
            .ok_or_else(|| heap_format(format!("edge at offset {offset} is unavailable")))?;
        let edge_type_index = usize::try_from(fields[self.edge_type])
            .map_err(|_| heap_format("edge type does not fit usize"))?;
        let edge_type = self
            .edge_type_names
            .get(edge_type_index)
            .ok_or_else(|| heap_format(format!("edge type {edge_type_index} is unavailable")))?
            .clone();
        let target_offset = usize::try_from(fields[self.edge_target])
            .map_err(|_| heap_format("edge target does not fit usize"))?;
        if target_offset % self.node_width != 0 {
            return Err(heap_format(format!(
                "edge target offset {target_offset} is not node-aligned"
            )));
        }
        let target_index = target_offset / self.node_width;
        if target_index >= self.node_count {
            return Err(heap_format(format!(
                "edge target node {target_index} is unavailable"
            )));
        }
        let edge_name_value = fields[self.edge_name];
        let edge_name = if matches!(edge_type.as_str(), "element" | "hidden") {
            edge_name_value.to_string()
        } else {
            let index = usize::try_from(edge_name_value)
                .map_err(|_| heap_format("edge name does not fit usize"))?;
            snapshot
                .strings
                .get(index)
                .ok_or_else(|| heap_format(format!("edge string {index} is unavailable")))?
                .clone()
        };
        Ok(HeapEdge {
            target_index,
            edge_type,
            edge_name,
        })
    }
}

fn field_index(fields: &[String], field: &str, table: &str) -> Result<usize, BrowserError> {
    fields
        .iter()
        .position(|candidate| candidate == field)
        .ok_or_else(|| heap_format(format!("{table} table is missing `{field}`")))
}

fn type_names<'a>(
    fields: &'a [RawHeapFieldType],
    index: usize,
    table: &str,
) -> Result<&'a [String], BrowserError> {
    match fields.get(index) {
        Some(RawHeapFieldType::Names(names)) => Ok(names),
        _ => Err(heap_format(format!(
            "{table} type metadata at field {index} is not an enum"
        ))),
    }
}

fn heap_format(message: impl Into<String>) -> BrowserError {
    BrowserError::HeapSnapshotFormat {
        message: message.into(),
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "V8 sample microseconds are exposed as a human-facing millisecond estimate"
)]
fn summarize_cpu(profile: &Profile, path: PathBuf) -> BrowserCpuProfile {
    let nodes = profile
        .nodes
        .iter()
        .map(|node| (node.id, node))
        .collect::<HashMap<_, _>>();
    let mut aggregate = HashMap::<(String, String, i64, i64), (u64, u64)>::new();
    if let Some(samples) = &profile.samples {
        for (index, sample) in samples.iter().enumerate() {
            let Some(node) = nodes.get(sample) else {
                continue;
            };
            let key = (
                node.call_frame.function_name.clone(),
                node.call_frame.url.clone(),
                node.call_frame.line_number.saturating_add(1),
                node.call_frame.column_number.saturating_add(1),
            );
            let delta = profile
                .time_deltas
                .as_ref()
                .and_then(|deltas| deltas.get(index))
                .copied()
                .unwrap_or_default()
                .max(0);
            let entry = aggregate.entry(key).or_default();
            entry.0 = entry.0.saturating_add(1);
            entry.1 = entry
                .1
                .saturating_add(u64::try_from(delta).unwrap_or(u64::MAX));
        }
    }
    let mut functions = aggregate
        .into_iter()
        .map(
            |((function_name, url, line_number, column_number), (samples, microseconds))| {
                BrowserCpuFunction {
                    function_name,
                    url,
                    line_number,
                    column_number,
                    samples,
                    estimated_self_time_ms: microseconds as f64 / 1_000.0,
                }
            },
        )
        .collect::<Vec<_>>();
    functions.sort_unstable_by(|left, right| {
        right
            .estimated_self_time_ms
            .total_cmp(&left.estimated_self_time_ms)
    });
    functions.truncate(MAX_CPU_FUNCTIONS);
    BrowserCpuProfile {
        path,
        duration_ms: (profile.end_time - profile.start_time).max(0.0) / 1_000.0,
        sample_count: profile.samples.as_ref().map_or(0, Vec::len),
        functions,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CoverageArtifact<'a> {
    timestamp: f64,
    scripts: &'a [ScriptCoverage],
}

fn summarize_coverage(scripts: &[ScriptCoverage], path: PathBuf) -> BrowserCoverage {
    let mut summaries = scripts
        .iter()
        .map(summarize_script_coverage)
        .filter(|summary| summary.total_bytes > 0)
        .collect::<Vec<_>>();
    summaries.sort_unstable_by_key(|summary| std::cmp::Reverse(summary.unused_bytes));
    let total_bytes = summaries.iter().map(|script| script.total_bytes).sum();
    let used_bytes = summaries.iter().map(|script| script.used_bytes).sum();
    BrowserCoverage {
        path,
        scripts: summaries,
        total_bytes,
        used_bytes,
        unused_bytes: total_bytes.saturating_sub(used_bytes),
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "coverage ratios are diagnostic floating-point summaries of exact byte counters"
)]
fn summarize_script_coverage(script: &ScriptCoverage) -> BrowserScriptCoverage {
    let ranges = script
        .functions
        .iter()
        .flat_map(|function| &function.ranges)
        .map(|range| (range.start_offset, range.end_offset, range.count))
        .collect::<Vec<_>>();
    let (total_bytes, used_bytes) = coverage_bytes(&ranges);
    BrowserScriptCoverage {
        url: script.url.clone(),
        total_bytes,
        used_bytes,
        unused_bytes: total_bytes.saturating_sub(used_bytes),
        used_ratio: if total_bytes == 0 {
            0.0
        } else {
            used_bytes as f64 / total_bytes as f64
        },
    }
}

fn coverage_bytes(ranges: &[(i64, i64, i64)]) -> (u64, u64) {
    let mut changes = BTreeMap::<i64, Vec<CoverageChange>>::new();
    let mut first = i64::MAX;
    let mut last = i64::MIN;
    for (index, &(start, end, count)) in ranges.iter().enumerate() {
        if start < 0 || end <= start {
            continue;
        }
        first = first.min(start);
        last = last.max(end);
        let key = (end - start, index);
        changes
            .entry(start)
            .or_default()
            .push(CoverageChange::Start { key, count });
        changes
            .entry(end)
            .or_default()
            .push(CoverageChange::End { key });
    }
    if first == i64::MAX {
        return (0, 0);
    }
    let mut active = BTreeMap::<(i64, usize), i64>::new();
    let mut previous = first;
    let mut used = 0_u64;
    for (offset, changes) in changes {
        if offset > previous
            && active
                .first_key_value()
                .is_some_and(|(_, count)| *count > 0)
        {
            used = used
                .saturating_add(u64::try_from(offset.saturating_sub(previous)).unwrap_or_default());
        }
        for change in changes
            .iter()
            .filter(|change| matches!(change, CoverageChange::End { .. }))
        {
            let CoverageChange::End { key } = change else {
                continue;
            };
            active.remove(key);
        }
        for change in changes {
            if let CoverageChange::Start { key, count } = change {
                active.insert(key, count);
            }
        }
        previous = offset;
    }
    (
        u64::try_from(last.saturating_sub(first)).unwrap_or_default(),
        used,
    )
}

enum CoverageChange {
    Start { key: (i64, usize), count: i64 },
    End { key: (i64, usize) },
}

fn signed_delta(after: u64, before: u64) -> i64 {
    if after >= before {
        i64::try_from(after - before).unwrap_or(i64::MAX)
    } else {
        -i64::try_from(before - after).unwrap_or(i64::MAX)
    }
}

fn signed_delta_usize(after: usize, before: usize) -> i64 {
    if after >= before {
        i64::try_from(after - before).unwrap_or(i64::MAX)
    } else {
        -i64::try_from(before - after).unwrap_or(i64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{analyze_heap_retainers, coverage_bytes};

    #[test]
    fn precise_coverage_prefers_nested_function_ranges() {
        let (total, used) = coverage_bytes(&[(0, 100, 1), (10, 40, 1), (50, 90, 0), (60, 70, 1)]);
        assert_eq!(total, 100);
        assert_eq!(used, 70);
    }

    #[test]
    fn heap_retainers_walk_strong_edges_toward_the_root() {
        let snapshot = br#"{
  "snapshot": {
    "meta": {
      "node_fields": ["type", "name", "id", "self_size", "edge_count", "trace_node_id", "detachedness"],
      "node_types": [
        ["hidden", "array", "string", "object", "code", "closure", "regexp", "number", "native", "synthetic", "concatenated string", "sliced string", "symbol", "bigint"],
        "string", "number", "number", "number", "number", "number"
      ],
      "edge_fields": ["type", "name_or_index", "to_node"],
      "edge_types": [
        ["context", "element", "property", "internal", "hidden", "shortcut", "weak"],
        "string_or_number", "node"
      ]
    }
  },
  "nodes": [
    9, 1, 1, 0, 1, 0, 0,
    3, 2, 3, 16, 0, 0, 0
  ],
  "edges": [2, 3, 7],
  "strings": ["", "(GC roots)", "RetainedFixture", "rooted"]
}"#;
        let retainers =
            analyze_heap_retainers(Cursor::new(snapshot), "heap-1".to_owned(), 3, 4, 10)
                .expect("valid retaining graph");

        assert!(!retainers.truncated);
        assert_eq!(retainers.nodes.len(), 2);
        assert_eq!(retainers.nodes[0].node_id, 3);
        assert_eq!(retainers.nodes[0].distance, 0);
        assert_eq!(retainers.nodes[1].node_id, 1);
        assert_eq!(retainers.nodes[1].retains_node_id, Some(3));
        assert_eq!(retainers.nodes[1].edge_type.as_deref(), Some("property"));
        assert_eq!(retainers.nodes[1].edge_name.as_deref(), Some("rooted"));
    }
}
