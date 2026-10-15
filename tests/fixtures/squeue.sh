# A format-aware mock shared by CLI tests; all cluster values are fictional.
if { [ "$#" -eq 1 ] && [ "$1" = '--helpFormat' ]; } || { [ "$#" -eq 2 ] && [ "$1" = '--steps' ] && [ "$2" = '--helpFormat' ]; }; then
    case " $* " in
      *' --steps '*) printf '%s\n' 'JobId StepId StepName StepState UserName TimeUsed StartTime Partition Nodes NumCPUs NumTasks TimeLimit WorkDir StdOut';;
      *) printf '%s\n' 'JobId Name UserName State StateCompact TimeUsed NumNodes Reason StartTime Partition ReasonList TimeLeft Account QOS WorkDir StdOut TimeLimit NumCPUs NumTasks PendingTime';;
    esac
    exit 0
fi
sj_fake() {
__BODY__
}
sj_format=''
sj_previous=''
for sj_arg in "$@"; do
  if [ "$sj_previous" = '--Format' ]; then sj_format=$sj_arg; fi
  sj_previous=$sj_arg
done
sj_record=$(printf '\036')
case "$sj_format" in
  *"$sj_record"*)
    sj_rows=$(sj_fake "$@") || exit $?
    [ -n "$sj_rows" ] || exit 0
    printf '%s\n' "$sj_rows" | awk -v fmt="$sj_format" '
      BEGIN { FS="\037"; count=split(fmt, fields, ","); for(i=1;i<=count;i++) sub(/:0.*/, "", fields[i]); }
      {
        values["JobId"]=$1; values["Name"]=$2; values["UserName"]=$3;
        values["StateCompact"]=$4; values["State"]=($4=="PD" ? "PENDING" : "RUNNING");
        values["TimeUsed"]=$5; values["NumNodes"]=$6; values["Reason"]=$7;
        values["StartTime"]=$8; values["Partition"]=$9; values["ReasonList"]=$10;
        values["TimeLeft"]=$11; values["StepId"]=$1 ".0"; values["StepName"]=$2;
        values["StepState"]="RUNNING"; values["Nodes"]=$10; values["Account"]="research";
        values["QOS"]="normal"; values["WorkDir"]="/work"; values["StdOut"]="/work/job.out";
        values["TimeLimit"]="1:00:00"; values["NumCPUs"]="8"; values["NumTasks"]="2";
        values["PendingTime"]="123456";
        for(key in values) canonical[tolower(key)]=values[key];
        for(i=1;i<=count;i++) printf "%s%s", canonical[tolower(fields[i])], (i==count ? "\036\n" : "\037");
      }'
    ;;
  *) sj_fake "$@";;
esac
