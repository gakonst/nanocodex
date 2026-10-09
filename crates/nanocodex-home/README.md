# nanocodex-home

One view of user and project agent context across the Codex-native and
Claude-native layouts, so both Nanocodex harnesses see the same instructions,
skills and subagent profiles whichever path a model (or harness) reaches for.

| | Codex convention | Claude convention |
|---|---|---|
| User home | `$CODEX_HOME` or `~/.codex` (canonical) | `$CLAUDE_CONFIG_DIR` or `~/.claude` (alias) |
| Global instructions | `AGENTS.override.md` else `AGENTS.md` | `CLAUDE.md` (+ `rules/**/*.md`) |
| User skills | `~/.codex/skills/<n>/SKILL.md`, `~/.agents/skills/<n>` | `~/.claude/skills/<n>/SKILL.md` |
| Project instructions | `AGENTS.override.md` else `AGENTS.md` per directory | `CLAUDE.md`, `CLAUDE.local.md`, `.claude/CLAUDE.md`, `.claude/rules/**/*.md` |
| Project skills | `.agents/skills` (each directory up to the git root) | `.claude/skills` |
| Subagents | `agents/*.toml` (Codex TOML) | `agents/**/*.md` (Markdown + YAML frontmatter) |

The canonical home stays the only place Nanocodex owns sessions and durable
state. Tool-owned formats (`settings.json`, `config.toml`, auth, `sessions/`,
`projects/`, …; see `NATIVE_OWNED_ENTRIES`) are never shared, linked or written.

## Resolution

```rust
use nanocodex_home::{AgentHome, LinkMode};

let home = AgentHome::from_env()?; // CODEX_HOME / CLAUDE_CONFIG_DIR / HOME
let project = home.project(&workspace); // git root = nearest ancestor with .git

let global = home.global_instructions();            // ordered, deduped sources
let project_docs = project.read_instructions(32 * 1024);
let skills = home.skills_for(&project);             // one winner per name
let profiles = home.agent_profiles_for(&project);   // per format + name
```

Deterministic precedence (first wins; every omission is a `Diagnostic`):

- **Global instructions:** the Codex file (`AGENTS.override.md` when non-empty,
  otherwise `AGENTS.md` — Codex's own rule), then `~/.claude/CLAUDE.md`. Both are
  included; a file reached twice (same canonical path, e.g. a natural-path
  symlink) or with identical text is included once.
- **Project instructions:** root to workspace; per directory the Codex file,
  then `CLAUDE.md`, `CLAUDE.local.md`, `.claude/CLAUDE.md`, then sorted rules.
  Rules are listed (`instruction_files`) but not read by `read_instructions`
  because they are path-scoped. The byte budget is shared, root first.
- **Skills:** user roots `$CODEX_HOME/skills` → `$CLAUDE_CONFIG_DIR/skills` →
  `~/.agents/skills`, then project roots nearest directory first with
  `.claude/skills` before `.agents/skills` (personal skills win, as in Claude
  Code). Names are folder names; hidden folders (e.g. Codex `.system`) are
  skipped; symlinked folders are followed and deduplicated by canonical path.
- **Subagent profiles:** project (`.claude/agents`, `.codex/agents`, nearest
  first) before user (`~/.claude/agents`, `~/.codex/agents`). Markdown and TOML
  profiles are unioned for discovery but never aliased: the formats differ.

## Natural-path links

`home.link_natural_paths(LinkMode::DryRun | LinkMode::Apply)` creates symlinks
on the missing side pointing at the existing side's canonical path:

- `~/.claude/CLAUDE.md -> ~/.codex/AGENTS.md` (or `AGENTS.override.md` when it is
  the only Codex file), or `~/.codex/AGENTS.md -> ~/.claude/CLAUDE.md` when Codex
  has no global file.
- Per skill, `~/.codex/skills/<n>` and `~/.claude/skills/<n>` each link to the
  highest-precedence real folder among the three user skill roots.
  `~/.agents/skills` is a source only.

It never overwrites, removes or follows into existing entries; leaves
both-sides-exist (`BothExist`) and dangling (`DanglingLink`) paths alone and
reports them; creates only the parent directories a link needs; is idempotent
(`AlreadyLinked` on rerun); and on non-Unix platforms reports `Unsupported`
without touching anything. The `LinkReport` (and every resolution type)
implements `Serialize`.

Try it against throwaway homes (never your real ones):

```sh
HOME=$(mktemp -d) cargo run -p nanocodex-home --example natural_paths -- [--apply] [WORKSPACE]
```

## Tests

`cargo test -p nanocodex-home` runs black-box journeys over real temporary
homes and symlinks: environment resolution, union precedence, dedup, dry-run,
apply, idempotency, no-overwrite and dangling-link handling.
