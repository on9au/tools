use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap, HashMap, VecDeque};
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};

/// Plans an upstream-first migration order from a dependency graph.
#[derive(Debug, Parser)]
#[command(
    name = "upstream-migration-planner",
    version,
    about = "Create migration phases that move upstream dependencies before downstream dependents"
)]
struct Cli {
    /// Path to the input JSON file. Reads stdin when omitted.
    #[arg(long)]
    input: Option<PathBuf>,
    /// Output format for the generated migration plan.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug, Deserialize)]
struct GraphInput {
    target: String,
    nodes: Vec<String>,
    edges: Vec<GraphEdge>,
}

#[derive(Debug, Deserialize)]
struct GraphEdge {
    from: String,
    to: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct MigrationPlan {
    target: String,
    phases: Vec<MigrationPhase>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct MigrationPhase {
    phase: usize,
    nodes: Vec<PlannedNode>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct PlannedNode {
    id: String,
    repository: String,
    component: String,
    is_target: bool,
    downstream_reach: usize,
    downstream_depth: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NodeDescriptor {
    repository: String,
    component: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct NodePriority {
    downstream_depth: usize,
    downstream_reach: usize,
}

fn main() -> ExitCode {
    match try_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn try_main() -> Result<()> {
    let cli = Cli::parse();
    let input = read_graph_input(cli.input)?;
    let plan = build_plan(input)?;

    match cli.format {
        OutputFormat::Text => print_text_plan(&plan),
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&plan)
                    .context("failed to serialize migration plan")?
            );
        }
    }

    Ok(())
}

fn read_graph_input(path: Option<PathBuf>) -> Result<GraphInput> {
    let contents = match path {
        Some(path) => fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?,
        None => {
            let mut buffer = String::new();
            io::stdin()
                .read_to_string(&mut buffer)
                .context("failed to read graph JSON from stdin")?;

            if buffer.trim().is_empty() {
                bail!("no input provided; pass --input <path> or pipe JSON on stdin");
            }

            buffer
        }
    };

    serde_json::from_str::<GraphInput>(&contents).context("failed to parse graph JSON")
}

fn build_plan(input: GraphInput) -> Result<MigrationPlan> {
    let GraphInput {
        target,
        nodes,
        edges,
    } = input;

    let known_nodes = nodes.iter().cloned().collect::<BTreeSet<_>>();
    if known_nodes.len() != nodes.len() {
        bail!("nodes list contains duplicates; each node id must be unique");
    }
    if !known_nodes.contains(&target) {
        bail!("target node is not present in the nodes list");
    }

    let mut downstreams = nodes
        .iter()
        .cloned()
        .map(|node| (node, Vec::<String>::new()))
        .collect::<HashMap<_, _>>();
    let mut prerequisites = nodes
        .iter()
        .cloned()
        .map(|node| (node, BTreeSet::<String>::new()))
        .collect::<HashMap<_, _>>();

    for edge in &edges {
        if !known_nodes.contains(&edge.from) {
            bail!("edge references unknown from-node: {}", edge.from);
        }
        if !known_nodes.contains(&edge.to) {
            bail!("edge references unknown to-node: {}", edge.to);
        }

        // Input edges are downstream -> upstream. Reverse them so planning can
        // walk prerequisites first and only release dependents afterwards.
        downstreams
            .get_mut(&edge.to)
            .expect("validated upstream node must exist")
            .push(edge.from.clone());
        prerequisites
            .get_mut(&edge.from)
            .expect("validated downstream node must exist")
            .insert(edge.to.clone());
    }

    for dependents in downstreams.values_mut() {
        dependents.sort();
        dependents.dedup();
    }

    let downstream_depth = compute_downstream_depth(&nodes, &downstreams)?;
    let downstream_reach = compute_downstream_reach(&nodes, &downstreams);
    let mut remaining_prerequisites = prerequisites
        .iter()
        .map(|(node, deps)| (node.clone(), deps.len()))
        .collect::<HashMap<_, _>>();

    let mut ready = BinaryHeap::<(NodePriority, Reverse<String>)>::new();
    for node in &nodes {
        if remaining_prerequisites
            .get(node)
            .copied()
            .unwrap_or_default()
            == 0
        {
            ready.push((
                priority_for(node, &downstream_depth, &downstream_reach),
                Reverse(node.clone()),
            ));
        }
    }

    let mut phases = Vec::new();
    let mut visited = 0usize;

    while !ready.is_empty() {
        let mut phase_queue = VecDeque::new();
        while let Some((_, Reverse(node))) = ready.pop() {
            phase_queue.push_back(node);
        }

        let mut phase_nodes = Vec::new();
        let mut next_ready = BinaryHeap::<(NodePriority, Reverse<String>)>::new();

        while let Some(node) = phase_queue.pop_front() {
            visited += 1;
            let descriptor = parse_node_descriptor(&node);
            phase_nodes.push(PlannedNode {
                repository: descriptor.repository,
                component: descriptor.component,
                is_target: node == target,
                downstream_reach: downstream_reach.get(&node).copied().unwrap_or_default(),
                downstream_depth: downstream_depth.get(&node).copied().unwrap_or_default(),
                id: node.clone(),
            });

            for dependent in downstreams.get(&node).into_iter().flatten() {
                let remaining = remaining_prerequisites
                    .get_mut(dependent)
                    .expect("dependent node must exist");
                *remaining = remaining.saturating_sub(1);
                if *remaining == 0 {
                    next_ready.push((
                        priority_for(dependent, &downstream_depth, &downstream_reach),
                        Reverse(dependent.clone()),
                    ));
                }
            }
        }

        phase_nodes.sort_by(|left, right| {
            right
                .downstream_depth
                .cmp(&left.downstream_depth)
                .then_with(|| right.downstream_reach.cmp(&left.downstream_reach))
                .then_with(|| left.id.cmp(&right.id))
        });

        phases.push(MigrationPhase {
            phase: phases.len() + 1,
            nodes: phase_nodes,
        });

        ready = next_ready;
    }

    if visited != nodes.len() {
        bail!("graph contains a cycle; unable to produce an upstream-first migration plan");
    }

    Ok(MigrationPlan { target, phases })
}

fn compute_downstream_depth(
    nodes: &[String],
    downstreams: &HashMap<String, Vec<String>>,
) -> Result<HashMap<String, usize>> {
    let mut indegree = nodes
        .iter()
        .cloned()
        .map(|node| (node, 0usize))
        .collect::<HashMap<_, _>>();

    for dependents in downstreams.values() {
        for dependent in dependents {
            *indegree
                .get_mut(dependent)
                .ok_or_else(|| anyhow!("unknown node in downstream graph: {dependent}"))? += 1;
        }
    }

    let mut queue = indegree
        .iter()
        .filter_map(|(node, count)| (*count == 0).then_some(node.clone()))
        .collect::<VecDeque<_>>();
    let mut topo = Vec::with_capacity(nodes.len());

    while let Some(node) = queue.pop_front() {
        topo.push(node.clone());
        for dependent in downstreams.get(&node).into_iter().flatten() {
            let count = indegree
                .get_mut(dependent)
                .expect("dependent referenced from graph must exist");
            *count = count.saturating_sub(1);
            if *count == 0 {
                queue.push_back(dependent.clone());
            }
        }
    }

    if topo.len() != nodes.len() {
        bail!("graph contains a cycle; unable to rank upstream depth");
    }

    let mut depth = nodes
        .iter()
        .cloned()
        .map(|node| (node, 0usize))
        .collect::<HashMap<_, _>>();

    for node in topo.into_iter().rev() {
        let best_child_depth = downstreams
            .get(&node)
            .into_iter()
            .flatten()
            .filter_map(|dependent| depth.get(dependent).copied())
            .max()
            .unwrap_or(0);
        let next_depth = if downstreams
            .get(&node)
            .is_some_and(|items| !items.is_empty())
        {
            best_child_depth + 1
        } else {
            0
        };
        depth.insert(node, next_depth);
    }

    Ok(depth)
}

fn compute_downstream_reach(
    nodes: &[String],
    downstreams: &HashMap<String, Vec<String>>,
) -> HashMap<String, usize> {
    nodes
        .iter()
        .cloned()
        .map(|node| {
            let mut visited = BTreeSet::new();
            let mut queue = downstreams
                .get(&node)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .collect::<VecDeque<_>>();

            while let Some(current) = queue.pop_front() {
                if visited.insert(current.clone()) {
                    for next in downstreams.get(&current).into_iter().flatten() {
                        queue.push_back(next.clone());
                    }
                }
            }

            (node, visited.len())
        })
        .collect()
}

fn priority_for(
    node: &str,
    downstream_depth: &HashMap<String, usize>,
    downstream_reach: &HashMap<String, usize>,
) -> NodePriority {
    NodePriority {
        downstream_depth: downstream_depth.get(node).copied().unwrap_or_default(),
        downstream_reach: downstream_reach.get(node).copied().unwrap_or_default(),
    }
}

fn parse_node_descriptor(node: &str) -> NodeDescriptor {
    let (repository, component) = node.split_once('|').unwrap_or((node, ""));
    NodeDescriptor {
        repository: repository.to_string(),
        component: if component.is_empty() {
            String::from("root")
        } else {
            component.to_string()
        },
    }
}

fn print_text_plan(plan: &MigrationPlan) {
    println!("Target: {}", plan.target);
    println!("Phases: {}", plan.phases.len());

    for phase in &plan.phases {
        println!();
        println!("Phase {}", phase.phase);
        for node in &phase.nodes {
            let target_marker = if node.is_target { " [target]" } else { "" };
            println!(
                "- {} | {}{} (depth={}, reach={})",
                node.repository,
                node.component,
                target_marker,
                node.downstream_depth,
                node.downstream_reach
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GraphEdge, GraphInput, build_plan};

    fn node(id: &str) -> String {
        id.to_string()
    }

    #[test]
    fn plans_prerequisites_before_dependents() {
        let plan = build_plan(GraphInput {
            target: node("repo|Framework"),
            nodes: vec![
                node("repo|Common"),
                node("repo|Framework"),
                node("repo|ConnectorA"),
                node("repo|ConnectorB"),
            ],
            edges: vec![
                GraphEdge {
                    from: node("repo|Framework"),
                    to: node("repo|Common"),
                },
                GraphEdge {
                    from: node("repo|ConnectorA"),
                    to: node("repo|Framework"),
                },
                GraphEdge {
                    from: node("repo|ConnectorB"),
                    to: node("repo|Framework"),
                },
            ],
        })
        .expect("plan should build");

        assert_eq!(plan.phases.len(), 3);
        assert_eq!(plan.phases[0].nodes[0].component, "Common");
        assert_eq!(plan.phases[1].nodes[0].component, "Framework");
        assert_eq!(plan.phases[2].nodes.len(), 2);
    }

    #[test]
    fn prefers_more_upstream_nodes_within_a_phase() {
        let plan = build_plan(GraphInput {
            target: node("repo|Target"),
            nodes: vec![
                node("repo|BaseA"),
                node("repo|BaseB"),
                node("repo|LeafA"),
                node("repo|LeafB"),
                node("repo|Target"),
            ],
            edges: vec![
                GraphEdge {
                    from: node("repo|LeafA"),
                    to: node("repo|BaseA"),
                },
                GraphEdge {
                    from: node("repo|Target"),
                    to: node("repo|BaseA"),
                },
                GraphEdge {
                    from: node("repo|LeafB"),
                    to: node("repo|BaseB"),
                },
            ],
        })
        .expect("plan should build");

        let phase_one = &plan.phases[0].nodes;
        assert_eq!(phase_one[0].component, "BaseA");
        assert_eq!(phase_one[1].component, "BaseB");
    }

    #[test]
    fn rejects_cycles() {
        let error = build_plan(GraphInput {
            target: node("repo|A"),
            nodes: vec![node("repo|A"), node("repo|B")],
            edges: vec![
                GraphEdge {
                    from: node("repo|A"),
                    to: node("repo|B"),
                },
                GraphEdge {
                    from: node("repo|B"),
                    to: node("repo|A"),
                },
            ],
        })
        .expect_err("cycle should fail");

        assert!(error.to_string().contains("cycle"));
    }
}
