#!/usr/bin/env python3
"""Synthetic JSONL metadata server. No account, filesystem discovery or network."""
import json
import sys
import time

overview = "--overview" in sys.argv
refresh = 0
for line in sys.stdin:
    message = json.loads(line)
    method = message.get('method')
    if method == 'initialize':
        result = {'userAgent': 'synthetic-fixture'}
    elif method == 'initialized':
        continue
    elif method == 'thread/list':
        assert message['params']['useStateDbOnly'] is True
        child = bool(message['params'].get('cursor'))
        if not child:
            refresh += 1
        result = {'data': [{
            'id': '00000000-0000-0000-0000-00000000000' + ('2' if child else '1'),
            'name': 'Resultado preparado' if child else 'Tarea sintética en ejecución',
            'source': 'cli', 'status': {'type': 'idle' if child else 'active'},
            'parentThreadId': '00000000-0000-0000-0000-000000000001' if child else None,
            'updatedAt': 1 if child else int(time.time()), 'projectId': 'synthetic-project',
            'cwd': '/synthetic/project', 'gitInfo': {'branch': 'codex/synthetic'},
            'preview': 'This field must never be kept by the metadata client.',
        }], 'nextCursor': None if child else 'second-page'}
        if overview and not child:
            result['data'] += [{
                'id': f'00000000-0000-0000-0001-{i:012d}',
                'name': f'Tarea de scroll {i:03d}', 'source': 'cli',
                'status': {'type': 'idle'}, 'updatedAt': 1,
                'projectId': 'first-project' if i < 75 else 'synthetic-project',
                'cwd': '/synthetic/project',
            } for i in range(1, 100)]
            result['data'] += [{'id': '00000000-0000-0000-0002-000000000001',
                'name': 'Sin proyecto real', 'source': 'cli', 'status': {'type': 'idle'},
                'updatedAt': 1, 'cwd': '/synthetic/project'}]
    elif method == 'project/list':
        parent = '00000000-0000-0000-0000-000000000001'
        child_id = '00000000-0000-0000-0000-000000000002'
        print(json.dumps({'method': 'turn/started', 'params': {'threadId': parent, 'turn': {'id': 'fixture-turn'}}}), flush=True)
        for call, tool, status in [('spawn', 'spawnAgent', 'completed'), ('wait', 'wait', 'inProgress' if refresh <= 4 else 'completed')]:
            print(json.dumps({'method': 'item/started' if status == 'inProgress' else 'item/completed', 'params': {'threadId': parent, 'turnId': 'fixture-turn', 'item': {'type': 'collabAgentToolCall', 'id': call, 'tool': tool, 'status': status, 'receiverThreadIds': [child_id], 'prompt': 'This synthetic content must not be retained.'}}}), flush=True)
        result = {'data': [{'id': 'synthetic-project', 'name': 'Proyecto sintético', 'position': 20}]}
        if overview:
            result['data'].append({'id': 'first-project', 'name': 'Primero en Codex', 'position': 10})
    elif 'id' in message and 'error' in message:
        continue
    else:
        raise ValueError('Unexpected operation in read-only fixture: ' + str(method))
    print(json.dumps({'id': message['id'], 'result': result}), flush=True)
