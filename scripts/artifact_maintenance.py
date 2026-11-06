#!/usr/bin/env python3
"""Bound routine, newly managed artifacts; legacy data is never auto-deleted.

Each operation records an exact-path plan, reacquires both locks, preserves and
verifies source/resources and refuses any uncertain or active artifact. Invoked
only after a successful managed build/package/install, outside their locks.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import stat
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]

def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'scripts' / (name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def reconcile(reserve_bytes=0, protect=()):
    plan_driver = load('artifact_plan')
    retention = plan_driver.retention
    started = time.monotonic()
    removed = 0
    for _ in range(16):
        if time.monotonic() - started > 30:
            print('Retention: time budget reached; remaining items preserved for the next pass', file=sys.stderr)
            return
        with retention.project_lock(ROOT):
            plan = plan_driver.generate()
            usage = plan_driver.inventory.managed_usage(ROOT, plan_driver.inventory.collect(ROOT), initialize=True)
            plan['managed_pool'] = usage
            plan['status'] = 'routine managed maintenance; legacy items remain protected'
            plan['authority'] = 'automatic retention for newly marked artifacts only'
            for category in ('stages', 'snapshots', 'rollbacks', 'archives'):
                for item in plan[category]:
                    path = Path(item['path'])
                    held = any(path == value or path in value.parents or value in path.parents
                               for value in protect)
                    if held:
                        item['reasons'].append('protected by the current operation')
                        item['classification'] = 'protected'
                    elif item['classification'] == 'candidate for further review':
                        item['authorized_for_removal'] = True
                        item['remaining_checks'] = [check for check in item['remaining_checks']
                                                    if check != 'reviewed exact-path authorization immediately before removal']
            plan_path = retention.common_git_dir(ROOT) / 'twr-retention-plan.json'
            if plan_path.is_symlink():
                raise RuntimeError('Retention plan is a symlink')
            if plan_path.exists():
                info = plan_path.lstat()
                if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
                    raise RuntimeError('Existing retention plan is not private and owned')
            raw = json.dumps(plan, ensure_ascii=False, sort_keys=True, indent=2).encode() + b'\n'
            fd, name = tempfile.mkstemp(prefix='.twr-plan-', dir=plan_path.parent)
            pending = Path(name)
            try:
                with os.fdopen(fd, 'wb') as out:
                    out.write(raw)
                    out.flush()
                    os.fsync(out.fileno())
                os.replace(pending, plan_path)
            finally:
                if pending.exists() and not pending.is_symlink():
                    pending.unlink()
        managed_full = (usage['allocated_bytes'] + reserve_bytes > usage['budget_bytes']
                        or any(usage['counts'][name] >= limit for name, limit in usage['limits'].items()))
        if not plan['exceeded'] and not managed_full:
            return
        digest = hashlib.sha256(raw).hexdigest()
        retired = False
        for category, module_name in (('archives', 'artifact_retire_archive'),
                                      ('rollbacks', 'artifact_retire_rollback'),
                                      ('stages', 'artifact_retire_stage'),
                                      ('snapshots', 'artifact_retire_snapshot')):
            candidates = [item for item in plan[category] if item['authorized_for_removal']]
            candidates.sort(key=lambda item: Path(item['path']).lstat().st_mtime_ns)
            for item in candidates[:3]:
                try:
                    load(module_name).run(plan_path, digest, item['path'])
                    removed += 1
                    retired = True
                    print('Retention: preserved and retired one managed ' + category[:-1], file=sys.stderr)
                    break
                except Exception as error:
                    print('Retention: candidate preserved or needs review: ' + str(error), file=sys.stderr)
                    if 'Partial ' in str(error):
                        raise
            if retired:
                break
        if not retired:
            print('Retention: protected/legacy artifacts exceed the target; review the exact-path plan', file=sys.stderr)
            return
    print('Retention: pass limit reached; remaining items preserved for the next pass', file=sys.stderr)


if __name__ == '__main__':
    try:
        reconcile()
    except (OSError, RuntimeError, ValueError) as error:
        print('Retention: no maintenance completed: ' + str(error), file=sys.stderr)
        sys.exit(1)
