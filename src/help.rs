use crate::util::{self, Result};

pub const ROOT: &str = "sj — Slurm job helpers

Usage:
  sj <COMMAND> [OPTIONS]
  sj [QUEUE_OPTIONS]                 Run q when no command is specified

Commands:
  q                                 Show an adaptive Slurm queue (default)
  out                               Open a job's sbatch output in less
  script                            View the submitted sbatch script
  sh                                Open a job node or running container shell
  config                            Locate, list, or validate queue configuration
  help [COMMAND]                    Show general or command-specific help

Options:
  -h, --help                        Show this help
  -V, --version                     Show the sj version
      --config FILE                 Choose the queue's TOML configuration
      --view NAME                   Choose a named queue view
      --color auto|always|never      Control queue colors
      --filter EXPR                 Filter queue fields with a boolean expression
      --no-view-filter              Ignore the selected view's default filter

Examples:
  sj --me                           Queue shortcut
  sj q --me --start                 Pending jobs and expected start times
  sj out 12345                       View a job's output log
  sj script 12345                   View the submitted script
  sj sh 12345                        Choose a job node and shell
  sj q -h                           Queue options and examples
  sj config path                    Locate your TOML configuration
  sj help out                       Output-log options and examples

Use sj <COMMAND> -h or sj <COMMAND> --help for that command's options and examples.
";

pub const QUEUE: &str = "sj q — Adaptive Slurm queue

Usage:
  sj q [OPTIONS]
  sj [QUEUE_OPTIONS]                 Shortcut for q

Options:
  -h, --help                        Show this help
      --me                          Show only the current user's jobs
      --start                       Show pending jobs and expected start times
  -i, --iterate SECONDS             Refresh with interval and update time in terminals
      --width COLUMNS               Override automatic terminal width
      --noheader                    Hide column headers
      --config FILE                 Read a TOML configuration at FILE
      --view NAME                   Choose a named queue or step view
      --color auto|always|never      Control terminal colors (default: auto)
      --list-fields                 List native fields (compatibility catalog if needed)
      --filter EXPR                 Keep rows matching EXPR; repeat for AND
      --no-view-filter              Ignore the selected view's default filter
  -s, --steps[=IDS]                 Show job steps using the configured step view

Common squeue filters:
  -u, --user USERS                  Filter users (comma-separated)
  -j, --job JOBIDS                  Filter job IDs (comma-separated)
  -n, --name NAMES                  Filter job names (comma-separated)
  -t, --states STATES               Filter states, e.g. RUNNING or PD
  -p, --partition PARTITIONS        Filter partitions (comma-separated)
  -A, --account ACCOUNTS            Filter accounts (comma-separated)
  -q, --qos QOS                     Filter quality of service
  -w, --nodelist HOSTS              Filter allocated nodes
  -S, --sort FIELDS                 Native squeue sort fields
  -r, --array                       Show individual array tasks
  -a, --all                         Include hidden partitions

Native output options:
  -o, --format FORMAT               Use a custom squeue format
  -O, --Format FIELDS               Use named squeue fields
  -l, --long                        Use the native long report
      --json, --yaml                Produce native structured output
      --helpformat, --helpstate     List native format fields or job states

Examples:
  sj q --me --start
  sj q --me -t RUNNING -i 5
  sj q -u ${USER} -p compute --width 120
  sj q --me --noheader
  sj q --me --json
  sj q --config ./sj.toml --view monitor --me -i5
  sj q --steps --list-fields
  sj q --filter 'Name glob \"train-*\" and State == \"RUNNING\"'
  sj q --filter 'not (Name =~ \"test|debug\")'
  sj q --steps --filter 'StepName glob \"worker-*\"'

Field expressions:
  ==, !=                            Compare complete strings
  =~, !~                            Search / negate a regular expression
  glob                              Match the whole field with *, ?, [abc], [!abc]
  not / !, and / &&, or / ||        Boolean operators, in this precedence order
  (...)                             Group conditions; matching short-circuits
  Field names are full native names, case-insensitive, including hidden fields.
  Values are case-sensitive; regex supports (?i). Use ^ and $ for whole regex matches.
  Single-quoted values are literal; double-quoted values use JSON escapes.
  Example: Name =~ '^train-[0-9]+$' or Partition glob 'debug-*'

Notes:
  In terminals, -i shows Every Ns and the last successful update's local time.
  Wide status rows include the date and UTC offset; narrow rows use a shorter time.
  --noheader hides table headers only. Redirected snapshots have no status row.
  Wide screens show TIME_LEFT and TIME; TIME_LEFT is hidden first as space shrinks.
  TIME_LEFT is unused runtime under the job limit, not an estimated completion time.
  Pending jobs show their unused time limit; --start keeps START_TIME as the main time.
  Time, USER, and JOBID can shrink to 5, 5, and 7 columns; shorter fields stay shorter.
  Time uses compact units, JOBID ends with an ellipsis, and USER truncates directly.
  Job names take priority over optional wide-screen columns; --json keeps full values.
  Wide screens include PARTITION and NODELIST(REASON); narrow screens compress reasons.
  Other squeue options pass through. Run squeue --help for the complete native list.
  Explicit native output formats bypass adaptive layout.
  Older Slurm without --helpFormat uses the compatibility fields shared by
  Slurm 22.05/23.11. --list-fields labels that fallback; version-specific fields
  remain accessible through native output.
  Filters use raw Slurm values before display cleanup, formatting, and truncation.
  Native squeue filters run first. View and repeated CLI filters combine with AND.
  --filter and --no-view-filter cannot be combined with native output or --list-fields.
  First adaptive use creates $XDG_CONFIG_HOME/sj/config.toml, or
  $HOME/.config/sj/config.toml when XDG_CONFIG_HOME is unset, empty, or relative.
  Configuration is read once; width rules are recalculated at each refresh.
  --view overrides default_view, start_view, and steps_view; --steps takes
  precedence over --start when choosing a default view.
  Native output cannot be combined with --view. Help and field listing create no file.
  -h always means help in sj; use --noheader to hide headers.
