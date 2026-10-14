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
  help [COMMAND]                    Show general or command-specific help

Options:
  -h, --help                        Show this help
  -V, --version                     Show the sj version

Examples:
  sj --me                           Queue shortcut
  sj q --me --start                 Pending jobs and expected start times
  sj out 12345                       View a job's output log
  sj script 12345                   View the submitted script
  sj sh 12345                        Choose a job node and shell
  sj q -h                           Queue options and examples
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
  -i, --iterate SECONDS             Refresh and recalculate terminal width
      --width COLUMNS               Override automatic terminal width
      --noheader                    Hide column headers

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

Notes:
  Wide screens show TIME_LEFT and TIME; TIME_LEFT is hidden first as space shrinks.
  TIME_LEFT is unused runtime under the job limit, not an estimated completion time.
  Pending jobs show their unused time limit; --start keeps START_TIME as the main time.
  Time, USER, and JOBID can shrink to 5, 5, and 7 columns; shorter fields stay shorter.
  Time uses compact units, JOBID ends with an ellipsis, and USER truncates directly.
  Job names take priority over optional wide-screen columns; --json keeps full values.
  Wide screens include PARTITION and NODELIST(REASON); narrow screens compress reasons.
  Other squeue options pass through. Run squeue --help for the complete native list.
  Explicit native output formats bypass adaptive layout.
  -h always means help in sj; use --noheader to hide headers.
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
            other => return Err(format!("unknown help command {other:?}; use sj -h")),
        },
        _ => return Err("usage: sj help [q | out | script | sh]".into()),
    };
    util::print(text)?;
    Ok(0)
}
