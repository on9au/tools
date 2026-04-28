# upstream-migration-planner

`upstream-migration-planner` creates an upstream-first migration order from a dependency graph.

Given a graph of downstream-to-upstream dependencies, it produces phases that move prerequisites first and only schedule dependents after their upstream requirements are ready.

## Input model

The tool reads JSON with this shape:

```json
{
  "target": "repo-a/component-x",
  "nodes": [
    "repo-a/component-x",
    "repo-b/component-y"
  ],
  "edges": [
    {
      "from": "repo-a/component-x",
      "to": "repo-b/component-y"
    }
  ]
}
```

Input edges are interpreted as `downstream -> upstream`.

## Output

The generated plan groups nodes into phases. Each planned node includes:

- the original node id
- parsed repository and component names
- whether the node is the target
- downstream reach
- downstream depth

Output formats:

- `text` (default)
- `json`

## Usage

Read input from a file:

```powershell
cargo run -p upstream-migration-planner -- --input .\graph.json
```

Emit JSON instead of text:

```powershell
cargo run -p upstream-migration-planner -- --input .\graph.json --format json
```

Pipe JSON through stdin:

```powershell
Get-Content .\graph.json | cargo run -p upstream-migration-planner --
```

## Validation behavior

The tool fails when:

- the target is missing from `nodes`
- the node list contains duplicates
- an edge references an unknown node
- the graph contains a cycle

That makes it useful as a lightweight planning step before working through a migration with real repositories or components.
