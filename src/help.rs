use crate::util::{self, Result};

pub const ROOT: &str = "sj — Slurm job helpers

Usage:
  sj <COMMAND> [OPTIONS]
  sj [QUEUE_OPTIONS]            Run q by default

Commands:
  q                            Show the adaptive queue
  out JOBID                    Open a job's output log
  script JOBID                 View the submitted sbatch script
  sh JOBID [NODE_INDEX]         Open a node or container shell
  config <path|views|check>     Inspect queue configuration
  help [COMMAND]               Show help for a command

Options:
  -h, --help                   Show help
  -V, --version                Show version

Examples:
  sj --me -i 5
  sj out 12345
  sj sh 12345 0

More: sj <COMMAND> -h
Docs: https://github.com/flymin/sj#commands
";

pub const QUEUE: &str = "sj q — Adaptive Slurm queue

Usage:
  sj q [OPTIONS]               Also: sj [QUEUE_OPTIONS]

Options:
  -h, --help                   Show help

  Query and refresh:
      --me                     Show your jobs
      --start                  Show pending jobs and expected start times
  -s, --steps[=IDS]             Show job steps
  -i, --iterate SECONDS         Refresh; show interval and last update time

  View and display:
      --config FILE            Read TOML configuration from FILE
      --view NAME              Select a named view
      --color auto|always|never Set colors (default: auto)
      --width COLUMNS          Override terminal width
      --noheader               Hide table headers
      --list-fields            List available native fields

  Filter:
      --filter EXPR            Match raw fields; repeat for AND
      --no-view-filter         Skip the view's default job filter

Native squeue options:
  -u, --user USERS; -j, --jobs JOBIDS; -t, --states STATES; -p, --partition PARTITIONS
  -o / -O / --long / --json / --yaml use native output and bypass views.
  All other native options pass through. Full list: squeue --help

Examples:
  sj q --me --start -i 5
  sj q --filter 'Name glob \"train-*\" and State == \"RUNNING\"'
  sj q --config ./sj.toml --view monitor

-h means help; use --noheader for headers. Native output rejects --view and filters.
Docs: https://github.com/flymin/sj#commands
Filters: https://github.com/flymin/sj#filter-any-field
";

pub const CONFIG: &str = "sj config — Queue configuration

Usage:
  sj config <COMMAND> [OPTIONS]

Commands:
  path                         Print the configuration path
  views                        List available named views
  check                        Validate configuration and local Slurm fields

Options:
      --config FILE            Use FILE instead of the default location
  -h, --help                   Show help

Examples:
  sj config path
  sj config check --config ./sj.toml

More: sj config <COMMAND> -h
Docs: https://github.com/flymin/sj#make-the-queue-yours
Column entry filters: https://github.com/flymin/sj#filter-entries-within-a-column
";

pub const CONFIG_PATH: &str = "sj config path — Locate configuration

Usage:
  sj config path [OPTIONS]

Options:
      --config FILE            Print FILE instead of the default path
  -h, --help                   Show help

Examples:
  sj config path
  sj config path --config ./sj.toml

Default: $XDG_CONFIG_HOME/sj/config.toml, or $HOME/.config/sj/config.toml.
This command does not create a file.
Docs: https://github.com/flymin/sj#make-the-queue-yours
";

pub const CONFIG_VIEWS: &str = "sj config views — List named views

Usage:
  sj config views [OPTIONS]

Options:
      --config FILE            Read views from FILE
  -h, --help                   Show help

Examples:
  sj config views
  sj config views --config ./sj.toml

Lists built-ins if no default file exists. Creates no file; does not query jobs.
Docs: https://github.com/flymin/sj#make-the-queue-yours
";

pub const CONFIG_CHECK: &str = "sj config check — Validate configuration

Usage:
  sj config check [OPTIONS]

Options:
      --config FILE            Validate FILE instead of the default configuration
  -h, --help                   Show help

Examples:
  sj config check
  sj config check --config ./sj.toml

Checks views, widths, colors, patterns and native fields without querying jobs.
Column entry filters: https://github.com/flymin/sj#filter-entries-within-a-column
Docs: https://github.com/flymin/sj#make-the-queue-yours
";

pub const OUTPUT: &str = "sj out — View sbatch output

Usage:
  sj out JOBID [OPTIONS]        JOBID can be an exact array task, e.g. 12340_3

Options:
      --err                    View stderr
      --path                   Print the path without opening less
  -h, --help                   Show help

Examples:
  sj out 12345
  sj out 12345 --err
  sj out 12345 --path

Opens less at the end: g = beginning, G = end, F = follow, q = quit.
Docs: https://github.com/flymin/sj#logs--out
";

pub const SCRIPT: &str = "sj script — View the submitted sbatch script

Usage:
  sj script JOBID [OPTIONS]     Use an exact task/component ID when ambiguous

Options:
      --raw                    Write the original script to stdout
  -h, --help                   Show help

Examples:
  sj script 12345
  sj script 12345 --raw
  sj script 12345 > job.sbatch

Opens less at the beginning. Redirected output preserves the original bytes.
Docs: https://github.com/flymin/sj#submitted-scripts--script
";

pub const SHELL: &str = "sj sh — Open a node or container shell

Usage:
  sj sh JOBID [NODE_INDEX] [OPTIONS]
  JOBID must be your RUNNING job; NODE_INDEX is zero-based (otherwise choose a node).

Options:
      --nodes                  List node indices and exit
      --list                   List running containers on the selected node
      --host                   Open the host shell directly
      --container NAME_OR_PID  Enter a container; numeric values are PIDs
  -h, --help                   Show help

Examples:
  sj sh 12345
  sj sh 12345 0 --host
  sj sh 12345 0 --container container-demo

Choose with arrows or digits, then Enter; q/Esc cancels. Containers precede host.
--nodes, --list, --host and --container are mutually exclusive.
Docs: https://github.com/flymin/sj#shells--sh
";

pub fn config_topic(action: Option<&str>) -> Result<&'static str> {
    match action {
        None => Ok(CONFIG),
        Some("path") => Ok(CONFIG_PATH),
        Some("views") => Ok(CONFIG_VIEWS),
        Some("check") => Ok(CONFIG_CHECK),
        Some(other) => Err(format!(
            "unknown config help command {other:?}; use sj config -h"
        )),
    }
}

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
        [name, action] if name == "config" => config_topic(Some(action))?,
        _ => {
            return Err(
                "usage: sj help [q | out | script | sh | config [path | views | check]]".into(),
            );
        }
    };
    util::print(text)?;
    Ok(0)
}
