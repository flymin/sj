<div align="center">

# sj

### Slurm，几次按键就到位。

查看队列。打开日志。检查脚本。进入作业。

<p>
  <img alt="版本 0.1.0" src="https://img.shields.io/badge/version-0.1.0-8b5cf6?style=flat-square">
  <img alt="使用 Rust 1.99 构建" src="https://img.shields.io/badge/Rust-1.99-f97316?style=flat-square&amp;logo=rust&amp;logoColor=white">
  <img alt="静态二进制" src="https://img.shields.io/badge/binary-static-14b8a6?style=flat-square">
  <img alt="Linux aarch64" src="https://img.shields.io/badge/Linux-aarch64-6366f1?style=flat-square&amp;logo=linux&amp;logoColor=white">
</p>

[English](README.md) · **简体中文**

[快速开始](#快速开始) · [安装](#安装) · [命令](#命令) · [快捷键](#快捷键) · [开发](#开发)

<img src="docs/assets/terminal-preview.svg" alt="sj 预览：包含剩余时间的队列、作业日志、提交脚本和容器 shell 菜单" width="1100">

<sub>示意会话。作业名、节点和日志内容均为示例。</sub>

</div>

`sj` 用一个小巧的 Rust 可执行文件，串起日常 Slurm 工作的四个步骤：查看运行状态、找到作业日志、检查提交脚本、在正确的节点或已有 Enroot 容器中打开 shell。

## 为什么选择 sj？

| 接下来要做什么 | 一条命令 | 得到什么 |
| --- | --- | --- |
| 查看队列 | `sj --me` | 适应终端宽度的列、剩余运行时间，以及优先留给作业名的空间。 |
| 阅读输出 | `sj out 12345` | 从 Slurm 元数据定位 sbatch 日志，并用 `less` 打开。 |
| 检查提交内容 | `sj script 12345` | 从头查看提交脚本，并显示行号。 |
| 检查运行中的作业 | `sj sh 12345` | 先选节点，再进入容器或宿主机 shell。 |

- **宽窄都清楚。** 宽屏显示 `TIME_LEFT`、`TIME`、`PARTITION` 和 `NODELIST(REASON)`，窄屏自动压缩布局。
- **熟悉的筛选方式。** 继续使用 `--me`、`--start`、`-u`、`-j`、`-t`、`-p` 等 `squeue` 选项。
- **菜单刚好装得下。** 节点和容器列表按终端高度滚动，随窗口缩放调整。
- **容器优先。** Enter 确认当前高亮的容器，宿主机 shell 始终排在最后。
- **一个可执行文件。** 交付的 Linux aarch64 二进制采用静态链接，无需激活 venv，也无需 Python 或 Rust 运行环境。

## 快速开始

```bash
sj --me                       # 本人的队列，自适应终端宽度
sj --me --start               # 等待中的作业及预计开始时间
sj out 12345                   # 从作业日志末尾开始查看
sj script 12345                # 从头查看提交时的脚本副本
sj sh 12345                    # 选择节点，再选择 shell
```

将 `12345` 替换为自己的作业 ID。首次使用请先查看[安装](#安装)。

## 安装

### 运行时要求

| 功能 | `PATH` 中需要有 |
| --- | --- |
| 队列 | `squeue` |
| 日志 | `sacct`、`scontrol`、`less` |
| 提交脚本 | `scontrol`；历史查询需要 `sacct`；交互查看需要 `less` |
| 节点与容器 shell | `scontrol`、`srun`；进入容器时目标节点还需要 `enroot` |

当前构建目标为 **aarch64 Linux**。使用 `sh` 时，可执行文件的路径必须在计算节点上也能访问，通常放在共享 home 目录中；登录节点与计算节点的架构需要兼容。

### 构建与安装

构建脚本使用**已有的 uv venv** 存放编译工具链：优先读取 `SJ_VENV`，其次为当前激活的 `VIRTUAL_ENV`，最后默认为 `$HOME/.venvs/dev`。示例假设源码位于 `~/local/src/sj`。

```bash
cd ~/local/src/sj

# 首次配置工具链；已经配置时可跳过。
SJ_VENV="$HOME/.venvs/dev" SJ_UV="$(command -v uv)" ./setup-toolchain.sh

# 检查、构建并安装静态二进制。
SJ_VENV="$HOME/.venvs/dev" SJ_INSTALL_DIR="$HOME/local/bin" ./build.sh

export PATH="$HOME/local/bin:$PATH"
sj -h
```

构建脚本检查格式、运行测试和 Clippy、编译 release、确认静态链接，再安装到 `~/local/bin/sj`。构建输出保存在 `.logs/build.log`，临时构建目录在退出时清理。构建和工具链配置日志中的本地用户名、文件系统路径会替换为环境变量占位符；二进制中的编译路径也会映射为匿名路径。文档和预览使用虚构标识。

## 命令

| 命令 | 用途 |
| --- | --- |
| `sj [q] [OPTIONS]` | 展示队列；`q` 是默认命令。 |
| `sj out JOBID [OPTIONS]` | 打开 stdout、stderr，或仅输出解析后的日志路径。 |
| `sj script JOBID [OPTIONS]` | 查看 Slurm 保存的提交脚本，或用 `--raw` 输出原文。 |
| `sj sh JOBID [NODE_INDEX] [OPTIONS]` | 选择节点，进入容器或宿主机 shell。 |
| `sj help [COMMAND]` | 查看总览或某个子命令的帮助。 |

所有层级都支持 **`-h` 和 `--help`**。`sj -h` 介绍各子命令；`sj q -h`、`sj out -h`、`sj script -h`、`sj sh -h` 分别展示本命令的用法、选项和示例。查看帮助不需要连接 Slurm。

### 队列 · `q`

```bash
sj q --me --start
sj q --me -t RUNNING -i 5
sj q -u "${USER}" -p compute --width 120
sj q --me --noheader
sj q --me --json
```

布局优先给作业名留出空间。宽屏同时显示 **`TIME_LEFT`** 和 **`TIME`**（已运行时间），以及 `PARTITION` 和原生 `NODELIST(REASON)`。终端变窄时，先隐藏 `TIME_LEFT`。剩余时间表示 Slurm 作业时限中尚未消耗的运行时间，不是工作负载预计何时完成；等待中作业显示尚未消耗的运行时限。

需要继续压缩时，时间、`USER`、`JOBID` 分别允许缩至 **5**、**5**、**7** 个显示字符；短内容不会被强行补足这些最小宽度。长时间改用 `1d02h`、`1h23m`、`12h` 等短格式，无限时限和未知值在需要时显示为 `∞`、`?`。作业 ID 结尾加 `…`，用户名直接截断。宽度恢复后显示原始值；原生格式或 `--json` 不受终端宽度影响，可获取完整值。过长节点列表使用省略号截断，窄屏等待原因使用 `Prio`、`Res`、`Dep` 等简写。

`--start` 按 Slurm 原生语义展示等待中的作业，并按预计开始时间排序，主要时间列保持为 `START_TIME`；宽屏额外显示 `TIME_LEFT`。`-i 5` 每五秒刷新，重新计算列宽并获取最新剩余时间。省略 `--me` 时，用户范围沿用 `squeue` 默认设置。

隐藏表头使用 **`--noheader`**；在 `sj` 中，`-h` 始终表示帮助。自定义格式（`-o`、`-O`）、`--long`、steps 和 JSON/YAML 使用原生 `squeue` 输出。

### 日志 · `out`

```bash
sj out 12345                       # 用 less 打开 stdout
sj out 12345 --err                 # stderr，或两路输出共用的日志
sj out 12345 --path                # 仅输出路径，不打开分页器
sj out 12340_3                     # 明确指定数组作业的任务
```

日志路径来自 Slurm 元数据，也支持 accounting 中保留的已完成作业记录。相对路径以作业工作目录解析。数组父 ID 对应多个日志时，需要指定明确的任务。

日志以 `less -R +G` 打开，从末尾开始查看。按 **F** 跟随新增输出，**Ctrl-C** 停止跟随，**q** 退出。只要元数据中已有路径，`--path` 就可以在文件创建前输出它；实际打开日志时，文件需要在当前主机上可读。

### 提交脚本 · `script`

```bash
sj script 12345                 # 用 less 查看 Slurm 保存的提交副本
sj script 12345 --raw           # 输出脚本原始字节
sj script 12345 > job.sbatch    # 保存提交时的副本
sj script 12340_3               # 明确指定数组任务
```

交互终端使用 `less -N +g` 从头查看，并显示行号。重定向和 `--raw` 只输出脚本原文，保留空白和末尾换行。查看的是 Slurm 保存的副本，也支持 `sbatch --wrap` 生成的脚本；提交后修改或删除本地原文件不会改变该副本。

先从控制器取回，失败后尝试 accounting 历史记录。历史脚本依赖集群已有的 `AccountingStoreFlags=job_script` 配置及尚未清理的记录。脚本缺失或权限不足时明确报错。数组父 ID 或异构作业匹配多个 allocation 时，需要指定明确的任务或组件 ID。查看过程不创建临时脚本文件。

### Shell · `sh`

```bash
sj sh 12345                            # 选择节点，再选择 shell
sj sh 12345 0                          # 在节点 0 上选择 shell
sj sh 12345 --nodes                    # 列出 allocation 中的节点编号
sj sh 12345 0 --list                   # 列出运行中的容器
sj sh 12345 0 --host                   # 直接进入宿主机
sj sh 12345 0 --container container-demo  # 按完整容器名进入
sj sh 12345 0 --container 23456        # 按 --list 给出的 PID 进入
```

节点编号从 **0** 开始，保留 Slurm 原始 allocation 顺序。单节点作业自动选择该节点；多节点作业省略编号时打开选择菜单。

容器排在前面，**host shell 始终作为最后一个可选项**。默认高亮第一个容器，直接 Enter 就会进入它；没有运行中容器时，host shell 是唯一选项。列表展示目标节点上当前用户可见的运行中 Enroot 容器，包含名称、代表 PID，以及能够识别的作业 ID。

`--container` 接受容器名或 **PID**，不是菜单编号。同名容器有多个运行实例时，用 PID 区分。`--nodes`、`--list`、`--host`、`--container` 互斥。

进入 shell 需要**本人拥有且处于 RUNNING 状态的作业**，并且需要交互终端。在脚本中使用 `--nodes`，或在多节点作业中明确给出节点编号后使用 `--list`。

## 快捷键

### 节点与容器菜单

| 按键 | 行为 |
| --- | --- |
| ↑ / ↓ | 移动高亮项。 |
| PageUp / PageDown | 按可见页翻动。 |
| Home / End | 跳到第一项或最后一项。 |
| 数字，再按 Enter | 选择从 0 开始的编号。 |
| Backspace | 修改输入的数字。 |
| Enter | 确认当前高亮项。 |
| q / Esc / Ctrl-C / Ctrl-D | 取消选择。 |

条目少时完整显示，条目多时在终端高度内滚动。窗口缩放时自动调整可见范围。菜单关闭时恢复终端画面、输入设置和光标。

### 日志查看器

| 按键 | 行为 |
| --- | --- |
| g / G | 跳到开头 / 末尾。 |
| F | 跟随新增输出。 |
| Ctrl-C | 停止跟随，恢复浏览。 |
| q | 关闭查看器。 |

## 开发

构建使用 **Rust 1.99.0**、edition 2024、`aarch64-unknown-linux-musl` 目标和自带的 `rust-lld`。依赖版本固定在 `Cargo.lock` 中。

```bash
./build.sh
```

测试覆盖队列布局、时间短格式、剩余时间刷新、Unicode、原生参数透传、日志定位、数组 ID、脚本取回与字节保真、节点顺序、容器发现和 CLI 帮助。伪终端测试验证脚本分页及提前退出、方向键、数字输入、窗口缩放、取消选择及终端恢复。构建脚本测试验证环境选择与日志匿名化。涉及 Slurm 的集成测试使用临时命令 mock。

<details>
<summary><strong>进阶行为与可移植性</strong></summary>

### 队列渲染

- 宽度依次取自终端、`COLUMNS` 和 120 列默认值。极窄时减少辅助列或使用多行。表格中的作业 ID 可以缩写；极窄的多行回退会将完整 ID 单独显示一行。
- 刷新间隔支持 `-i 5`、`-i5`、`--iterate=5`。TTY 中重绘，重定向时连续追加快照。
- 开始时间在窄屏显示月日和时分，宽屏显示完整时间戳。
- 多集群输出带原生集群标题时，使用明确的原生格式或 JSON/YAML。

### 日志定位

- 通过 `sacct --expand-pattern` 解析已记录的路径；accounting 不可用时回退到 `scontrol` JSON。
- 回退逻辑支持 `%j`、`%A`、`%a`、`%b`、`%x`、`%u`、`%%` 及数字补零。按节点或任务拆分的复杂模板依赖 Slurm 原生展开。
- 使用元数据中记录的路径，不递归搜索文件系统。日志需要在运行 `out` 的主机上挂载且可读。

### Shell 执行

- 连接使用单个共享资源的 `srun` step，设置 `--nodes=1`、`--ntasks=1`、`--cpus-per-task=1`、`--overlap`、`--exact`、`--pty`。设备权限沿用 allocation，step 的资源等待使用 `--immediate=10`。
- 菜单采用 JSON 中的原始 allocation 顺序。新启动的交互 step 有自己的 node rank；这个单节点 step 的 `SLURM_NODEID` 通常为 0。
- 通过 mount namespace 合并可访问进程，并检查 Enroot 的 `/.lock` 标记，不依赖 `ENROOT_DATA_PATH`。缓存的容器根文件系统不会被列为运行中。
- 进入前检查 PID、进程启动时间和 namespace。`enroot exec` 加入已有容器，恢复其环境和工作目录。
- 容器内优先使用 `/bin/bash -l`，否则使用 `/bin/sh -l`。宿主机使用 `$SHELL -l`，找不到时回退到 bash/sh。

### 工具链配置

- 编译工具放在所选 venv 的 `opt/rust-1.99.0` 下，在其 `bin` 中提供 Rust 命令入口。Cargo 缓存位于 `share/cargo`。
- 配置脚本在 venv 内安装 patchelf，并解包隔离的 musl/libgcc 运行库；不编辑 `pyvenv.cfg` 或激活脚本，也不安装系统包。
- Rust 下载按发布 manifest 中的 SHA256 校验；运行库版本与 SHA256 固定在配置脚本中。
- `SJ_VENV`、`SJ_UV`、`SJ_INSTALL_DIR` 分别选择已有环境、uv 可执行文件和安装目录。`SJ_UV` 默认使用 `PATH` 中的 `uv`，其次为 `$HOME/.local/bin/uv`；安装目录默认为 `$HOME/local/bin`。工具链已经可用时跳过配置。
- 交付的可执行文件不依赖共享库，但仍按功能调用集群的 Slurm 工具、`less` 和 `enroot`。

</details>

---

<div align="center">

README 的呈现方式参考了 [sqwatch](https://github.com/fedonman/sqwatch)。

[回到顶部](#sj) · [English](README.md)

</div>
