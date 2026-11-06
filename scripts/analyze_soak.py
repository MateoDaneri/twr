#!/usr/bin/env python3
"""Read aggregate synthetic tmux receipts without contacting a process or GUI.

No repair, cleanup, test launch or inference of continuous wall-clock coverage.
Exit 0: running or sampled transport passed. Exit 1: review/failure. Exit 2: invalid input.
"""
import argparse
import datetime
import json
import math
import os
from pathlib import Path
import stat

MAX_REPORT = 64 * 1024
MAX_SAMPLES = 32 * 1024 * 1024
MAX_LINES = 86400


def number(value, label, minimum=0):
    if isinstance(value, bool) or not isinstance(value, (int, float)) or value < minimum:
        raise ValueError('Invalid ' + label)
    try:
        if not math.isfinite(value):
            raise ValueError('Invalid ' + label)
    except OverflowError as error:
        raise ValueError('Number exceeds finite range: ' + label) from error
    return value


def integer(value, label, minimum=0):
    number(value, label, minimum)
    if not isinstance(value, int):
        raise ValueError('Invalid integer ' + label)
    return value


def read_owned(path, maximum, optional=False, dir_fd=None):
    # O_NOFOLLOW plus fstat avoids following a swapped receipt symlink.
    try:
        fd = os.open(path.name if dir_fd is not None else path,
                     os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=dir_fd)
    except FileNotFoundError:
        if optional:
            return b'', None
        raise
    with os.fdopen(fd, 'rb') as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_size > maximum:
            raise ValueError('Receipt must be an owned bounded regular file: ' + path.name)
        data = stream.read(maximum + 1)
        if len(data) > maximum:
            raise ValueError('Receipt grew beyond limit: ' + path.name)
        return data, info.st_mtime


