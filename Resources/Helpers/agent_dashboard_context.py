#!/usr/bin/env python3
"""Read Dashboard objective metadata and local Git checkout identity for Neovim."""

import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
from urllib.request import HTTPRedirectHandler, Request, build_opener

UUID = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
WORK_FIELDS = ("id", "key", "title", "state", "objective", "acceptance", "evidence",
               "note", "worktree", "branch", "updated_at", "completion", "review_url")


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def dashboard(root):
    try:
        token = (Path(root) / ".dashboard" / "api-token").read_text().strip()
        req = Request("http://127.0.0.1:8741/api/state",
                      headers={"Authorization": "Bearer " + token})
        with build_opener(NoRedirect).open(req, timeout=3) as response:
            data = response.read(4_000_001)
        if len(data) > 4_000_000:
            raise ValueError("Response exceeds limit")
        state = json.loads(data)
        if not isinstance(state.get("projects"), list):
            raise ValueError("Missing project list")
        return state, None
    except Exception:
        # Never return the token, raw exception, response body or request headers.
        return None, "Dashboard unavailable. Check the service and local API token."


def thread_id(work):
    value = work.get("codex_thread_id")
    if value is None:
        origin = work.get("origin", "")
        value = origin[len("codex-task:"):] if origin.startswith("codex-task:") else ""
    value = value.lower() if isinstance(value, str) else ""
    return value if UUID.fullmatch(value) else None


def checkout(path):
    checked_at = time.time()
    result = {"path": path, "checked_at": checked_at}
    if not path or not os.path.isabs(path) or not os.path.isdir(path):
        return {**result, "error": "Checkout missing or directory unavailable"}
    try:
        completed = subprocess.run(
            ["git", "--no-optional-locks", "-C", path, "rev-parse",
             "--show-toplevel", "--abbrev-ref", "HEAD"],
            capture_output=True, text=True, timeout=2, check=False)
        lines = completed.stdout.strip().splitlines()
        if completed.returncode != 0 or len(lines) != 2:
            return {**result, "error": "Git checkout unavailable"}
        return {**result, "root": lines[0], "branch": lines[1], "detached": lines[1] == "HEAD"}
    except (OSError, subprocess.TimeoutExpired, UnicodeError):
        return {**result, "error": "Git checkout unavailable"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dashboard-root", required=True)
    args = parser.parse_args()
    raw = sys.stdin.read(256_001)
    if len(raw) > 256_000:
        raise ValueError("Task list exceeds limit")
    tasks = json.loads(raw)
    if not isinstance(tasks, list) or len(tasks) > 200:
        raise ValueError("Invalid task list")
    tasks = [task for task in tasks if isinstance(task, dict) and isinstance(task.get("id"), str)]
    wanted = {task.get("thread_id") for task in tasks}
    state, error = dashboard(args.dashboard_root)
    objectives = {task["id"]: [] for task in tasks}
    by_thread = {}
    for task in tasks:
        by_thread.setdefault(task.get("thread_id"), []).append(task["id"])
    projects = []
    if state is not None:
        for project in state["projects"]:
            projects.append({key: project.get(key) for key in ("id", "name", "path")})
            for work in project.get("work", []):
                linked = thread_id(work)
                if not linked or linked not in wanted:
                    continue
                item = {key: work.get(key) for key in WORK_FIELDS}
                item.update(project=project.get("id"), project_name=project.get("name"),
                            project_path=project.get("path"))
                for task_key in by_thread.get(linked, []):
                    objectives[task_key].append(item)
    paths = set()
    for task in tasks:
        path = task.get("cwd")
        if isinstance(path, str) and path:
            paths.add(path)
    for items in objectives.values():
        for item in items:
            path = item.get("worktree")
            if isinstance(path, str) and path:
                paths.add(path)
    with ThreadPoolExecutor(max_workers=4) as pool:
        observed = dict(zip(sorted(paths), pool.map(checkout, sorted(paths))))
    output = {
        "checked_at": time.time(), "dashboard_ok": state is not None, "dashboard_error": error,
        "collector_at": (state or {}).get("collector", {}).get("last_seen"),
        "projects": projects,
        "tasks": {task["id"]: {"checkout": observed.get(task.get("cwd")),
                                "objectives": objectives[task["id"]]} for task in tasks},
        "checkouts": observed,
    }
    print(json.dumps(output, separators=(",", ":")))


if __name__ == "__main__":
    try:
        main()
    except Exception:
        print(json.dumps({"error": "Context refresh failed. Check Python and Git availability."}))
        sys.exit(1)
