#!/usr/bin/env python3
"""
EdgeStash Codebase Exporter for AI Evaluation
=============================================
This script aggregates all major EdgeStash source code, manifests, and architectural
context into a single comprehensive, beautifully structured Markdown document.
The resulting file is intended for external AI evaluation, architecture audits,
and deep code review.

Usage:
    python scripts/generate_codebase_markdown.py [--output docs/CODEBASE_EVALUATION.md]
"""

import argparse
import datetime
import os
import sys
from pathlib import Path

# Ordered list of crates and source files for evaluation
WORKSPACE_FILES = [
    {
        "category": "Workspace Manifest",
        "path": "Cargo.toml",
        "role": "Root workspace configuration, compiler optimizations, and dependencies.",
    },
]

CRATE_ORDER = [
    {
        "name": "edgestash-ipc",
        "role": "Core domain types, geometry math, configuration schemas, and Windows named pipe IPC protocol.",
        "files": [
            ("crates/edgestash-ipc/Cargo.toml", "Crate manifest and dependencies."),
            ("crates/edgestash-ipc/src/lib.rs", "IPC module exports and re-exports."),
            ("crates/edgestash-ipc/src/types.rs", "Rect, ScreenEdge, DockMode, SlotId, and geometric primitives."),
            ("crates/edgestash-ipc/src/config.rs", "EdgeStashConfig TOML schema, defaults, and boundary clamping."),
            ("crates/edgestash-ipc/src/protocol.rs", "IpcRequest, IpcResponse, StatusSnapshot, and IPC error codes."),
            ("crates/edgestash-ipc/src/pipe.rs", "Named pipe server and client implementation with length-prefixed JSON framing."),
        ],
    },
    {
        "name": "edgestash-daemon",
        "role": "Background engine managing low-level Win32 hooks, gesture recognition, state machines, DWM window placement, and DirectComposition overlays.",
        "files": [
            ("crates/edgestash-daemon/Cargo.toml", "Crate manifest and Win32 / Direct2D dependencies."),
            ("crates/edgestash-daemon/build.rs", "Build script linking Windows application manifests."),
            ("crates/edgestash-daemon/src/main.rs", "Daemon CLI entry point and argument parsing."),
            ("crates/edgestash-daemon/src/lib.rs", "Subsystem module declarations."),
            ("crates/edgestash-daemon/src/daemon.rs", "Main Win32 message pump, named pipe dispatcher, timer loops, and tray management."),
            ("crates/edgestash-daemon/src/state.rs", "Core EngineState managing multi-monitor decks, slot stacks, and transitions."),
            ("crates/edgestash-daemon/src/trigger.rs", "Gesture state machines: Corner Churn, Edge Bump velocity, and Edge Hover."),
            ("crates/edgestash-daemon/src/animator.rs", "DWM cubic-easing slide animations and coordinate interpolation."),
            ("crates/edgestash-daemon/src/focus.rs", "Delayed focus restoration, popup tracking, and active window protection."),
            ("crates/edgestash-daemon/src/geometry.rs", "Per-monitor DPI coordinate transforms and boundary padding calculations."),
            ("crates/edgestash-daemon/src/dwm.rs", "Win32 DWM cloak, window framing, and border inset compensations."),
            ("crates/edgestash-daemon/src/appbar.rs", "Native Windows AppBar work-area registration for docked presentation."),
            ("crates/edgestash-daemon/src/vdesktop.rs", "COM VirtualDesktopManager integration for virtual desktop scoping."),
            ("crates/edgestash-daemon/src/crash_recovery.rs", "JSON manifest persistence for orphaned window restoration across restarts."),
            ("crates/edgestash-daemon/src/platform/mod.rs", "Abstract platform interface definitions."),
            ("crates/edgestash-daemon/src/platform/win32.rs", "Win32 implementation: hooks, SysDragImage z-order fix, and message loop helpers."),
            ("crates/edgestash-daemon/src/renderer/mod.rs", "Renderer module exports."),
            ("crates/edgestash-daemon/src/renderer/d2d.rs", "DirectComposition / Direct2D overlay rendering: edge pills, drop zones, headers, and scrims."),
        ],
    },
    {
        "name": "edgestash-cli",
        "role": "Fast command-line interface for terminal scripting, status inspection, and third-party hotkey integration.",
        "files": [
            ("crates/edgestash-cli/Cargo.toml", "Crate manifest."),
            ("crates/edgestash-cli/src/main.rs", "CLI binary entry point."),
            ("crates/edgestash-cli/src/lib.rs", "Library exports."),
            ("crates/edgestash-cli/src/cli.rs", "Subcommand parsing (stash, toggle, cycle, dock-mode, status) and formatting."),
        ],
    },
    {
        "name": "edgestash-ui",
        "role": "Modern Fluent-styled GUI settings manager and system tray host built with egui and DirectComposition.",
        "files": [
            ("crates/edgestash-ui/Cargo.toml", "Crate manifest and eframe/egui dependencies."),
            ("crates/edgestash-ui/src/main.rs", "GUI binary entry point."),
            ("crates/edgestash-ui/src/lib.rs", "Library exports."),
            ("crates/edgestash-ui/src/app.rs", "egui UI layouts: gestures tab, dock tab, hotkeys tab, live slots deck."),
            ("crates/edgestash-ui/src/model.rs", "Settings state, draft modifications, dirty-checking, and preset saving."),
            ("crates/edgestash-ui/src/hotkey.rs", "Interactive hotkey capture widget and Win32 key code translation."),
            ("crates/edgestash-ui/src/ipc_bridge.rs", "Named pipe client bridge communicating with the background daemon."),
            ("crates/edgestash-ui/src/theme.rs", "Fluent dark theme palettes, visual tokens, and rounded card styling."),
        ],
    },
]

