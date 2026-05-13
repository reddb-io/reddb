#!/usr/bin/env python3
import json
import sys


APP_QUERIES = {
    "pgwire360-psycopg": [
        "INSERT INTO psy_items",
        "SELECT name FROM psy_items",
        "SEARCH SIMILAR",
    ],
    "pgwire360-pgx": [
        "INSERT INTO pgx_items",
        "SELECT name FROM pgx_items",
        "SEARCH SIMILAR",
    ],
    "pgwire360-jdbc": [
        "INSERT INTO jdbc_items",
        "SELECT name FROM jdbc_items",
        "SEARCH SIMILAR",
    ],
}


def main(path):
    events = [json.loads(line) for line in open(path, encoding="utf-8") if line.strip()]
    by_app = {
        app: [event for event in events if event.get("app") == app] for app in APP_QUERIES
    }
    for app, app_events in by_app.items():
        tags = [event.get("tag") for event in app_events]
        if "P" not in tags or "B" not in tags or "E" not in tags:
            raise SystemExit(f"{app}: expected Parse/Bind/Execute frames, saw {tags}")
        parsed_queries = "\n".join(event.get("query", "") for event in app_events if event.get("tag") == "P")
        for query_fragment in APP_QUERIES[app]:
            if query_fragment not in parsed_queries:
                raise SystemExit(f"{app}: missing extended Parse for {query_fragment}")
        simple_queries = "\n".join(event.get("query", "") for event in app_events if event.get("tag") == "Q")
        for query_fragment in APP_QUERIES[app]:
            if query_fragment in simple_queries:
                raise SystemExit(f"{app}: {query_fragment} fell back to simple Query")
    print("pgwire client frame audit passed")


if __name__ == "__main__":
    main(sys.argv[1])
