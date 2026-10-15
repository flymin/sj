<div align="center">

# sj

### Slurm, a few keystrokes away.

Read the queue. Open the log. Inspect the script. Step into your job.

<p>
  <img alt="Version 0.2.0" src="https://img.shields.io/badge/version-0.2.0-8b5cf6?style=flat-square">
  <img alt="Built with Rust 1.99" src="https://img.shields.io/badge/Rust-1.99-f97316?style=flat-square&amp;logo=rust&amp;logoColor=white">
  <img alt="Static binary" src="https://img.shields.io/badge/binary-static-14b8a6?style=flat-square">
  <img alt="Linux x86_64 and aarch64" src="https://img.shields.io/badge/Linux-x86__64%20%7C%20aarch64-6366f1?style=flat-square&amp;logo=linux&amp;logoColor=white">
</p>

**English** · [简体中文](README.zh-CN.md)

[Quick start](#quick-start) · [Installation](#installation) · [Commands](#commands) · [Custom views](#make-the-queue-yours) · [Filters](#filter-any-field) · [Keybindings](#keybindings) · [Development](#development)

<img src="docs/assets/terminal-preview.svg" alt="sj preview: queue with remaining time, output log, submitted script, and container shell picker" width="1100">

<sub>Illustrative session. Job names, nodes, and log contents are examples.</sub>

</div>

`sj` brings four everyday Slurm tasks into one small Rust executable: see what is running, find a job's output, inspect its submitted script, and open a shell on the right node or inside an existing Enroot container.

## Why sj?

| Your next task | One command | What you get |
| --- | --- | --- |
| Check the queue | `sj --me` | Terminal-aware columns, runtime remaining, and space for meaningful job names. |
| Read the output | `sj out 12345` | The sbatch log resolved from Slurm metadata and opened in `less`. |
| Inspect the submission | `sj script 12345` | The submitted script, opened from the beginning with line numbers. |
| Inspect a running job | `sj sh 12345` | A node picker, then a container or host shell. |

- **Room to breathe.** Wide terminals show `TIME_LEFT`, `TIME`, `PARTITION`, and `NODELIST(REASON)`; narrow terminals compact the view.
- **Your queue, your view.** Choose columns from every native Slurm field, set responsive TOML rules, and add terminal colors.
- **Keep what matters.** Match any field with regex or glob, combine conditions, and save defaults in a view.
- **Familiar filters.** Keep using `--me`, `--start`, `-u`, `-j`, `-t`, `-p`, and other `squeue` options.
- **A menu that fits.** Node and container pickers scroll within the terminal height and react to resizing.
- **Containers first.** Enter confirms the highlighted container; the host shell remains the last choice.
- **One executable.** The Linux x86_64 and aarch64 release binaries are statically linked and run without activating a venv or installing a Python or Rust runtime.

## Quick start

```bash
sj --me                       # Your queue, fitted to the terminal
sj --me --start               # Pending jobs and expected start times
sj out 12345                   # Open the job's output at the end
sj script 12345                # Read the submitted script from the beginning
sj sh 12345                    # Pick a node, then a shell
```

Replace `12345` with your job ID. For a new setup, follow [Installation](#installation).

## Installation

### Runtime requirements

| Feature | Available on `PATH` |
| --- | --- |
| Queue | `squeue` |
| Logs | `sacct`, `scontrol`, `less` |
| Submitted scripts | `scontrol`; `sacct` for historical retrieval; `less` for interactive viewing |
| Node and container shells | `scontrol`, `srun`; `enroot` on the target node for container entry |

Release builds support **x86_64 and aarch64 Linux**, using static musl binaries. For `sh`, the executable's path must be accessible on the compute nodes, typically through a shared home directory; login and compute nodes must use a compatible architecture.

### Download a release

Download the archive matching your Linux architecture from [Releases](https://github.com/flymin/sj/releases):

| Architecture (`uname -m`) | Release target |
| --- | --- |
| `x86_64` | `x86_64-unknown-linux-musl` |
| `aarch64` | `aarch64-unknown-linux-musl` |

```bash
SJ_VERSION=0.2.0
SJ_TARGET="$(uname -m)-unknown-linux-musl"
SJ_ARCHIVE="sj-v${SJ_VERSION}-${SJ_TARGET}.tar.gz"
curl -fLO "https://github.com/flymin/sj/releases/download/v${SJ_VERSION}/${SJ_ARCHIVE}"
curl -fLO "https://github.com/flymin/sj/releases/download/v${SJ_VERSION}/SHA256SUMS"
sha256sum --check --ignore-missing SHA256SUMS
tar -xzf "$SJ_ARCHIVE" sj
mkdir -p "$HOME/local/bin"
install -m 755 sj "$HOME/local/bin/sj"
```

Release archives contain the executable, both READMEs, and the terminal preview. The binary needs no Rust or Python runtime; the Slurm commands listed above remain runtime requirements.

### Build and install

The scripts use an **existing uv venv** for compiler tooling: `SJ_VENV` takes priority, followed by the active `VIRTUAL_ENV`, then `$HOME/.venvs/dev`. The examples assume a checkout at `~/local/src/sj`.

```bash
cd ~/local/src/sj

# First-time toolchain setup; skip if already configured.
SJ_VENV="$HOME/.venvs/dev" SJ_UV="$(command -v uv)" ./setup-toolchain.sh

# Check, build, and install the static executable.
SJ_VENV="$HOME/.venvs/dev" SJ_INSTALL_DIR="$HOME/local/bin" ./build.sh

export PATH="$HOME/local/bin:$PATH"
sj -h
```

The build script checks formatting, runs tests and Clippy, builds a release binary, verifies static linkage, and installs it as `~/local/bin/sj`. Build output is saved to `.logs/build.log`; temporary build directories are cleaned up on exit. Local account and filesystem identifiers are replaced with environment placeholders in build/setup logs, and compiler paths are remapped inside the binary. Documentation and previews use fictional identifiers.

## Commands

| Command | Purpose |
| --- | --- |
| `sj [q] [OPTIONS]` | Show the queue; `q` is the default command. |
| `sj out JOBID [OPTIONS]` | Open stdout, stderr, or print the resolved log path. |
| `sj script JOBID [OPTIONS]` | View Slurm's submitted script copy, or print it with `--raw`. |
| `sj sh JOBID [NODE_INDEX] [OPTIONS]` | Choose a node and open a container or host shell. |
| `sj config path\|views\|check [--config FILE]` | Locate, list, or validate queue configuration. |
| `sj help [COMMAND]` | Show general or command-specific help. |

Every level accepts **`-h` and `--help`**. `sj -h` introduces the commands; `sj q -h`, `sj out -h`, `sj script -h`, `sj sh -h`, and `sj config -h` each show usage, options, and examples. Help works without contacting Slurm.

### Queue · `q`

```bash
sj q --me --start
sj q --me -t RUNNING -i 5
sj q -u "${USER}" -p compute --width 120
sj q --me --noheader
sj q --me --json
```

The layout reserves space for job names. Wide screens show **`TIME_LEFT`** alongside **`TIME`** (elapsed), plus `PARTITION` and native `NODELIST(REASON)`. As the terminal narrows, `TIME_LEFT` is hidden first. Remaining time is Slurm's unused runtime under the job's time limit, rather than an estimate of when the workload will finish. Pending jobs show their unused time limit.

If more compression is needed, time, `USER`, and `JOBID` can shrink to **5**, **5**, and **7** display columns respectively; shorter fields are not padded up to these minimums. Long durations become `1d02h`, `1h23m`, or `12h`; unlimited and unknown values become `∞` and `?` when necessary. Job IDs end with `…`, while usernames are truncated directly. Full values return when space permits; native formats or `--json` retain complete values regardless of terminal width. Long node lists use an ellipsis, and narrow pending reasons use labels such as `Prio`, `Res`, or `Dep`.

`--start` shows pending jobs in expected-start order, following native Slurm behavior, and keeps `START_TIME` as the main time column; wide screens also include `TIME_LEFT`. `-i 5` refreshes every five seconds, recalculating the width and fetching the latest remaining time. Without `--me`, user visibility follows `squeue` defaults.

In an interactive terminal, `-i` adds a watch-style status row above the queue:

```text
Every 5s                           Updated: 2030-01-02 03:04:05 +0000
```

The timestamp records the latest successful update in local time, with its UTC offset. Narrow terminals shorten the timestamp to the clock time or split the status across two lines. `--noheader` hides only the table's column headers; refresh metadata remains visible. Redirected snapshots and native formats such as JSON keep their original output without this status row.

Use **`--noheader`** to hide column headers. In `sj`, `-h` always means help. Explicit formats (`-o`, `-O`), `--long`, and JSON/YAML use native `squeue` output. Steps use the configured adaptive step view unless an explicit native output option is supplied.

### Filter any field

Use **`--filter EXPR`** to keep matching jobs or steps. Combine conditions with `and`, `or`, `not`, and parentheses; exclude a group by negating it. Filters can reference any full field in the selected Slurm catalog, including fields hidden by your view.

```bash
sj --me --filter 'Name =~ "^train" and State == "RUNNING"'
sj q --filter '(Name glob "train-*" or Partition glob "debug-*") and not (Name glob "*test*")'
sj q --steps --filter 'StepName glob "worker-*"'
sj q --filter 'Account == "research"' --filter 'Name !~ "test|debug"'
sj q --no-view-filter --filter 'Name glob "eval-*"'
```

| Operator | Meaning |
| --- | --- |
| `==`, `!=` | Exact string equality / inequality. |
| `=~`, `!~` | Regex search / negated regex search. Use `^` and `$` for a complete match; `(?i)` enables case-insensitive regex matching. |
| `glob` | Whole-value Unix shell pattern: `*`, `?`, `[abc]`, `[!abc]`; wildcards also match `/` and leading dots. Literal wildcard characters use brackets, such as `[*]`. |
| `not` / `!` | Negate a condition or group. |
| `and` / `&&` | Both conditions must hold. |
| `or` / `||` | Either condition may hold. |
| `(...)` | Override the default grouping. |

Comparison binds first, then **NOT → AND → OR**; AND and OR short-circuit. Logical words and field names are case-insensitive; values are case-sensitive by default. Comparisons use strings, including numeric-looking fields. Every referenced field and pattern is validated before querying jobs, even inside a branch that would short-circuit.

Values must be quoted. **Single quotes** inside the expression preserve text literally, useful for regex backslashes. **Double quotes** inside the expression use JSON escapes. Shell quoting is a separate outer layer:

```bash
sj q --filter "Name =~ '^train-\d+$'"       # Literal regex string inside the expression
sj q --filter 'Name =~ "^train-\\d+$"'     # JSON-escaped regex string inside the expression
```

sj does not expand variables itself. Use your shell's quoting rules when expanding `${USER}` or other environment variables. Regex syntax follows the Rust [regex crate](https://docs.rs/regex/latest/regex/#syntax), and glob syntax follows the [glob crate](https://docs.rs/glob/latest/glob/struct.Pattern.html)'s Unix shell patterns; neither evaluates shell commands or searches the filesystem.

Save a default expression in a named view:

```toml
[views.queue]
filter = 'not (Name glob "*test*")'
```

The effective condition is **view filter AND every command-line filter**. Repeating `--filter` adds an AND condition; write OR inside an expression. `--no-view-filter` ignores only the selected view's default filter, leaving command-line and native Slurm filters active. Width rules affect layout only and cannot define filters.

Native `squeue` filters run first. sj then tests the original native field strings, before display cleanup, reason abbreviations, time compaction, truncation, or colors. For example, `Reason == "None"` tests the native string even if its displayed cell is blank. Only retained records determine column widths. No matches produce the normal headers, or empty output with `--noheader`.

Expressions and patterns are compiled at startup and reused for each `-i` refresh. Updated records are filtered again; resizing changes the layout without changing the conditions. Edit a view filter and restart sj to reload it. `sj config check` validates all configured filter expressions, patterns, and referenced fields without querying jobs.

`--filter` and `--no-view-filter` require adaptive output and cannot be combined with `--list-fields`, JSON/YAML, `-o`, `-O`, or other native output options. Native output continues to bypass configuration. Errors identify the expression position; configuration errors also identify the file and view.

### Make the queue yours

Named views, reusable columns, responsive rules, and a small terminal theme live in one **TOML** file. The first adaptive queue run creates a fully commented configuration at `${XDG_CONFIG_HOME}/sj/config.toml`, falling back to `${HOME}/.config/sj/config.toml` when XDG is unset, empty, or relative. Existing files are never overwritten. If this default location cannot be created, sj warns and uses built-ins; an existing invalid file or an explicitly requested missing/invalid file is an error.

```bash
sj config path                               # Find your configuration
sj config views                              # List named views
sj config check                              # Validate TOML and local Slurm fields
sj q --list-fields                           # All job fields on this Slurm version
sj q --steps --list-fields                    # Step fields have their own catalog
sj q --config ./sj.toml --view monitor --me -i5
sj --view compact --me                       # The queue shortcut accepts these options
sj q --steps                                 # Adaptive layout for job steps, too
```

Help, version, field listing, native output, and `sj config` commands do not create a file. `config check` reads the local job/step field catalogs without querying jobs. `--config` also works before an explicit `q` or `config` command.

Built-in views are **`queue`**, **`start`**, **`steps`**, and **`compact`**. Root `default_view`, `start_view`, and `steps_view` select their defaults. `--view` wins over those settings; otherwise `--steps` takes precedence over `--start`. Every full named field reported by the selected Slurm catalog can be a column, including fields such as `Account`, `QOS`, `NumCPUs`, and `WorkDir`. Names are case-insensitive; abbreviated field names are rejected.

Slurm versions without `--helpFormat` use a built-in compatibility catalog containing named fields shared by upstream 22.05 and 23.11. `--list-fields` labels this fallback on stderr; version-specific fields outside that catalog remain accessible through explicit native output. Other command failures still produce errors. `config check` verifies that `default_view` and `start_view` work with job fields, and `steps_view` works with step fields.

A custom configuration can be a small patch over the built-ins. Maps merge by property; arrays replace. To change a built-in view's order without retaining its responsive rules, set `rules = []`. Here is a complete custom view:

```toml
schema_version = 1
default_view = "monitor"

[columns.Name]
max_width = 60

[views.monitor]
order = ["JobId", "Name", "StateCompact", "TimeLeft"]

[views.monitor.columns.Name]
preferred_width = 32
align = "left"

[[views.monitor.rules]]
min_width = 120
order = [
  "JobId", "Partition", "Name", "UserName",
  "StateCompact", "TimeLeft", "TimeUsed", "ReasonList"
]

[views.monitor.rules.columns.UserName]
max_width = 12

[views.monitor.rules.theme.header]
fg = "#8be9fd"

[theme]
header = { fg = "cyan", bold = true }
status = { fg = "bright_black" }
state_scope = "state"

[theme.states]
RUNNING = { fg = "green" }
PENDING = { fg = "yellow" }
```

Column definitions live under `[columns.<Field>]`, with view overrides under `[views.<Name>.columns.<Field>]` and rule overrides under `[views.<Name>.rules.columns.<Field>]`.

| Property | Meaning / default for a new column |
| --- | --- |
| `title` | Header text; defaults to the field name. |
| `format` | `auto`, `raw`, `duration`, `timestamp`, or `reason`; `auto` recognizes common Slurm time/reason fields. Durations accept Slurm clock formats and integer seconds (including `PendingTime`). |
| `align` | `left` (default) or `right`. |
| `min_width` | Compression floor in display cells; default `1`. Short content stays short. |
| `preferred_width` | Initial allocation, bounded by the natural width and floor; omitted means natural width. |
| `max_width` | Maximum display cells; omitted means no cap. |
| `overflow` | `ellipsis` (default) or `truncate`. |
| `optional` | Whether the solver can hide the column; default `false`. |
| `shrink_priority` | Ascending priority for shrinking toward the floor; default `20`. |
| `hide_priority` | Ascending priority for hiding an optional column; default `70`. |
| `grow_priority` | Ascending priority for receiving spare space; default `10`. |
| `style` | Column cell style: `fg`, `bg`, `bold`, `dim`, `italic`, `underline`. |

Natural width is the widest header or formatted value, capped by `max_width`. Shrink and hide actions share one priority scale: lower numbers run first, ties shrink before hiding, then follow column order. Only the needed amount is shrunk. Remaining space grows visible columns toward natural width. If mandatory columns still cannot fit, each job becomes a wrapped stack of `title: value` lines with complete mandatory values. `--noheader` omits those labels.

Rules have inclusive `min_width` / `max_width` bounds; either can be omitted. All matching rules apply in file order, with later properties winning. Each rule can change `order`, column properties, and theme properties. Configuration is read **once at startup**; `-i` re-evaluates rules from the base configuration after each query, including terminal resizes. Edit the file and restart sj to reload it.

Themes style `header`, `row`, and watch `status`. Colors accept the 16 names (`black` through `white`, plus `bright_*`), an index `0..255`, `"#RRGGBB"`, or `"reset"`. Styles inherit property by property: `false` clears an inherited attribute; `fg` / `bg = "reset"` clears a color. Theme overrides follow global → view → matching rules. `theme.states` uses full state names; `state_scope = "state"` colors state cells and `"row"` colors the whole row. Cell style precedence is row → row state → column → cell state, with only the chosen state scope applied.

`--color auto` uses colors on terminals, honors `NO_COLOR`, and disables styling for `TERM=dumb`. `--color always` also colors redirected output; `--color never` disables styling. Padding, truncation, and wrapping use Unicode display widths before ANSI styling, with resets at each cell or line. Native output stays untouched and cannot be combined with `--view`.

### Logs · `out`

```bash
sj out 12345                       # stdout, opened in less
sj out 12345 --err                 # stderr, or the shared output file
sj out 12345 --path                # Print the path; no pager
sj out 12340_3                     # An exact job-array task
```

The path comes from Slurm metadata, including accounting records for completed jobs. Relative paths resolve against the job's working directory. Specify an exact array task when the parent ID maps to multiple logs.

Logs open with `less -R +G`, starting at the end. Press **F** to follow new output, **Ctrl-C** to stop following, and **q** to quit. `--path` can print a known path before the file has been created; opening a log requires the file to be readable on the current host.

### Submitted scripts · `script`

```bash
sj script 12345                 # Open Slurm's submitted copy in less
sj script 12345 --raw           # Print the original script bytes
sj script 12345 > job.sbatch    # Save the submitted copy
sj script 12340_3               # An exact array task
```

Interactive terminals open `less -N +g` at the beginning, with line numbers. Redirected output and `--raw` contain only the original script, preserving its whitespace and final newline. Viewing reads the copy retained by Slurm, including scripts generated by `sbatch --wrap`; later edits or deletion of the local submission file do not change that copy.

Retrieval tries the controller first, then accounting for historical records. Historical scripts require the cluster's existing `AccountingStoreFlags=job_script` setting and retained records. Missing scripts or insufficient permissions produce an error. Array parents or heterogeneous jobs matching multiple allocations require an exact task/component ID. No temporary script file is created.

### Shells · `sh`

```bash
sj sh 12345                            # Choose a node, then a shell
sj sh 12345 0                          # Choose a shell on node 0
sj sh 12345 --nodes                    # List allocation indices
sj sh 12345 0 --list                   # List running containers
sj sh 12345 0 --host                   # Enter the host directly
sj sh 12345 0 --container container-demo  # Select by exact container name
sj sh 12345 0 --container 23456        # Select by PID from --list
```

Node indices start at **0** and preserve Slurm's original allocation order. A single node is selected automatically; multiple nodes open a picker when no index is supplied.

Containers appear first, with **host shell as the last selectable item**. The first container is highlighted by default, so Enter opens it. With no running containers, the host shell is the only option. The list includes the current user's visible running Enroot containers on the selected node, with their names, representative PIDs, and job IDs when identifiable.

`--container` accepts a name or **PID**, not a menu index. Use a PID to distinguish multiple running instances with the same name. `--nodes`, `--list`, `--host`, and `--container` are mutually exclusive.

Shell access requires a **RUNNING job owned by the current user** and an interactive terminal. For scripts, use `--nodes` or `--list` with an explicit node index on multi-node jobs.

## Keybindings

### Node and container menus

| Key | Action |
| --- | --- |
| ↑ / ↓ | Move the highlight. |
| PageUp / PageDown | Move one visible page. |
| Home / End | Jump to the first or last item. |
| Digits, then Enter | Select a zero-based index. |
| Backspace | Correct the numeric entry. |
| Enter | Confirm the highlighted item. |
| q / Esc / Ctrl-C / Ctrl-D | Cancel the selection. |

Short lists are shown in full; long lists scroll within the available height. Resizing adjusts the viewport. Menus restore the terminal screen, keyboard settings, and cursor when they close.

### Log viewer

| Key | Action |
| --- | --- |
| g / G | Jump to the beginning / end. |
| F | Follow new output. |
| Ctrl-C | Stop following and return to browsing. |
| q | Close the viewer. |

## Development

The build uses **Rust 1.99.0**, edition 2024, the `aarch64-unknown-linux-musl` target, and the bundled `rust-lld`. Dependencies are pinned in `Cargo.lock`.

```bash
./build.sh
```

Tests cover configuration generation and concurrent creation, patch/rule validation, field catalogs, job/step views, boolean filters and raw values, view filter composition, themes, queue layout, compact durations, remaining-time refreshes, Unicode, native option forwarding, log resolution, array IDs, script retrieval and byte preservation, node ordering, container discovery, and CLI help. PTY tests exercise script paging and early exit, arrow keys, numeric entry, resizing, cancellation, and terminal restoration. Build-script tests check environment selection and log anonymization. Slurm-facing integration tests use temporary command mocks.

### GitHub Actions

[CI and release](https://github.com/flymin/sj/actions/workflows/ci.yml) runs on pull requests, pushes to `main`, and version tags. Each architecture uses a native GitHub-hosted runner and Rust 1.99.0 to run formatting, Python build/release tests, Rust tests, Clippy, a static release build, and binary smoke checks. Successful runs upload archives and per-archive checksums as Actions artifacts for 14 days.

To publish a version, update `Cargo.toml`, the `sj` entry in `Cargo.lock`, and both README version badges, then commit and push. Create a tag matching the package version:

```bash
git tag v0.2.0
git push origin v0.2.0
```

The tag must exactly match `v` plus the package version. Only after both architectures pass does the workflow create a draft Release, upload both archives and `SHA256SUMS`, and publish it. Prerelease versions such as `v0.2.0-rc.1` are marked as prereleases. Already published assets are kept intact on reruns; a failed draft upload can be retried. Use Actions **Run workflow** on a branch for a build, or on a matching version tag to retry a release.

Publishing uses GitHub's automatic `GITHUB_TOKEN`; no personal token is required. Build jobs have read access, while only the release job gets `contents: write`. Local toolchain setup remains available through `build.sh`; Actions uses its own pinned toolchain and `scripts/ci-build.sh`.

<details>
<summary><strong>Advanced behavior &amp; portability</strong></summary>

### Queue rendering

- Width comes from the terminal, then `COLUMNS`, then a 120-column fallback. Extremely narrow views reduce auxiliary columns or use multiple lines. Job IDs can be abbreviated in the table; the stacked fallback wraps complete mandatory field values.
- Iteration accepts `-i 5`, `-i5`, and `--iterate=5`. TTY output redraws with the refresh interval and successful update time; redirected output appends snapshots without metadata.
- Narrow start-time columns use month/day and hour/minute; wide columns show the full timestamp.
- For multi-cluster output with native cluster headings, use an explicit native format or JSON/YAML.

### Log resolution

- `sacct --expand-pattern` resolves recorded paths; unavailable accounting data falls back to `scontrol` JSON.
- The fallback expands `%j`, `%A`, `%a`, `%b`, `%x`, `%u`, `%%`, and numeric padding. Node/task-specific patterns rely on Slurm's native expansion.
- The tool uses recorded paths rather than recursively searching the filesystem. Logs must be mounted and readable on the host running `out`.

### Shell execution

- Connections use one overlapping `srun` step with `--nodes=1`, `--ntasks=1`, `--cpus-per-task=1`, `--overlap`, `--exact`, and `--pty`. Device access follows the allocation; step resource waiting uses `--immediate=10`.
- The picker uses the original JSON allocation order. The interactive step itself has its own node rank, usually `SLURM_NODEID=0` for this single-node step.
- Discovery groups accessible processes by mount namespace and checks Enroot's `/.lock` marker, independent of `ENROOT_DATA_PATH`. Cached root filesystems are not listed as running containers.
- Before entry, the PID, process start time, and namespace are checked. `enroot exec` joins the existing container and restores its environment and working directory.
- Container shells prefer `/bin/bash -l`, then `/bin/sh -l`. Host shells use `$SHELL -l`, falling back to bash/sh.

### Toolchain setup

- Compiler tools live inside the selected venv under `opt/rust-1.99.0`; its `bin` directory provides the Rust command entry points. Cargo's cache lives under `share/cargo`.
- Setup installs patchelf into that venv and unpacks isolated musl/libgcc runtime libraries there. It does not edit `pyvenv.cfg` or activation scripts, or install system packages.
- Rust downloads are checked against the release manifest's SHA256 values; runtime library versions and SHA256 values are fixed in the setup script.
- `SJ_VENV`, `SJ_UV`, and `SJ_INSTALL_DIR` select the existing environment, uv executable, and installation directory. `SJ_UV` defaults to `uv` on `PATH`, then `$HOME/.local/bin/uv`; installation defaults to `$HOME/local/bin`. Setup skips an already working toolchain.
- The delivered executable has no shared-library dependency, but still calls the cluster's Slurm tools, `less`, and `enroot` as appropriate.

</details>

---

<div align="center">

README presentation inspired by [sqwatch](https://github.com/fedonman/sqwatch).

[Back to top](#sj) · [简体中文](README.zh-CN.md)

</div>