ARCHITECTURAL_PREAMBLE = """# EdgeStash: Complete Codebase & Architectural Context for AI Evaluation

> **Export Date**: {export_date}
> **Target OS**: Windows 11 / Windows 10 (Build 19041+)
> **Language & Toolchain**: Rust 2024 Edition (MSVC `x86_64-pc-windows-msvc`)
> **Total Files Exported**: {total_files} | **Total Source Lines**: {total_lines:,}

---

## 1. Project Overview & Core Mission

**EdgeStash** is an ultra-low-latency, gesture-driven window stashing and perimeter edge-drawer utility engineered for Windows 10 and 11. Instead of minimizing windows into the taskbar or overcrowding virtual desktops, EdgeStash allows power users to stash active applications (File Explorers, terminal windows, audio instruments, reference manuals) into the screen perimeter edges.

Users summon and peek into stashed drawers using natural physical interactions:
- **Edge Bump**: Pushing the mouse against the physical screen edge past a velocity threshold.
- **Corner Churn**: Swirling the mouse cursor in a screen corner (e.g. 270° angular accumulation).
- **Edge Hover**: Resting the cursor along a 1px non-blocking edge trigger strip.
- **Drop Zones**: Dragging files or windows directly to visual screen edge zones.

---

## 2. Non-Negotiable Invariants & Performance Constraints

EdgeStash is designed for live audio production (DAWs such as Ableton Live, Bitwig, FL Studio) and real-time visual performance (TouchDesigner, Resolume). As such, it enforces strict architectural invariants:

1. **0.0% Idle CPU Overhead**:
   - The daemon must never run continuous `while true` loops or periodic polling timers.
   - All state transitions are strictly event-driven: Win32 window message queues, low-level mouse hooks (`WH_MOUSE_LL`), and asynchronous named pipe IPC.

2. **Zero Audio DPC Latency Spikes**:
   - The low-level mouse hook executes in sub-microsecond time.
   - Zero heap allocations (`Box`, `Vec`, `String`) and zero thread synchronization locks inside the mouse callback.
   - No aggressive process working-set trimming (`SetProcessWorkingSetSize(-1, -1)`) that causes hard page faults.

3. **Principle of Least Privilege (`asInvoker`)**:
   - Runs strictly with standard user privileges (`asInvoker` execution level in the application manifest).
   - Never triggers UAC prompts and respects Windows User Interface Privilege Isolation (UIPI).

4. **Explorer Restart Resilience**:
   - Automatically reinstates the system tray icon via `ChangeWindowMessageFilterEx` when `explorer.exe` restarts.

5. **Topmost Band Management & Drag Preview (`SysDragImage`)**:
   - Windows Shell drag-and-drop previews (`SysDragImage`) are dynamically promoted above `HWND_TOPMOST` drawers so users maintain visual feedback while dragging items into stashed folders.

---

## 3. High-Level System Architecture

```mermaid
flowchart TD
    subgraph UserSpace["Windows Desktop User Session"]
        Cursor["Mouse & Gesture Input"]
        WinDWM["Windows DWM & Explorer"]
    end

    subgraph CoreEngine["EdgeStash Engine"]
        Daemon["edgestash-daemon.exe\n(Win32 Low-Level Hooks & State Machine)"]
        IPC["crates/edgestash-ipc\n(Named Pipe Protocol & Geometric Math)"]
        UI["edgestash-ui.exe\n(DirectComposition / Direct2D Settings Host)"]
        CLI["edgestash.exe\n(Command Line Interface)"]
    end

    Cursor -->|WH_MOUSE_LL| Daemon
    WinDWM <-->|SetWindowPos / AppBars| Daemon
    Daemon <--->|\\\\.\\pipe\\edgestash-ipc| IPC
    CLI <--->|Named Pipe Requests| IPC
    UI <--->|Named Pipe Settings Sync| IPC
```

---

## 4. Table of Contents & Manifest

{table_of_contents}

---

"""