def analyze(report, samples, *, partial_line=False, age_seconds=None):
    if not isinstance(report, dict) or type(report.get('schema')) is not int or report['schema'] != 1:
        raise ValueError('Unsupported report schema')
    status = report.get('status')
    if status not in ('starting', 'running', 'passed', 'stopped', 'failed'):
        raise ValueError('Invalid report status')
    live = status in ('starting', 'running')
    interval = integer(report.get('interval_seconds'), 'interval', 1)
    requested = integer(report.get('requested_seconds'), 'requested duration', 10)
    if interval > 60 or requested > 86400:
        raise ValueError('Duration or interval exceeds runner bounds')
    completed = number(report.get('completed_seconds'), 'completed seconds')
    iterations = integer(report.get('iterations'), 'iterations')
    reattachments = integer(report.get('reattachments'), 'reattachments')
    failures = report.get('failures')
    if not isinstance(failures, list) or len(failures) > 100 or not all(isinstance(f, str) for f in failures):
        raise ValueError('Invalid failure list')
    if len(samples) > MAX_LINES:
        raise ValueError('Too many samples')
    slack = max(5, interval * .25)
    limit = interval + slack
    previous = 0
    gaps = []
    max_gap = 0
    for index, sample in enumerate(samples, 1):
        if not isinstance(sample, dict) or integer(sample.get('iteration'), 'sample iteration', 1) != index:
            raise ValueError('Sample iteration sequence is not contiguous')
        elapsed = number(sample.get('elapsed_seconds'), 'sample elapsed seconds')
        if elapsed <= previous:
            raise ValueError('Sample clock must increase')
        width, height = (100, 30) if index % 2 else (82, 24)
        if (integer(sample.get('width'), 'width'), integer(sample.get('height'), 'height')) != (width, height):
            raise ValueError('Unexpected synthetic resize dimensions')
        integer(sample.get('client_bytes'), 'client bytes', 1)
        if sample['client_bytes'] > 2 * 1024 * 1024 or sample.get('pane_pid_preserved') is not True:
            raise ValueError('Invalid synthetic client or PID evidence')
        gap = elapsed - previous
        max_gap = max(max_gap, gap)
        if gap > limit:
            gaps.append({'before_iteration': index, 'seconds': round(gap, 2)})
        previous = elapsed
    warnings = []
    count_delta = iterations - len(samples)
    # The runner atomically saves report BEFORE appending its sample. Conversely,
    # our read of report can precede its next save and see one later sample.
    if count_delta or partial_line or previous > completed:
        if live and abs(count_delta) <= 1 and previous <= completed + limit:
            warnings.append('live_snapshot_incomplete; reread later without restarting runner')
        else:
            warnings.append('report_and_samples_inconsistent')
    if reattachments != iterations:
        warnings.append('reattachment_count_mismatch')
    if not live and partial_line:
        warnings.append('final_sample_line_incomplete')
    tail = max(0, completed - previous)
    if tail > limit:
        warnings.append('unobserved_tail_exceeds_sampling_budget')
    if gaps:
        warnings.append('monotonic_sampling_gaps')
    if age_seconds is not None and live and age_seconds > limit:
        warnings.append('receipt_stale; cause unknown, do not infer process exit or restart')
    if age_seconds is not None and age_seconds < -slack:
        warnings.append('receipt_mtime_in_future; wall clock may have changed')
    if not live and report.get('private_server_cleanup') is not True:
        warnings.append('private_server_cleanup_not_confirmed')
    if status == 'passed' and (completed < requested or iterations < 2):
        warnings.append('passed_receipt_does_not_cover_requested_duration_or_cycles')
    if status == 'failed' or failures:
        decision = 'failed_transport'
    elif status == 'stopped':
        decision = 'stopped_incomplete'
    elif warnings and warnings != ['live_snapshot_incomplete; reread later without restarting runner']:
        decision = 'review_evidence'
    else:
        decision = 'running' if live else 'passed_sampled_transport'
    return {'schema': 1, 'decision': decision, 'runner_status': status,
            'samples': len(samples), 'reported_iterations': iterations,
            'requested_seconds': requested, 'reported_monotonic_seconds': completed,
            'last_sample_monotonic_seconds': previous,
            'maximum_monotonic_sample_gap_seconds': round(max_gap, 2),
            'allowed_monotonic_gap_seconds': limit, 'tail_seconds': round(tail, 2),
            'sampling_gaps': gaps, 'warnings': warnings, 'reported_failures': failures,
            'continuous_wall_clock_coverage': 'unavailable; schema1 has no per-sample UTC timestamps',
            'scope': 'sampled private tmux transport; not GPUI, personal shell, providers or 24/7 certification'}


def inspect(directory):
    directory = Path(directory).absolute()
    if directory.parent != Path('/private/tmp') or not directory.name.startswith('tw-transport-soak-'):
        raise ValueError('Only a synthetic /private/tmp/tw-transport-soak-* directory is accepted')
    fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        info = os.fstat(fd)
        if info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise ValueError('Run directory must be owned, private and not a symlink')
        raw, mtime = read_owned(directory / 'report.json', MAX_REPORT, dir_fd=fd)
        report = json.loads(raw)
        raw_samples, _ = read_owned(directory / 'samples.jsonl', MAX_SAMPLES, optional=True, dir_fd=fd)
    finally:
        os.close(fd)
    partial = bool(raw_samples and not raw_samples.endswith(b'\n'))
    lines = raw_samples.splitlines()
    if partial:
        lines.pop()  # A live append is not evidence of a complete cycle yet.
    if len(lines) > MAX_LINES:
        raise ValueError('Too many sample lines')
    samples = [json.loads(line) for line in lines]
    now = datetime.datetime.now(datetime.timezone.utc)
    result = analyze(report, samples, partial_line=partial, age_seconds=now.timestamp() - mtime)
    result.update(directory=str(directory), analyzed_utc=now.isoformat())
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory')
    args = parser.parse_args()
    try:
        result = inspect(args.directory)
    except (ValueError, OSError, UnicodeError) as error:
        parser.exit(2, 'Invalid synthetic evidence: ' + str(error) + '\n')
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 0 if result['decision'] in ('running', 'passed_sampled_transport') else 1


if __name__ == '__main__':
    raise SystemExit(main())
