#!/usr/bin/env python3
import json
from collections import Counter

files = [
    '/home/cyber/.claude/projects/-home-cyber-Work-reddb-io-reddb/3cefd5bb-e01d-4633-b7b6-ce385d06fa21.jsonl',
    '/home/cyber/.claude/projects/-home-cyber-Work-reddb-io-reddb/d77eaa35-6cc8-41fb-82e6-87b9d02fb9e6.jsonl',
    '/home/cyber/.claude/projects/-home-cyber-Work-reddb-io-reddb/bf00cf34-0064-41e7-9498-5b48e7d7ea3f.jsonl',
    '/home/cyber/.claude/projects/-home-cyber-Work-reddb-io-reddb/ecf9728c-29f3-4f90-9ef8-d3c4663d61a2.jsonl',
    '/home/cyber/.claude/projects/-home-cyber-Work-reddb-io-reddb/aa271613-7c91-42ad-9418-766e7bc5f6ff.jsonl',
    '/home/cyber/.claude/projects/-home-cyber-Work-reddb-io-reddb/52bd2d7e-f6ec-49d5-9f1d-5a73af42d494.jsonl',
    '/home/cyber/.claude/projects/-home-cyber-Work-reddb-io-reddb/adba08b6-b1a4-4aa0-8a81-e34589b9eec2.jsonl',
    '/home/cyber/.claude/projects/-home-cyber-Work-reddb-io-reddb/39061ebf-ebb5-4f19-9c38-8f6f326ce8d2.jsonl',
    '/home/cyber/.claude/projects/-home-cyber-Work-reddb-io-reddb/d673d964-ee60-4030-a688-7801d03ca6dc.jsonl',
    '/home/cyber/.claude/projects/-home-cyber-Work-reddb-io-reddb/0e26e8a1-6068-4e4c-a3fb-49839e9d87dc.jsonl',
]

token_counter = Counter()
token_examples = {}
total = 0

for filepath in files:
    with open(filepath) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                obj = json.loads(line)
            except Exception:
                continue
            msg = obj.get('message', {})
            if not isinstance(msg, dict):
                continue
            content = msg.get('content', [])
            if not isinstance(content, list):
                continue
            for item in content:
                if not isinstance(item, dict):
                    continue
                if item.get('type') == 'tool_use' and item.get('name') == 'Bash':
                    cmd = item.get('input', {}).get('command', '')
                    if not cmd:
                        continue
                    total += 1
                    stripped = cmd.strip()
                    parts = stripped.split()
                    if not parts:
                        continue
                    # Skip env var assignments like KEY=val at start
                    idx = 0
                    while idx < len(parts) and '=' in parts[idx] and not parts[idx].startswith('-'):
                        idx += 1
                    first = parts[idx] if idx < len(parts) else parts[0]
                    token_counter[first] += 1
                    if first not in token_examples:
                        token_examples[first] = stripped

print(f'Total Bash calls: {total}')
print(f'Unique leading tokens: {len(token_counter)}')
print()
print(f'{"Rank":<5} {"Count":<6} {"Token":<25} Example')
print('-' * 120)
for rank, (tok, cnt) in enumerate(token_counter.most_common(30), 1):
    ex = token_examples[tok]
    if len(ex) > 80:
        ex = ex[:77] + '...'
    print(f'{rank:<5} {cnt:<6} {tok:<25} {ex}')