def get_language(path: str) -> str:
    if path.endswith(".rs"):
        return "rust"
    if path.endswith(".toml"):
        return "toml"
    if path.endswith(".py"):
        return "python"
    if path.endswith(".json"):
        return "json"
    return "text"


def collect_file_info(root_dir: Path):
    entries = []

    # Add workspace files
    for item in WORKSPACE_FILES:
        p = root_dir / item["path"]
        if p.is_file():
            content = p.read_text(encoding="utf-8", errors="replace")
            lines = content.splitlines()
            entries.append({
                "category": item["category"],
                "rel_path": item["path"],
                "role": item["role"],
                "line_count": len(lines),
                "size_kb": len(content.encode("utf-8")) / 1024.0,
                "content": content,
            })

    # Add crate files
    for crate in CRATE_ORDER:
        for rel_path, role in crate["files"]:
            p = root_dir / rel_path
            if p.is_file():
                content = p.read_text(encoding="utf-8", errors="replace")
                lines = content.splitlines()
                entries.append({
                    "category": f"Crate: `{crate['name']}`",
                    "rel_path": rel_path,
                    "role": role,
                    "line_count": len(lines),
                    "size_kb": len(content.encode("utf-8")) / 1024.0,
                    "content": content,
                })
            else:
                print(f"[WARN] File not found: {rel_path}", file=sys.stderr)

    return entries


def build_markdown(root_dir: Path, output_file: Path):
    entries = collect_file_info(root_dir)
    total_lines = sum(e["line_count"] for e in entries)
    total_files = len(entries)

    # Build Table of Contents
    toc_lines = []
    current_category = None
    for idx, e in enumerate(entries, 1):
        if e["category"] != current_category:
            current_category = e["category"]
            toc_lines.append(f"\n### {current_category}\n")
            toc_lines.append("| # | File | Lines | Size | Role |")
            toc_lines.append("| :--- | :--- | :--- | :--- | :--- |")

        anchor = e["rel_path"].replace("/", "").replace(".", "").replace("-", "").replace("_", "").lower()
        toc_lines.append(
            f"| {idx} | [`{e['rel_path']}`](#{anchor}) | {e['line_count']:,} | {e['size_kb']:.1f} KB | {e['role']} |"
        )

    toc_str = "\n".join(toc_lines)

    export_date = datetime.datetime.now().strftime("%Y-%m-%d %H:%M:%S UTC")
    preamble = ARCHITECTURAL_PREAMBLE.format(
        export_date=export_date,
        total_files=total_files,
        total_lines=total_lines,
        table_of_contents=toc_str,
    )

    # Build Source Code Blocks
    code_blocks = []
    code_blocks.append("## 5. Major Source Code Listings\n\n")

    for idx, e in enumerate(entries, 1):
        lang = get_language(e["rel_path"])
        anchor = e["rel_path"].replace("/", "").replace(".", "").replace("-", "").replace("_", "").lower()

        section = f"""<a id="{anchor}"></a>
### {idx}. `{e['rel_path']}`

> **Location**: [`{e['rel_path']}`]({e['rel_path']}) | **Category**: {e['category']}
> **Metrics**: {e['line_count']:,} lines | {e['size_kb']:.1f} KB
> **Purpose**: {e['role']}

```{lang}
{e['content']}
```

---
"""
        code_blocks.append(section)

    output_file.parent.mkdir(parents=True, exist_ok=True)
    full_markdown = preamble + "\n".join(code_blocks)
    output_file.write_text(full_markdown, encoding="utf-8")

    return total_files, total_lines, len(full_markdown.encode("utf-8"))


def main():
    parser = argparse.ArgumentParser(description="Export EdgeStash codebase to a single Markdown file for AI evaluation.")
    parser.add_argument(
        "--output",
        "-o",
        type=str,
        default="docs/CODEBASE_EVALUATION.md",
        help="Path to output markdown file (default: docs/CODEBASE_EVALUATION.md)",
    )
    args = parser.parse_args()

    root_dir = Path(__file__).resolve().parent.parent
    output_path = root_dir / args.output

    print(f"Generating codebase export from: {root_dir}")
    print(f"Target output file: {output_path}")

    files, lines, bytes_written = build_markdown(root_dir, output_path)

    print("\n" + "=" * 60)
    print("Export Complete!")
    print(f"  Files Exported : {files}")
    print(f"  Total Lines    : {lines:,}")
    print(f"  File Size      : {bytes_written / 1024:.1f} KB ({bytes_written / (1024 * 1024):.2f} MB)")
    print(f"  Estimated Tokens: ~{lines * 8:,} - {int(bytes_written / 4):,} tokens")
    print(f"  Output Path    : {output_path}")
    print("=" * 60 + "\n")


if __name__ == "__main__":
    main()
