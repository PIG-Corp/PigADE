<p align="center">
  <img src="docs/assets/readme-hero.png" alt="PigADE — Agentic Development Environment. Terminal panels and a small porcelain pig." width="100%">
</p>

<h1 align="center">PigADE</h1>
<p align="center"><strong>CLI agents. One workspace.</strong></p>
<p align="center">Claude Code · Codex · OpenCode · Shell</p>
<p align="center"><code>dev 0.0.1</code> · Linux · Rust + GPUI</p>

---

PigADE is a desktop environment for working with multiple CLI agents in the same project. Open a repository, create a workspace for your task, and arrange your terminals the way you work.

**Project → Workspace → Tabs → Terminal panels**

## Features

- **Real terminals.** Each agent runs in its own CLI session, using its own interface and authentication.
- **Tiling.** Split, move, and resize panels. Use focus mode, undo/redo, and move sessions between tabs and workspaces.
- **Task-based workspaces.** Work across multiple projects, using a shared checkout or a separate Git worktree for each workspace.
- **Shared context.** Project instructions, individual roles, and versioned memory, with explicit control over what reaches each agent.
- **History and continuation.** Browse saved output and previous process generations; explicitly resume a specific conversation through the CLI's native capabilities.
- **Tools close at hand.** Agent profiles, a prompt library, saved layouts, and Git change review with staging, unstaging, and commits.

## Try it

PigADE is currently an **experimental Linux development build**, with no packaged installer. The `main` branch includes unfinished changes; use the last verified snapshot, `87f40c0`, to try the application.

You need Rust/Cargo, a C/C++ toolchain, and GPUI's system dependencies. The verified development environment is Linux x86_64 with Rust 1.98.1. See the [application guide](crates/app/README.md) for build details.

```sh
git clone https://github.com/PIG-Corp/PigADE.git
cd PigADE
git switch --detach 87f40c0

cargo build --locked --release --workspace --bins
crates/app/bootstrap.sh build --locked --release

./crates/app/target/release/pigade-app
```

The application starts its local session host automatically. Install and authenticate Claude Code, Codex, and OpenCode separately. Supported capabilities and verified versions are listed in the [CLI compatibility matrix](docs/compatibility/CLI.md).

After building, add PigADE to your application launcher, including Quickshell:

```sh
python3 scripts/install-desktop.py
```

The launcher uses the latest release binary built in this repository.

## Your first five minutes

1. Open an existing Git repository or create a new project folder from the start screen.
2. A default workspace and the first tab are created automatically.
3. Open a shell or choose a CLI from the agent picker.
4. Arrange your panels using their menus, the mouse, or keyboard controls.
5. Create another workspace for a separate task, optionally using a Git worktree.

A normal launch starts the CLI without delivering shared context. Use the explicit launch configuration actions in the command palette to choose roles, memory, and instructions.

## Controls

| Action | Default |
| --- | --- |
| Command palette | `Ctrl+Shift+Space`, then `P` |
| New shell / new tab | `Ctrl+Enter` / PigADE mode, then `T` |
| Move a panel | Hold `Ctrl` and drag with the left mouse button |
| Resize a panel | Hold `Ctrl` and drag with the right mouse button, or drag a divider |
| Focus a panel | Move the pointer over it |
| Copy selected text | `Ctrl+Shift+C` |

The panel management modifier is configurable in settings. See the [user guide](docs/USAGE.md) for the complete controls.

## Development status

**dev 0.0.1 is not an MVP or beta release yet.** The terminal foundation and core workflows are implemented, but platform coverage, accessibility, performance, and complete recovery acceptance remain unfinished. Historical output is identified separately from a live process; a process from before an OS reboot is not treated as still running.

The current `main` branch also includes unfinished tab ordering and research patches. Known issues and the next steps are recorded in the [development handoff](docs/AGENT_HANDOFF.md). The cover above is an illustration, not an application screenshot.

## Documentation

- [User guide and commands](docs/USAGE.md)
- [Specification](SPEC.md) · [Design](DESIGN.md) · [Architecture](docs/PROJECT_MAP.md)
- [CLI compatibility](docs/compatibility/CLI.md) · [Architecture decisions](docs/adr/)
- [Changelog](CHANGELOG.md) · [Development handoff](docs/AGENT_HANDOFF.md)

The development UI and some linked documents are currently in Russian. Before contributing code, read [AGENTS.md](AGENTS.md). A project license has not been selected yet; third-party licenses are preserved in the repository.
