#!/usr/bin/env python3
"""Render Stage 4 tables from one comparison result, never estimate missing data."""
import argparse
import json
from pathlib import Path
from statistics import median


def show_median(values):
    if not values or any(value is None for value in values):
        return 'unknown'
    value = median(values)
    return str(int(value)) if int(value) == value else str(value)


def render(result):
    if result['status'] != 'complete' or result.get('smoke'):
        raise ValueError('Only a complete non-smoke comparison can be published')
    lines = ['| Agent | N | Total per conversation, median bytes | First, median bytes | '
             'Last, median bytes | Storage bytes | Files |',
             '| --- | ---: | ---: | ---: | ---: | ---: | ---: |']
    for agent in result['agents']:
        if agent['status'] != 'complete':
            raise ValueError(f"Incomplete agent: {agent['agent']}")
        for case in agent['cases']:
            conversations = case['traffic']['conversations']
            if len(conversations) != case['n']:
                raise ValueError('Missing conversation observation')
            columns = [show_median([row.get(key) for row in conversations])
                       for key in ('total_bytes', 'first_bytes', 'last_bytes')]
            stored = case['storage']
            lines.append('| ' + ' | '.join([agent['agent'], str(case['n']), *columns,
                                             str(stored['bytes']), str(stored['files'])]) + ' |')
    return '\n'.join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('result', type=Path)
    args = parser.parse_args()
    print(render(json.loads(args.result.read_text())))


if __name__ == '__main__':
    main()