";

pub const CONFIG: &str = "sj config — Queue configuration

Usage:
  sj config path [OPTIONS]
  sj config views [OPTIONS]
  sj config check [OPTIONS]

Commands:
  path                              Print the resolved configuration path
  views                             List views (built-ins if no file exists)
  check                             Validate TOML and local Slurm field catalogs

Options:
  -h, --help                        Show this help
      --config FILE                 Use FILE instead of the default location

Examples:
  sj config path
  sj config views
  sj config check --config ./sj.toml
  sj q --config ./sj.toml --view monitor --me

Notes:
  These commands never create or overwrite a configuration file.
  check reads job/step catalogs (with a legacy fallback); it does not query jobs.
  default_view/start_view must work for jobs; steps_view must work for steps.
  Custom TOML patches built-ins: maps merge by property; arrays replace.
  Unknown schema keys, invalid widths/colors, and unknown fields are errors.
  View filter strings use the same expressions as --filter, including hidden fields.
  check validates every configured filter's syntax, patterns, and field names.
";

pub const OUTPUT: &str = "sj out — View sbatch output

Usage:
  sj out JOBID [OPTIONS]

Arguments:
  JOBID                             Job ID or exact array task, e.g. 12340_3

Options:
  -h, --help                        Show this help
      --err                         View stderr (stdout when the file is shared)
      --path                        Print the path without opening the file

Examples:
  sj out 12345
  sj out 12345 --err
  sj out 12345 --path
  sj out 12340_3

Notes:
  Resolve the path from Slurm metadata, including recorded completed jobs.
  Open less -R +G at the end: g = beginning, G = end, F = follow, q = quit.
  Ctrl-C stops following. --path works even before the log file is created.
";

pub const SCRIPT: &str = "sj script — View the submitted sbatch script

Usage:
  sj script JOBID [OPTIONS]

Arguments:
  JOBID                             Job ID or exact array task/component

Options:
  -h, --help                        Show this help
      --raw                         Write the original script to stdout

Examples:
  sj script 12345
  sj script 12345 --raw
  sj script 12345 > job.sbatch
  sj script 12340_3

Notes:
  Show Slurm's submitted copy, including scripts submitted with --wrap.
  Interactive terminals open less -N +g at the beginning, with line numbers.
  Redirected output and --raw contain only the original script bytes.
  Retrieve from the controller, then try accounting for historical jobs.
  Historical scripts require AccountingStoreFlags=job_script and retained records.
  Missing scripts or insufficient permissions produce an error.
  Array parents with multiple records require an exact task or component ID.
";

pub const SHELL: &str = "sj sh — Open a job node or running container shell

Usage:
  sj sh JOBID [NODE_INDEX] [OPTIONS]

Arguments:
  JOBID                             A RUNNING job owned by the current user
  NODE_INDEX                        Zero-based allocation index; optional

Options:
  -h, --help                        Show this help
      --nodes                       List node indices and exit
      --list                        List running containers on the selected node
      --host                        Open the host shell directly
      --container NAME_OR_PID       Enter a running container directly

Examples:
  sj sh 12345                        Choose a node, then a shell
  sj sh 12345 0                      Choose a shell on node 0
  sj sh 12345 --nodes                 Show node indices
  sj sh 12345 0 --list                Show running containers on node 0
  sj sh 12345 0 --host                Enter the host shell
  sj sh 12345 0 --container container-demo
  sj sh 12345 0 --container 23456

Notes:
  Without NODE_INDEX, a single node is automatic; multiple nodes show a menu.
  Menus fit terminal height: Up/Down, PgUp/PgDn, Home/End, or digits then Enter.
  Enter confirms the highlight, initially the first container; host shell is last.
  q/Esc/Ctrl-C/Ctrl-D cancels. Resizing adjusts the visible list.
  --container numbers are PIDs, not menu indices; use a PID for duplicate names.
  --nodes, --list, --host and --container are mutually exclusive.
  Shells require a terminal; --nodes and --list also work in scripts.
";

pub fn show_if_requested(args: &[String], text: &str) -> Result<bool> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        util::print(text)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

pub fn run(args: &[String]) -> Result<i32> {
    let text = match args {
        [] => ROOT,
        [name] => match name.as_str() {
            "-h" | "--help" => ROOT,
            "q" => QUEUE,
            "out" => OUTPUT,
            "script" => SCRIPT,
            "sh" => SHELL,
            "config" => CONFIG,
            other => return Err(format!("unknown help command {other:?}; use sj -h")),
        },
        _ => return Err("usage: sj help [q | out | script | sh | config]".into()),
    };
    util::print(text)?;
    Ok(0)
}
